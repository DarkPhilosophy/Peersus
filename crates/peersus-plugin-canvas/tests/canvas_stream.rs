#![cfg(all(unix, not(target_os = "android")))]

use peersus_api::canvas::{
    CANVAS_DATA_KIND, FRAME_PREFIX_BYTES, FrameHeader, MAX_FRAME_HEADER_BYTES, PRESENT_INVALID,
    display as dl,
};
use peersus_plugin::{
    Plugin, Service,
    data::{Right, canvas},
    hub::Hub,
    module::ModuleLauncher,
    transport,
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;

fn list(seq: u64, width: u32, color: [u8; 4]) -> Vec<u8> {
    let mut body = b"PSD1\x01\x03map".to_vec();
    body.extend(1i32.to_le_bytes()); // first data_open handle
    body.extend(seq.to_le_bytes());
    body.extend(42u64.to_le_bytes()); // settled
    body.extend(width.to_le_bytes());
    body.extend(2u32.to_le_bytes());
    body.extend(1000u32.to_le_bytes());
    body.extend([0; dl::PALETTE_BYTES]);
    body.extend([dl::CLEAR, 0]);
    body.extend(color);
    body.push(dl::END);
    body
}

fn escaped(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("\\{byte:02x}")).collect()
}

async fn run_stream(memory64: bool) {
    let directory = tempfile::tempdir().unwrap();
    let module = directory.path().join("guest.wasm");
    let helper = Path::new(env!("CARGO_BIN_EXE_peersus-wasm-canvas-host")).to_owned();
    let ptr = if memory64 { "i64" } else { "i32" };
    let memory = if memory64 { "i64 1" } else { "1" };
    let first = list(1, 3, [20, 40, 60, 255]);
    let bad = list(2, 4097, [0; 4]);
    let second = list(3, 3, [40, 0, 20, 128]);
    let wat = format!(
        r#"(module
            (import "peersus_v1" "receive" (func $receive (param {ptr} {ptr}) (result i32)))
            (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
            (import "peersus_v1" "canvas_present" (func $present (param {ptr} {ptr}) (result i32)))
            (memory (export "memory") {memory})
            (data ({ptr}.const 0) "{}")
            (data ({ptr}.const 256) "{}")
            (data ({ptr}.const 512) "{}")
            (func (export "peersus_main") (result i32)
                i32.const {CANVAS_DATA_KIND} i64.const 123 call $open i32.const 1 i32.ne
                if i32.const 1 return end
                {ptr}.const 0 {ptr}.const {} call $present
                if i32.const 2 return end
                {ptr}.const 0 {ptr}.const {} call $present i32.const {PRESENT_INVALID} i32.ne
                if i32.const 3 return end
                {ptr}.const 256 {ptr}.const {} call $present i32.const {PRESENT_INVALID} i32.ne
                if i32.const 4 return end
                {ptr}.const 65535 {ptr}.const 2 call $present i32.const {PRESENT_INVALID} i32.ne
                if i32.const 5 return end
                {ptr}.const 512 {ptr}.const {} call $present
                if i32.const 6 return end
                {ptr}.const 1024 {ptr}.const 4096 call $receive drop
                i32.const 0))"#,
        escaped(&first),
        escaped(&bad),
        escaped(&second),
        first.len(),
        first.len(),
        bad.len(),
        second.len(),
    );
    std::fs::write(&module, wat::parse_str(wat).unwrap()).unwrap();
    let (handoff, mut incoming) = tokio::sync::mpsc::channel(1);
    let service = Service::start_with_launcher(
        Hub::default(),
        Plugin {
            id: "canvas-fixture".into(),
            manifest: peersus_api::manifest::Manifest::new(
                peersus_api::manifest::Execution::Wasm {
                    abi: peersus_api::manifest::EXECUTION_ABI_VERSION,
                },
            ),
            grants: Default::default(),
            program: Some(module.clone()),
            arguments: Vec::new(),
            directory: directory.path().to_owned(),
            policy: Default::default(),
            handoff: Some(handoff),
        },
        Arc::new(ModuleLauncher::new(helper, module).unwrap()),
    )
    .unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut accepted = incoming.recv().await.unwrap();
        assert!(matches!(accepted.right, Right::Claim(123)));
        let listener = transport::Listener::bind(&directory.path().join("window")).unwrap();
        let token = [7; canvas::TOKEN_BYTES];
        let redirect =
            canvas::Redirect::new(listener.endpoint().to_str().unwrap().into(), token).unwrap();
        redirect.write(&mut accepted.stream).await.unwrap();
        drop(accepted);
        let mut direct = listener.accept().await.unwrap();
        canvas::authenticate(&mut direct, &token).await.unwrap();
        for (seq, color) in [(1, [20, 40, 60, 255]), (3, [20, 0, 10, 128])] {
            let mut wire = [0; MAX_FRAME_HEADER_BYTES];
            direct
                .read_exact(&mut wire[..FRAME_PREFIX_BYTES])
                .await
                .unwrap();
            let len =
                FrameHeader::encoded_len(wire[..FRAME_PREFIX_BYTES].try_into().unwrap()).unwrap();
            direct
                .read_exact(&mut wire[FRAME_PREFIX_BYTES..len])
                .await
                .unwrap();
            let header = FrameHeader::decode(&wire[..len]).unwrap();
            assert_eq!((header.id, header.seq, header.settled), ("map", seq, 42));
            assert_eq!(
                (
                    header.layout.width(),
                    header.layout.height(),
                    header.layout.stride()
                ),
                (3, 2, 12)
            );
            let mut pixels = vec![0; header.layout.byte_len()];
            direct.read_exact(&mut pixels).await.unwrap();
            assert_eq!(pixels, color.repeat(6));
        }
        assert!(
            incoming.try_recv().is_err(),
            "both frames must use the direct window connection"
        );
        service.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm32_presents_two_validated_frames_on_one_authenticated_claim() {
    run_stream(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm64_presents_two_validated_frames_on_one_authenticated_claim() {
    run_stream(true).await;
}
