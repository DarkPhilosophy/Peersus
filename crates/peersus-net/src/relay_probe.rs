//! How quickly a relay answers: the time to open a connection to it. Nothing is sent over the connection and it is closed at once,
//! so no credential, identity or traffic crosses; it only tells whether the relay is reachable from here and how far it is.
use std::time::{Duration, Instant};

use peersus_core::relay::RelayTarget;
use tokio::net::TcpStream;

/// One relay may take this long before it counts as not answering.
const TIMEOUT: Duration = Duration::from_secs(4);
/// How many connection attempts are open at once, however long the list: a long list is measured in waves, never all at once.
static IN_FLIGHT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);

/// The relays of the public pool, by address.
fn public_pool() -> Vec<String> {
    iroh::RelayMode::Default
        .relay_map()
        .urls::<Vec<_>>()
        .into_iter()
        .map(|url| url.to_string())
        .collect()
}

/// The milliseconds the fastest address of `target` took to accept a connection, or `None` when none answered in time. The public
/// pool answers with its nearest relay.
pub async fn probe(target: &RelayTarget) -> Option<u32> {
    let addresses = match target.url() {
        Some(url) => vec![url.to_owned()],
        None => public_pool(),
    };
    let attempts = addresses.iter().map(|address| connect_time(address));
    futures_util::future::join_all(attempts)
        .await
        .into_iter()
        .flatten()
        .min()
}

async fn connect_time(address: &str) -> Option<u32> {
    let (host, port) = host_and_port(address)?;
    let _permit = IN_FLIGHT.acquire().await.ok()?;
    let started = Instant::now();
    tokio::time::timeout(TIMEOUT, TcpStream::connect((host.as_str(), port)))
        .await
        .ok()?
        .ok()?;
    Some(
        u32::try_from(started.elapsed().as_millis())
            .unwrap_or(u32::MAX)
            .max(1),
    )
}

/// The host and port to connect to for an `https://` (or `http://`) address.
fn host_and_port(address: &str) -> Option<(String, u16)> {
    let (scheme, rest) = address.split_once("://")?;
    let default_port = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        let port = after
            .strip_prefix(':')
            .map_or(Some(default_port), |port| port.parse().ok())?;
        return Some((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), default_port)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_gives_the_host_and_port_that_are_dialled() {
        assert_eq!(
            host_and_port("https://peersus.axxa.dev"),
            Some(("peersus.axxa.dev".into(), 443))
        );
        assert_eq!(
            host_and_port("https://relay.example.org:8443/x"),
            Some(("relay.example.org".into(), 8443))
        );
        assert_eq!(
            host_and_port("http://10.0.0.2"),
            Some(("10.0.0.2".into(), 80))
        );
        assert_eq!(
            host_and_port("https://[2001:db8::1]:444"),
            Some(("2001:db8::1".into(), 444))
        );
        assert_eq!(
            host_and_port("https://user@host.example"),
            Some(("host.example".into(), 443))
        );
        assert_eq!(host_and_port("ftp://host"), None);
        assert_eq!(host_and_port("host"), None);
    }

    #[tokio::test]
    async fn a_listening_relay_answers_with_a_time_and_a_closed_one_does_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let open = RelayTarget::Custom(format!("http://127.0.0.1:{port}"));
        assert!(probe(&open).await.is_some());
        drop(listener);
        let closed = RelayTarget::Custom(format!("http://127.0.0.1:{port}"));
        assert_eq!(probe(&closed).await, None);
    }
}
