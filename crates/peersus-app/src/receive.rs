//! Carrying what a peer sent to the plugin that was asked to take it.
//!
//! The pump does the copying and knows nothing of why. This decides what the copy is
//! allowed to be and what the user is told when it does not finish, because those are
//! application decisions: how much a peer may send a plugin, and in which words a stalled or
//! oversized transfer is reported. The wording is fixed and says nothing about the content,
//! the peer's name or the plugin's own output.
use peersus_plugin_host::pump::{self, Outcome};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// Most a peer may send to a plugin in one go. A plugin that needs to move more than this
/// has to be given a larger allowance deliberately; the default is not "unlimited".
pub const DEFAULT_LIMIT: u64 = 64 * 1024 * 1024;

/// How the transfer ended, as far as the application cares.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Every byte reached the plugin. The count is what was carried.
    Done(u64),
    /// It stopped, for a reason the user may be told, in fixed words.
    Failed(&'static str),
}

/// Copy everything `from_peer` sends into `to_plugin`, up to `limit` bytes.
pub async fn deliver(
    from_peer: impl AsyncRead + Unpin,
    to_plugin: impl AsyncWrite + Unpin,
    limit: u64,
    idle: Duration,
) -> Delivery {
    match pump::copy(from_peer, to_plugin, limit, idle).await {
        Outcome::Complete(carried) => Delivery::Done(carried),
        Outcome::TooLarge => Delivery::Failed("A device sent more than a plugin may receive"),
        Outcome::Stalled => Delivery::Failed("A transfer to a plugin stopped making progress"),
        Outcome::Broken => Delivery::Failed("A transfer to a plugin was interrupted"),
    }
}

