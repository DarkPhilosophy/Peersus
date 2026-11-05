//! Moves bulk bytes between a remote peer and a plugin process, on the host's terms.
//!
//! A plugin never touches the network. When a peer opens a stream for a plugin, the host
//! gives the plugin a one-off local connection instead and copies bytes across, so the
//! plugin sees a plain byte stream and the host stays the only holder of the peer's
//! authenticated channel. The copy is bounded in size and in idle time, so a peer or a
//! plugin that stalls, or one that sends more than was agreed, ends the transfer rather
//! than tying up the application.
//!
//! The local side is bound to one accepted call: it must present the token minted for
//! that call, so a plugin cannot open a data connection that no manifest and no grant
//! allowed.
use std::{io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// How long either side may make no progress before the copy is abandoned.
pub const IDLE_LIMIT: Duration = Duration::from_secs(30);

/// Why a copy ended. Distinguishes a clean finish from the ways it can be cut short, so
/// the caller can report the right thing and decide whether anything must be cleaned up.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every byte arrived and the source ended cleanly.
    Complete(u64),
    /// More bytes arrived than the agreed maximum. The excess was not forwarded.
    TooLarge,
    /// Neither side made progress within the idle limit.
    Stalled,
    /// One side failed or closed early.
    Broken,
}

/// Copy `from` to `to` until `from` ends, refusing to forward more than `maximum` bytes.
///
/// The limit is enforced before a byte is written on, not after, so an oversized source
/// can never push more than `maximum` to the destination. Each read and write has its own
/// idle deadline, so a slow but progressing transfer of any total length is allowed while
/// a silent one is not.
pub async fn copy(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    maximum: u64,
    idle: Duration,
) -> Outcome {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = match tokio::time::timeout(idle, from.read(&mut buffer)).await {
            Err(_) => return Outcome::Stalled,
            Ok(Err(_)) => return Outcome::Broken,
            Ok(Ok(0)) => break,
            Ok(Ok(read)) => read,
        };
        // `read` is at most the buffer length, so this cannot overflow `u64`.
        let next = total + read as u64;
        if next > maximum {
            // Forward nothing of this chunk: the agreed size has already been reached.
            return Outcome::TooLarge;
        }
        match tokio::time::timeout(idle, to.write_all(&buffer[..read])).await {
            Err(_) => return Outcome::Stalled,
            Ok(Err(_)) => return Outcome::Broken,
            Ok(Ok(())) => {}
        }
        total = next;
    }
    match tokio::time::timeout(idle, to.shutdown()).await {
        Err(_) => Outcome::Stalled,
        Ok(Err(error)) if error.kind() != io::ErrorKind::NotConnected => Outcome::Broken,
        Ok(_) => Outcome::Complete(total),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, empty};

    /// A source that yields `data` and then never ends, to model a stalled sender.
    struct Stalls {
        data: Vec<u8>,
    }
    impl AsyncRead for Stalls {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if self.data.is_empty() {
                return std::task::Poll::Pending;
            }
            let take = self.data.len().min(buf.remaining());
            let chunk: Vec<u8> = self.data.drain(..take).collect();
            buf.put_slice(&chunk);
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A destination that takes every byte but never confirms it has closed, like a
    /// plugin that stopped responding while holding its end of the connection open.
    struct NeverCloses;
    impl AsyncWrite for NeverCloses {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_destination_that_never_confirms_closing_is_abandoned_exactly_at_the_idle_limit() {
        let started = tokio::time::Instant::now();
        let outcome = copy(
            std::io::Cursor::new(vec![5u8; 1000]),
            NeverCloses,
            1 << 20,
            Duration::from_secs(5),
        )
        .await;
        // Every byte was delivered, yet the transfer is not reported as complete: the
        // receiving side never acknowledged the end, so it cannot be trusted to have it.
        assert_eq!(outcome, Outcome::Stalled);
        assert_eq!(started.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test]
    async fn copies_exactly_what_was_sent_and_reports_the_length() {
        let payload: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        let (mut sink, drain) = duplex(1 << 20);
        let source = std::io::Cursor::new(payload.clone());
        let outcome = copy(source, &mut sink, 1 << 20, IDLE_LIMIT).await;
        assert_eq!(outcome, Outcome::Complete(payload.len() as u64));
        drop(sink);
        let mut received = Vec::new();
        let mut drain = drain;
        drain.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, payload, "every byte, in order, nothing added");
    }

    #[tokio::test]
    async fn an_oversized_source_is_cut_off_without_forwarding_the_excess() {
        let (mut sink, mut drain) = duplex(1 << 20);
        let source = std::io::Cursor::new(vec![7u8; 100_000]);
        let outcome = copy(source, &mut sink, 70_000, IDLE_LIMIT).await;
        assert_eq!(outcome, Outcome::TooLarge);
        drop(sink);
        let mut received = Vec::new();
        drain.read_to_end(&mut received).await.unwrap();
        // The limit is enforced before writing, so strictly no more than the maximum
        // can ever have been handed on, whatever the chunking was.
        assert!(
            received.len() <= 70_000,
            "forwarded {} bytes",
            received.len()
        );
    }

    #[tokio::test]
    async fn a_source_of_exactly_the_maximum_is_complete_not_too_large() {
        let (mut sink, _drain) = duplex(1 << 20);
        let outcome = copy(
            std::io::Cursor::new(vec![1u8; 4096]),
            &mut sink,
            4096,
            IDLE_LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Complete(4096));
        let (mut sink, _drain) = duplex(1 << 20);
        let outcome = copy(
            std::io::Cursor::new(vec![1u8; 4097]),
            &mut sink,
            4096,
            IDLE_LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::TooLarge);
    }

    #[tokio::test]
    async fn an_empty_source_completes_with_zero() {
        let (mut sink, _drain) = duplex(64);
        assert_eq!(
            copy(empty(), &mut sink, 10, IDLE_LIMIT).await,
            Outcome::Complete(0)
        );
    }

    /// Run `copy` under a paused clock and report the outcome together with how much
    /// virtual time passed. With the clock paused, the runtime advances time by itself to
    /// the nearest timer once everything is idle, so the elapsed virtual time is exactly
    /// the deadline that fired. That is what lets a test tell a 5 second limit from a one
    /// year limit, which "it eventually returned Stalled" cannot.
    async fn timed(
        from: impl AsyncRead + Unpin,
        to: impl AsyncWrite + Unpin,
        idle: Duration,
    ) -> (Outcome, Duration) {
        let started = tokio::time::Instant::now();
        let outcome = copy(from, to, 1 << 20, idle).await;
        (outcome, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_sender_is_abandoned_exactly_at_the_idle_limit() {
        let (sink, _drain) = duplex(1 << 20);
        let (outcome, elapsed) = timed(
            Stalls {
                data: vec![9u8; 10],
            },
            sink,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(outcome, Outcome::Stalled);
        assert_eq!(
            elapsed,
            Duration::from_secs(5),
            "must give up at the limit, not later"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_receiver_that_stops_reading_is_abandoned_exactly_at_the_idle_limit() {
        // A tiny buffer that nobody drains: the second write can never complete.
        let (sink, _never_read) = duplex(8);
        let (outcome, elapsed) = timed(
            std::io::Cursor::new(vec![3u8; 4096]),
            sink,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(outcome, Outcome::Stalled);
        assert_eq!(
            elapsed,
            Duration::from_secs(5),
            "must give up at the limit, not later"
        );
    }

    #[tokio::test]
    async fn a_closed_destination_is_reported_as_broken() {
        let (mut sink, drain) = duplex(64);
        drop(drain);
        let outcome = copy(
            std::io::Cursor::new(vec![1u8; 1000]),
            &mut sink,
            1 << 20,
            IDLE_LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Broken);
    }
}
