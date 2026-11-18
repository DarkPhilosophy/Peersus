#![cfg(unix)]

use peersus_plugin_runtime::{Transport, socket::SocketTransport};
use std::{io::Write, os::unix::net::UnixStream};

fn transport() -> (SocketTransport, UnixStream) {
    let (control, remote) = UnixStream::pair().unwrap();
    let transport = SocketTransport::from_connected(control, "fixture".into(), || {
        Err(std::io::Error::other("unexpected open"))
    })
    .unwrap();
    (transport, remote)
}

#[test]
fn idle_first_controller_does_not_block_second_controller_or_control() {
    let (mut transport, mut control) = transport();
    let (first, _first_sender) = UnixStream::pair().unwrap();
    let (second, mut second_sender) = UnixStream::pair().unwrap();
    let streams = [(7, &first), (8, &second)];
    assert_eq!(transport.poll(&streams, 0).unwrap(), None);
    second_sender.write_all(b"movement").unwrap();
    assert_eq!(transport.poll(&streams, 100).unwrap(), Some(8));
    // Leave the second stream readable: a busy peer must not starve control.
    control.write_all(b"control readiness").unwrap();
    let ready: Vec<_> = (0..4)
        .map(|_| transport.poll(&streams, 100).unwrap())
        .collect();
    assert!(
        ready.contains(&Some(0)),
        "busy data starved control: {ready:?}"
    );
    assert!(ready.contains(&Some(8)), "control starved data: {ready:?}");
}

#[test]
fn all_controllers_make_progress_and_eof_is_readable() {
    let (mut transport, _control) = transport();
    let (first, mut first_sender) = UnixStream::pair().unwrap();
    let (second, second_sender) = UnixStream::pair().unwrap();
    first_sender.write_all(b"movement").unwrap();
    drop(second_sender);
    let streams = [(7, &first), (8, &second)];
    let ready: Vec<_> = (0..4)
        .map(|_| transport.poll(&streams, 100).unwrap())
        .collect();
    assert!(
        ready.contains(&Some(7)) && ready.contains(&Some(8)),
        "not fair: {ready:?}"
    );
}

#[test]
fn interpreter_poll_preserves_memory_width_and_rejects_invalid_selections() {
    use peersus_plugin_runtime::{DataRequest, run};
    use std::io::{self, Cursor};
    struct Selected;
    impl Transport for Selected {
        type Stream = Cursor<Vec<u8>>;
        fn receive(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn send(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn open_data(&mut self, _: DataRequest) -> io::Result<Self::Stream> {
            Ok(Cursor::new(Vec::new()))
        }
        fn shutdown_data(&mut self, _: &mut Self::Stream) -> io::Result<()> {
            Ok(())
        }
        fn poll(
            &mut self,
            streams: &[(i32, &Self::Stream)],
            timeout_ms: i32,
        ) -> io::Result<Option<i32>> {
            assert_eq!(timeout_ms, 0);
            assert_eq!(streams.len(), 1);
            Ok(Some(streams[0].0))
        }
    }
    for (width, memory) in [("i32", "1"), ("i64", "i64 1")] {
        for (pointer, count, data, accepted) in [
            (16_u64, 1, "\\01\\00\\00\\00", true),
            (16, 2, "\\01\\00\\00\\00\\01\\00\\00\\00", false),
            (16, 1, "\\02\\00\\00\\00", false),
            (16, 33, "", false),
            (65535, 1, "", false),
        ] {
            let module = wat::parse_str(format!(r#"(module
                (import "peersus_v1" "data_open" (func $open (param i32 i64) (result i32)))
                (import "peersus_v1" "poll" (func $poll (param {width} {width} i32) (result i32)))
                (memory (export "memory") {memory})
                (data ({width}.const 16) "{data}")
                (func (export "peersus_main") (result i32)
                    (drop (call $open (i32.const 0) (i64.const 7)))
                    (if (i32.ne (call $poll ({width}.const {pointer}) ({width}.const {count}) (i32.const 0)) (i32.const 1))
                        (then unreachable))
                    (i32.const 0)))"#)).unwrap();
            assert_eq!(
                run(&module, &mut Selected).is_ok(),
                accepted,
                "{width} pointer={pointer} count={count}"
            );
        }
    }
}