/// Copy everything `from_plugin` writes to `to_device`, up to `limit` bytes.
///
/// The same copy as [`deliver`] the other way round, with its own words: here it is the
/// plugin that can send more than agreed or go quiet, and a message that blamed a device
/// would tell the user something false about who to distrust.
pub async fn send_out(
    from_plugin: impl AsyncRead + Unpin,
    to_device: impl AsyncWrite + Unpin,
    limit: u64,
    idle: Duration,
) -> Delivery {
    match pump::copy(from_plugin, to_device, limit, idle).await {
        Outcome::Complete(carried) => Delivery::Done(carried),
        Outcome::TooLarge => Delivery::Failed("A plugin tried to send more than it may"),
        Outcome::Stalled => Delivery::Failed("A transfer to a device stopped making progress"),
        Outcome::Broken => Delivery::Failed("A transfer to a device was interrupted"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, duplex};

    const IDLE: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn everything_the_peer_sent_reaches_the_plugin_in_order() {
        let payload: Vec<u8> = (0..300_000u32).map(|n| (n % 251) as u8).collect();
        let (plugin_end, mut plugin_reads) = duplex(1 << 20);
        let carried = deliver(
            std::io::Cursor::new(payload.clone()),
            plugin_end,
            DEFAULT_LIMIT,
            IDLE,
        )
        .await;
        assert_eq!(carried, Delivery::Done(payload.len() as u64));
        let mut received = Vec::new();
        plugin_reads.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn a_peer_that_sends_more_than_allowed_is_cut_off_and_the_user_is_told_plainly() {
        let (plugin_end, mut plugin_reads) = duplex(1 << 20);
        let outcome = deliver(
            std::io::Cursor::new(vec![9u8; 100_000]),
            plugin_end,
            50_000,
            IDLE,
        )
        .await;
        assert_eq!(
            outcome,
            Delivery::Failed("A device sent more than a plugin may receive")
        );
        let mut received = Vec::new();
        plugin_reads.read_to_end(&mut received).await.unwrap();
        assert!(
            received.len() <= 50_000,
            "the plugin was handed {} bytes though the limit was 50000",
            received.len()
        );
    }

    #[tokio::test]
    async fn a_plugin_that_closed_its_end_is_reported_as_interrupted_not_as_success() {
        let (plugin_end, plugin_reads) = duplex(64);
        drop(plugin_reads);
        let outcome = deliver(
            std::io::Cursor::new(vec![1u8; 10_000]),
            plugin_end,
            DEFAULT_LIMIT,
            IDLE,
        )
        .await;
        assert_eq!(
            outcome,
            Delivery::Failed("A transfer to a plugin was interrupted")
        );
    }

    /// A source that produces `remaining` bytes of zeros without ever holding them.
    struct Zeros {
        remaining: u64,
    }
    impl AsyncRead for Zeros {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let take = (buf.remaining() as u64).min(self.remaining) as usize;
            buf.put_slice(&vec![0u8; take]);
            self.remaining -= take as u64;
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A destination that discards everything and remembers only how much it was given.
    struct Counts(std::sync::Arc<std::sync::atomic::AtomicU64>);
    impl AsyncWrite for Counts {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0
                .fetch_add(buf.len() as u64, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_default_allowance_admits_exactly_that_much_and_not_a_byte_more() {
        let given = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let exact = deliver(
            Zeros {
                remaining: DEFAULT_LIMIT,
            },
            Counts(given.clone()),
            DEFAULT_LIMIT,
            IDLE,
        )
        .await;
        assert_eq!(exact, Delivery::Done(DEFAULT_LIMIT));
        assert_eq!(
            given.load(std::sync::atomic::Ordering::SeqCst),
            DEFAULT_LIMIT
        );

        let given = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let over = deliver(
            Zeros {
                remaining: DEFAULT_LIMIT + 1,
            },
            Counts(given.clone()),
            DEFAULT_LIMIT,
            IDLE,
        )
        .await;
        assert_eq!(
            over,
            Delivery::Failed("A device sent more than a plugin may receive")
        );
        assert!(
            given.load(std::sync::atomic::Ordering::SeqCst) <= DEFAULT_LIMIT,
            "a byte beyond the allowance reached the plugin"
        );
    }

    #[tokio::test]
    async fn everything_a_plugin_sends_reaches_the_device_in_order() {
        let payload: Vec<u8> = (0..300_000u32).map(|n| (n % 251) as u8).collect();
        let (device_end, mut device_reads) = duplex(1 << 20);
        let carried = send_out(
            std::io::Cursor::new(payload.clone()),
            device_end,
            DEFAULT_LIMIT,
            IDLE,
        )
        .await;
        assert_eq!(carried, Delivery::Done(payload.len() as u64));
        let mut received = Vec::new();
        device_reads.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn a_plugin_that_sends_more_than_allowed_is_cut_off_before_the_excess_leaves() {
        let (device_end, mut device_reads) = duplex(1 << 20);
        let result = send_out(std::io::Cursor::new(vec![7u8; 1000]), device_end, 600, IDLE).await;
        assert!(matches!(result, Delivery::Failed(_)));
        let mut received = Vec::new();
        device_reads.read_to_end(&mut received).await.unwrap();
        assert!(
            received.len() <= 600,
            "{} bytes left past the limit of 600",
            received.len()
        );
    }

    #[tokio::test]
    async fn a_device_that_went_away_is_reported_as_interrupted_not_as_success() {
        let (device_end, device_reads) = duplex(16);
        drop(device_reads);
        let result = send_out(
            std::io::Cursor::new(vec![1u8; 4096]),
            device_end,
            1 << 20,
            IDLE,
        )
        .await;
        assert!(matches!(result, Delivery::Failed(_)), "{result:?}");
    }

    #[tokio::test]
    async fn what_the_user_is_told_about_a_send_never_blames_a_device_for_the_plugin() {
        // The same three ways to fail as when receiving, with the roles the other way round:
        // here the plugin is the one that can send too much, and the words must say so.
        let (device_end, _keep) = duplex(1 << 20);
        let too_large =
            send_out(std::io::Cursor::new(vec![7u8; 1000]), device_end, 600, IDLE).await;
        let (plugin_end, _keep_plugin) = duplex(1 << 20);
        let received_too_large =
            deliver(std::io::Cursor::new(vec![7u8; 1000]), plugin_end, 600, IDLE).await;
        assert_ne!(
            too_large, received_too_large,
            "sending and receiving must not share the words for the same failure"
        );
        let Delivery::Failed(text) = too_large else {
            panic!("expected a failure")
        };
        assert!(
            text.to_lowercase().contains("plugin"),
            "the user must be told it was the plugin that sent too much: {text}"
        );
    }
}
