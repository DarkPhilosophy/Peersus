//! Downloads for the extension store: one HTTPS GET, bounded, from one fixed base, and nothing else.
//!
//! This lives apart from the application owner on purpose. A TLS and HTTP stack is a large attack
//! surface; whatever it returns is only BYTES, and the owner verifies them against a compiled-in key
//! before using any of them, so a flaw here can at worst produce bytes that are then refused.
//!
//! What is fixed: HTTPS only, no redirect, no proxy, no retry, connect and total timeouts, and a hard
//! cap on the body counted as it streams (a `Content-Length` is never trusted). What a caller can name
//! is only a plain file name: it is joined under the base and can never leave it.
#![forbid(unsafe_code)]
use futures_util::StreamExt;
use reqwest::{Client, redirect::Policy};
use std::time::Duration;
use url::Url;

/// Where the catalog and the approved archives are published. Fixed at compile time.
pub const STORE_BASE: &str = "https://darkphilosophy.github.io/Peersus/store/";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("the file name is not a plain name")]
    Name,
    #[error("the address is not allowed")]
    Address,
    #[error("the store could not be reached")]
    Network,
    #[error("the store answered with an error")]
    Status(u16),
    #[error("what the store sent is larger than allowed")]
    TooLarge,
}

pub struct Store {
    client: Client,
    base: Url,
}

impl Store {
    pub fn production() -> Result<Self, Error> {
        Self::with_base(STORE_BASE, true)
    }

    /// `https_only` is false only so a test can talk to a loopback server; production never calls
    /// this with `false`.
    pub fn with_base(base: &str, https_only: bool) -> Result<Self, Error> {
        let base = Url::parse(base).map_err(|_| Error::Address)?;
        if !base.path().ends_with('/') || base.query().is_some() || base.fragment().is_some() || !base.username().is_empty() {
            return Err(Error::Address);
        }
        if https_only && base.scheme() != "https" {
            return Err(Error::Address);
        }
        let client = Client::builder()
            .https_only(https_only)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self { client, base })
    }

    /// `name` must be one plain file name: ASCII letters, digits, `.`, `_`, `-`, not starting with a dot.
    pub fn address(&self, name: &str) -> Result<Url, Error> {
        let plain = !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('.')
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !plain {
            return Err(Error::Name);
        }
        let url = self.base.join(name).map_err(|_| Error::Address)?;
        // Joined under the base and still on its host and directory: never anywhere else.
        if url.origin() != self.base.origin() || !url.path().starts_with(self.base.path()) {
            return Err(Error::Address);
        }
        Ok(url)
    }

    /// The body of `name`, at most `limit` bytes. Counted as it arrives; the declared length is ignored.
    pub async fn get(&self, name: &str, limit: usize) -> Result<Vec<u8>, Error> {
        let response = self.client.get(self.address(name)?).send().await.map_err(|_| Error::Network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Status(status.as_u16()));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::Network)?;
            if body.len() + chunk.len() > limit {
                return Err(Error::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One-shot loopback server answering every request with `reply`.
    async fn serve(reply: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 1024];
                    let _ = socket.read(&mut request).await;
                    let _ = socket.write_all(&reply).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{address}/store/")
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
        reply.extend_from_slice(body);
        reply
    }

    #[test]
    fn production_is_https_only_and_names_are_plain() {
        assert!(Store::production().is_ok());
        assert_eq!(Store::with_base("http://example.org/store/", true).err(), Some(Error::Address));
        assert_eq!(Store::with_base("https://example.org/store", true).err(), Some(Error::Address), "needs a directory");
        assert_eq!(Store::with_base("https://user@example.org/store/", true).err(), Some(Error::Address));
        let store = Store::production().unwrap();
        for bad in ["", ".hidden", "../x", "a/b", "a\\b", "a?b", "a#b", "a b", "%2e%2e", "é", &"x".repeat(129)] {
            assert_eq!(store.address(bad), Err(Error::Name), "{bad:?}");
        }
        assert_eq!(store.address("catalog.json").unwrap().as_str(), format!("{STORE_BASE}catalog.json"));
    }

    #[tokio::test]
    async fn a_body_within_the_limit_is_returned() {
        let base = serve(ok(b"hello")).await;
        let store = Store::with_base(&base, false).unwrap();
        assert_eq!(store.get("catalog.json", 5).await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_refused_whatever_it_claims() {
        let store = Store::with_base(&serve(ok(&[0u8; 100])).await, false).unwrap();
        assert_eq!(store.get("a.bin", 99).await, Err(Error::TooLarge));
        // A chunked reply with no length is counted as it streams, too.
        let mut chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for _ in 0..4 {
            chunked.extend_from_slice(b"19\r\n");
            chunked.extend_from_slice(&[7u8; 25]);
            chunked.extend_from_slice(b"\r\n");
        }
        chunked.extend_from_slice(b"0\r\n\r\n");
        let store = Store::with_base(&serve(chunked).await, false).unwrap();
        assert_eq!(store.get("a.bin", 99).await, Err(Error::TooLarge));
    }

    #[tokio::test]
    async fn an_error_status_and_a_redirect_are_failures_not_followed() {
        let missing = serve(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        assert_eq!(Store::with_base(&missing, false).unwrap().get("a", 10).await, Err(Error::Status(404)));
        let moved = serve(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        assert_eq!(Store::with_base(&moved, false).unwrap().get("a", 10).await, Err(Error::Status(302)));
    }
}
