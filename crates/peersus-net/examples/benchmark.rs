//! Reproducible release-mode measurements using the real transfer and history paths.
//! Run: cargo run --release --locked -p peersus-net --example benchmark -- [iterations] [MiB]
//! Loopback throughput excludes consent, discovery and relay latency; it is not a device benchmark.
use peersus_core::{NetworkSettings, model::{PeerId, Transfer, TransferState}};
use peersus_net::{history::History, transfer::{self, Source}, transport};
use std::{io::Write, time::Instant};

fn record(id: usize) -> Transfer {
    Transfer {
        id: format!("{id:032x}"), peer: PeerId("benchmark".into()), peer_name: None,
        name: format!("fixture-{id}.bin"), folders: vec!["benchmark".into()],
        bytes: 1024, completed_bytes: 1024, incoming: false, state: TransferState::Complete,
        started_at: id as u64, code: None, expires_at: None, saved_as: None,
    }
}

fn sample(values: &[f64]) -> serde_json::Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    serde_json::json!({"samples": values, "median": sorted[sorted.len() / 2],
        "min": sorted[0], "max": sorted[sorted.len() - 1]})
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let iterations: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(5);
    let mib: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(16);
    if !(1..=50).contains(&iterations) || !(1..=256).contains(&mib) || args.next().is_some() {
        return Err("expected iterations 1..50 and MiB 1..256".into());
    }
    // Use an on-disk temporary directory when TMPDIR points to one; setup is not timed.
    let fixture = tempfile::tempdir()?;
    let path = fixture.path().join("payload.bin");
    let block: Vec<u8> = (0..1024 * 1024).map(|n| (n % 251) as u8).collect();
    {
        let mut file = std::fs::File::create(&path)?;
        for _ in 0..mib { file.write_all(&block)?; }
        file.sync_all()?;
    }
    let policy = NetworkSettings { bind: Some(std::net::Ipv4Addr::LOCALHOST.into()), allow_relay: false };
    let binding = Instant::now();
    let a = transport::bind(iroh::SecretKey::generate(), &policy).await?;
    let b = transport::bind(iroh::SecretKey::generate(), &policy).await?;
    let endpoint_pair_ms = binding.elapsed().as_secs_f64() * 1000.;
    let connecting = Instant::now();
    let (sender, receiver) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
        b.accept().await.ok_or("no incoming connection")?.await.map_err(|_| "connection refused")
    });
    let (sender, receiver) = (sender?, receiver?);
    let connect_ms = connecting.elapsed().as_secs_f64() * 1000.;
    let mut transfers = Vec::new();
    for _ in 0..iterations {
        let destination = tempfile::tempdir_in(fixture.path())?;
        let source = Source::open(&path).await?;
        let offer = source.offer.clone();
        let started = Instant::now();
        let (sent, received) = tokio::join!(async {
            let mut stream = sender.open_uni().await.map_err(|_| "open stream failed")?;
            source.send(&mut stream, |_| {}).await.map_err(|_| "send failed")
        }, async {
            let mut stream = receiver.accept_uni().await.map_err(|_| "accept stream failed")?;
            transfer::receive(&mut stream, &offer, destination.path(), |_| {}).await.map_err(|_| "receive failed")
        });
        sent?;
        let received = received?;
        let seconds = started.elapsed().as_secs_f64();
        // Integrity is already verified in receive; independently verify fixture bytes outside the timed region.
        let bytes = std::fs::read(received)?;
        if bytes.len() != mib * block.len() || !bytes.chunks(block.len()).all(|chunk| chunk == block) {
            return Err("received bytes differ from fixture".into());
        }
        transfers.push(mib as f64 / seconds);
    }
    sender.close(0u32.into(), b"benchmark complete");
    receiver.close(0u32.into(), b"benchmark complete");
    a.close().await;
    b.close().await;
    let mut histories = Vec::new();
    for count in [1, 100, 1000] {
        let directory = tempfile::tempdir_in(fixture.path())?;
        let mut history = History::open(directory.path().join("history.json"))?;
        for id in 0..count { history.record(record(id))?; }
        let mut milliseconds = Vec::new();
        for iteration in 0..iterations {
            let mut entry = record(count - 1);
            entry.completed_bytes = iteration as u64;
            let started = Instant::now();
            history.record(entry)?;
            milliseconds.push(started.elapsed().as_secs_f64() * 1000.);
        }
        histories.push(serde_json::json!({"records":count,"update_ms":sample(&milliseconds)}));
    }
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "schema":1, "build":if cfg!(debug_assertions) { "debug" } else { "release" },
        "iterations":iterations,"payload_mib":mib,"endpoint_pair_ms":endpoint_pair_ms,
        "connect_ms":connect_ms,"encrypted_loopback_mib_s":sample(&transfers),"history":histories,
        "scope":"local QUIC + SHA-256 + fsync; warm file cache; no relay/discovery/consent"
    }))?);
    Ok(())
}
