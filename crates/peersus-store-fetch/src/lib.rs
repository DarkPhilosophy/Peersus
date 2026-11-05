//! Bounded extension-store downloads, isolated from the application owner.
//!
//! This lives apart from the application owner on purpose. A TLS and HTTP stack is a large attack
//! surface; whatever it returns is only BYTES, and the owner verifies them against a compiled-in key
//! before using any of them, so a flaw here can at worst produce bytes that are then refused.
//!
//! Discovery never follows redirects. Repository-bound release assets may follow one HTTPS redirect
//! to GitHub's asset hosts, with no credentials on that hop. Both paths disable proxies and retries,
//! enforce timeouts, and count streamed bytes instead of trusting `Content-Length`.
#![forbid(unsafe_code)]
use futures_util::StreamExt;
use reqwest::{Client, redirect::Policy};
use std::time::Duration;
use url::Url;

/// The source repository is fixed; approval assets are addressed by their repository-bound IDs.
pub const STORE_REPOSITORY: &str = "DarkPhilosophy/Peersus";

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
    assets: Option<(Url, Client)>,
    revision: Option<String>,
}

impl Store {
    pub fn production() -> Result<Self, Error> {
        Self::github_public(STORE_REPOSITORY, "extension", "extension")
    }

    /// `https_only` is false only so a test can talk to a loopback server; production never calls
    /// this with `false`.
    pub fn with_base(base: &str, https_only: bool) -> Result<Self, Error> {
        let base = Url::parse(base).map_err(|_| Error::Address)?;
        if !base.path().ends_with('/')
            || base.query().is_some()
            || base.fragment().is_some()
            || !base.username().is_empty()
        {
            return Err(Error::Address);
        }
        if https_only && base.scheme() != "https" {
            return Err(Error::Address);
        }
        let client = Self::client(https_only, reqwest::header::HeaderMap::new())?;
        Ok(Self {
            client,
            base,
            assets: None,
            revision: None,
        })
    }

    fn client(https_only: bool, headers: reqwest::header::HeaderMap) -> Result<Client, Error> {
        Client::builder()
            .https_only(https_only)
            .redirect(Policy::none())
            .default_headers(headers)
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| Error::Network)
    }

    /// Authenticated development catalog. Credentials are headers, never URLs;
    /// the origin is fixed and redirects/proxies remain disabled.
    #[cfg(feature = "private-store")]
    pub fn github_private(
        repository: &str,
        directory: &str,
        revision: &str,
        token: &str,
    ) -> Result<Self, Error> {
        Self::github(
            repository,
            directory,
            revision,
            Self::private_headers(token)?,
        )
    }

    pub fn github_public(repository: &str, directory: &str, revision: &str) -> Result<Self, Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("extension-store"),
        );
        Self::github(repository, directory, revision, headers)
    }

    fn github(
        repository: &str,
        directory: &str,
        revision: &str,
        headers: reqwest::header::HeaderMap,
    ) -> Result<Self, Error> {
        fn component(value: &str) -> bool {
            !value.is_empty()
                && value != "."
                && value != ".."
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        }
        if repository.len() > 256
            || repository.split('/').count() != 2
            || !repository.split('/').all(component)
            || directory.len() > 512
            || !directory.split('/').all(component)
            || revision.is_empty()
            || revision.len() > 256
            || !revision.split('/').all(component)
        {
            return Err(Error::Address);
        }
        let base = Url::parse(&format!(
            "https://api.github.com/repos/{repository}/contents/{directory}/"
        ))
        .map_err(|_| Error::Address)?;
        Ok(Self {
            client: Self::client(true, headers)?,
            base,
            assets: Some((
                Url::parse(&format!(
                    "https://api.github.com/repos/{repository}/releases/assets/"
                ))
                .map_err(|_| Error::Address)?,
                Self::client(true, reqwest::header::HeaderMap::new())?,
            )),
            revision: Some(revision.to_owned()),
        })
    }

    #[cfg(feature = "private-store")]
    fn private_headers(token: &str) -> Result<reqwest::header::HeaderMap, Error> {
        use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
        if token.is_empty()
            || token.len() > 4096
            || token.bytes().any(|byte| !byte.is_ascii_graphic())
        {
            return Err(Error::Address);
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::Address)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github.raw+json"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("private-extension-store"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static("2022-11-28"),
        );
        Ok(headers)
    }

    /// `name` must be one plain file name: ASCII letters, digits, `.`, `_`, `-`, not starting with a dot.
    pub fn address(&self, name: &str) -> Result<Url, Error> {
        let plain = !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !plain {
            return Err(Error::Name);
        }
        let url = self.base.join(name).map_err(|_| Error::Address)?;
        // Joined under the base and still on its host and directory: never anywhere else.
        if url.origin() != self.base.origin() || !url.path().starts_with(self.base.path()) {
            return Err(Error::Address);
        }
        let url = {
            let mut url = url;
            if let Some(revision) = &self.revision {
                url.query_pairs_mut().append_pair("ref", revision);
            }
            url
        };
        Ok(url)
    }

    /// The body of `name`, at most `limit` bytes. Counted as it arrives; the declared length is ignored.
    pub async fn get(&self, name: &str, limit: usize) -> Result<Vec<u8>, Error> {
        if self.assets.is_some() {
            if let Some(id) = name.strip_prefix("store-asset-") {
                return self
                    .get_asset(id.parse().map_err(|_| Error::Name)?, limit)
                    .await;
            }
            if let Some((address, raw, missing_is_pending)) = self.api_address(name)? {
                let response = self
                    .client
                    .get(address)
                    .header(
                        reqwest::header::ACCEPT,
                        if raw {
                            "application/vnd.github.raw+json"
                        } else {
                            "application/vnd.github+json"
                        },
                    )
                    .send()
                    .await
                    .map_err(|_| Error::Network)?;
                if missing_is_pending && response.status() == reqwest::StatusCode::NOT_FOUND {
                    return if limit >= 4 {
                        Ok(b"null".to_vec())
                    } else {
                        Err(Error::TooLarge)
                    };
                }
                return Self::body(response, limit).await;
            }
        }
        let response = self
            .client
            .get(self.address(name)?)
            .send()
            .await
            .map_err(|_| Error::Network)?;
        Self::body(response, limit).await
    }

    fn api_address(&self, name: &str) -> Result<Option<(Url, bool, bool)>, Error> {
        let (assets, _) = self.assets.as_ref().ok_or(Error::Address)?;
        let root = assets.join("../../").map_err(|_| Error::Address)?;
        if name == "store-tree" {
            let mut address = root.join("git/trees/").map_err(|_| Error::Address)?;
            address
                .path_segments_mut()
                .map_err(|_| Error::Address)?
                .pop_if_empty()
                .push(self.revision.as_deref().ok_or(Error::Address)?);
            address.query_pairs_mut().append_pair("recursive", "1");
            return Ok(Some((address, false, false)));
        }
        if let Some(sha) = name.strip_prefix("store-blob-") {
            if sha.len() != 40
                || !sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Error::Name);
            }
            return Ok(Some((
                root.join(&format!("git/blobs/{sha}"))
                    .map_err(|_| Error::Address)?,
                true,
                false,
            )));
        }
        if let Some(release) = name.strip_prefix("store-release-") {
            let (plugin, version) = release.rsplit_once("--v").ok_or(Error::Name)?;
            if plugin.is_empty()
                || plugin.len() > 128
                || plugin.starts_with('.')
                || !plugin
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || version.split('.').count() != 3
                || !version.split('.').all(|part| {
                    !part.is_empty() && part.len() <= 20 && part.bytes().all(|b| b.is_ascii_digit())
                })
            {
                return Err(Error::Name);
            }
            let mut address = root.join("releases/tags/").map_err(|_| Error::Address)?;
            address
                .path_segments_mut()
                .map_err(|_| Error::Address)?
                .pop_if_empty()
                .push(&format!("extension/{plugin}/v{version}"));
            return Ok(Some((address, false, true)));
        }
        Ok(None)
    }

    /// Fetch one asset from this repository, never an arbitrary URL supplied by a listing.
    /// Only this path permits one redirect to GitHub's HTTPS asset hosts. The second
    /// request uses a separate client with no credential-bearing default headers.
    pub async fn get_asset(&self, id: u64, limit: usize) -> Result<Vec<u8>, Error> {
        self.asset_download(id, limit, allowed_asset_redirect).await
    }

    async fn asset_download(
        &self,
        id: u64,
        limit: usize,
        allowed: fn(&Url) -> bool,
    ) -> Result<Vec<u8>, Error> {
        if id == 0 {
            return Err(Error::Name);
        }
        if limit > 128 * 1024 * 1024 {
            return Err(Error::TooLarge);
        }
        let (base, anonymous) = self.assets.as_ref().ok_or(Error::Address)?;
        let address = base.join(&id.to_string()).map_err(|_| Error::Address)?;
        tokio::time::timeout(Duration::from_secs(60), async {
            let mut response = self
                .client
                .get(address)
                .header(reqwest::header::ACCEPT, "application/octet-stream")
                .send()
                .await
                .map_err(|_| Error::Network)?;
            if response.status() == reqwest::StatusCode::FOUND {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(Error::Address)?;
                let target = Url::parse(location).map_err(|_| Error::Address)?;
                if !allowed(&target) {
                    return Err(Error::Address);
                }
                response = anonymous
                    .get(target)
                    .send()
                    .await
                    .map_err(|_| Error::Network)?;
            }
            Self::body(response, limit).await
        })
        .await
        .map_err(|_| Error::Network)?
    }

    async fn body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Error> {
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

fn allowed_asset_redirect(target: &Url) -> bool {
    target.scheme() == "https"
        && target.username().is_empty()
        && target.password().is_none()
        && target.fragment().is_none()
        && target.port_or_known_default() == Some(443)
        && matches!(
            target.host_str(),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
        )
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
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(body);
        reply
    }

    #[test]
    fn production_is_https_only_and_names_are_plain() {
        assert!(Store::production().is_ok());
        assert_eq!(
            Store::with_base("http://example.org/store/", true).err(),
            Some(Error::Address)
        );
        assert_eq!(
            Store::with_base("https://example.org/store", true).err(),
            Some(Error::Address),
            "needs a directory"
        );
        assert_eq!(
            Store::with_base("https://user@example.org/store/", true).err(),
            Some(Error::Address)
        );
        let store = Store::production().unwrap();
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "a\\b",
            "a?b",
            "a#b",
            "a b",
            "%2e%2e",
            "é",
            &"x".repeat(129),
        ] {
            assert_eq!(store.address(bad), Err(Error::Name), "{bad:?}");
        }
        assert_eq!(
            store.address("plugin.json").unwrap().as_str(),
            format!(
                "https://api.github.com/repos/{STORE_REPOSITORY}/contents/extension/plugin.json?ref=extension"
            )
        );
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
        let mut chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
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
        let missing = serve(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        )
        .await;
        assert_eq!(
            Store::with_base(&missing, false)
                .unwrap()
                .get("a", 10)
                .await,
            Err(Error::Status(404))
        );
        let moved = serve(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        assert_eq!(
            Store::with_base(&moved, false).unwrap().get("a", 10).await,
            Err(Error::Status(302))
        );
    }

    #[cfg(feature = "private-store")]
    #[test]
    fn private_catalog_is_scoped_to_github_and_keeps_credentials_out_of_urls() {
        let store =
            Store::github_private("example/private", "catalog", "main", "test-only").unwrap();
        assert_eq!(
            store.address("catalog.json").unwrap().as_str(),
            "https://api.github.com/repos/example/private/contents/catalog/catalog.json?ref=main"
        );
        for repository in ["../private", "example/private/extra", "https://elsewhere"] {
            assert!(Store::github_private(repository, "catalog", "main", "test-only").is_err());
        }
        for path in [
            "../catalog",
            "/catalog",
            "catalog/../other",
            "catalog?token=x",
        ] {
            assert!(Store::github_private("example/private", path, "main", "test-only").is_err());
        }
        assert!(Store::github_private("example/private", "catalog", "main", "").is_err());
        assert!(
            Store::github_private("example/private", "catalog", "main", "bad\nheader").is_err()
        );
    }

    #[cfg(feature = "private-store")]
    #[tokio::test]
    async fn authenticated_private_download_uses_raw_contents_and_bounded_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /catalog/catalog.json?ref=main http/1.1\r\n"));
            assert!(request.contains("\r\nauthorization: bearer test-only\r\n"));
            assert!(request.contains("\r\naccept: application/vnd.github.raw+json\r\n"));
            socket
                .write_all(&ok(b"signed-catalog-bytes"))
                .await
                .unwrap();
        });
        let mut store =
            Store::github_private("example/private", "catalog", "main", "test-only").unwrap();
        // Only this unit test can replace the fixed GitHub origin.
        store.base = Url::parse(&format!("http://{endpoint}/catalog/")).unwrap();
        store.client = Store::client(false, Store::private_headers("test-only").unwrap()).unwrap();
        assert_eq!(
            store.get("catalog.json", 20).await.unwrap(),
            b"signed-catalog-bytes"
        );
        server.await.unwrap();
    }

    #[cfg(feature = "private-store")]
    #[tokio::test]
    async fn authenticated_download_refuses_redirects_instead_of_forwarding_credentials() {
        let moved = serve(b"HTTP/1.1 302 Found\r\nLocation: https://example.invalid/outside\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        let mut store =
            Store::github_private("example/private", "catalog", "main", "test-only").unwrap();
        store.base = Url::parse(&moved).unwrap();
        store.client = Store::client(false, Store::private_headers("test-only").unwrap()).unwrap();
        assert_eq!(store.get("catalog.json", 16).await, Err(Error::Status(302)));
    }

    #[test]
    fn release_redirects_are_https_and_exactly_github_asset_hosts() {
        for allowed in [
            "https://release-assets.githubusercontent.com/blob?signature=test-only",
            "https://objects.githubusercontent.com/blob",
        ] {
            assert!(allowed_asset_redirect(&Url::parse(allowed).unwrap()));
        }
        for denied in [
            "http://objects.githubusercontent.com/blob",
            "https://objects.githubusercontent.com.example.org/blob",
            "https://example.org/blob",
            "https://user@objects.githubusercontent.com/blob",
            "https://objects.githubusercontent.com:444/blob",
            "https://objects.githubusercontent.com/blob#fragment",
        ] {
            assert!(!allowed_asset_redirect(&Url::parse(denied).unwrap()));
        }
    }

    async fn once(reply: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                assert!(request.len() <= 8192);
            }
            socket.write_all(&reply).await.unwrap();
            String::from_utf8(request).unwrap().to_ascii_lowercase()
        });
        (address, task)
    }

    fn loopback_redirect(target: &Url) -> bool {
        target.scheme() == "http"
            && target.host_str() == Some("127.0.0.1")
            && target.username().is_empty()
    }

    #[tokio::test]
    async fn release_asset_redirect_strips_authentication_and_preserves_size_bound() {
        use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
        for limit in [4, 5] {
            let (cdn, received) = once(ok(b"hello")).await;
            let redirect = format!(
                "HTTP/1.1 302 Found\r\nLocation: {cdn}blob\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let (api, initial) = once(redirect.into_bytes()).await;
            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_static("Bearer fixture-only"),
            );
            let mut store = Store::with_base(&api, false).unwrap();
            store.client = Store::client(false, headers).unwrap();
            store.assets = Some((
                Url::parse(&api).unwrap(),
                Store::client(false, HeaderMap::new()).unwrap(),
            ));
            let downloaded = store.asset_download(42, limit, loopback_redirect).await;
            if limit == 5 {
                assert_eq!(downloaded.unwrap(), b"hello");
            } else {
                assert_eq!(downloaded, Err(Error::TooLarge));
            }
            let first = initial.await.unwrap();
            assert!(first.starts_with("get /42 http/1.1"));
            assert!(first.contains("\r\nauthorization: bearer fixture-only\r\n"));
            assert!(first.contains("\r\naccept: application/octet-stream\r\n"));
            let second = received.await.unwrap();
            assert!(second.starts_with("get /blob http/1.1"));
            assert!(!second.contains("\r\nauthorization:"));
        }
    }

    #[tokio::test]
    async fn release_asset_refuses_a_second_redirect_and_supports_direct_bytes() {
        for (response, expected) in [
            (ok(b"hello"), Ok(b"hello".to_vec())),
            (b"HTTP/1.1 302 Found\r\nLocation: https://example.invalid/third\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                Err(Error::Status(302))),
        ] {
            let (cdn, received) = once(response).await;
            let redirect = format!("HTTP/1.1 302 Found\r\nLocation: {cdn}blob\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let (api, initial) = once(redirect.into_bytes()).await;
            let mut store = Store::with_base(&api, false).unwrap();
            store.assets = Some((Url::parse(&api).unwrap(),
                Store::client(false, reqwest::header::HeaderMap::new()).unwrap()));
            assert_eq!(store.asset_download(42, 5, loopback_redirect).await, expected);
            initial.await.unwrap();
            received.await.unwrap();
        }
        let (api, initial) = once(ok(b"hello")).await;
        let mut store = Store::with_base(&api, false).unwrap();
        store.assets = Some((
            Url::parse(&api).unwrap(),
            Store::client(false, reqwest::header::HeaderMap::new()).unwrap(),
        ));
        assert_eq!(store.get_asset(42, 5).await.unwrap(), b"hello");
        initial.await.unwrap();
    }

    #[tokio::test]
    async fn public_branch_and_per_plugin_release_requests_are_scoped_on_the_wire() {
        for (name, path, accept, missing) in [
            (
                "store-tree".to_owned(),
                "/git/trees/extension?recursive=1".to_owned(),
                "application/vnd.github+json",
                false,
            ),
            (
                format!("store-blob-{}", "a".repeat(40)),
                format!("/git/blobs/{}", "a".repeat(40)),
                "application/vnd.github.raw+json",
                false,
            ),
            (
                "store-release-upload--v1.2.3".to_owned(),
                "/releases/tags/extension%2fupload%2fv1.2.3".to_owned(),
                "application/vnd.github+json",
                true,
            ),
            (
                "store-asset-42".to_owned(),
                "/releases/assets/42".to_owned(),
                "application/octet-stream",
                false,
            ),
        ] {
            let response = if missing {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            } else {
                ok(b"data")
            };
            let (api, received) = once(response).await;
            let mut store =
                Store::github_public("example/plugins", "extension", "extension").unwrap();
            store.client = Store::client(false, reqwest::header::HeaderMap::new()).unwrap();
            store.assets = Some((
                Url::parse(&format!("{api}releases/assets/")).unwrap(),
                Store::client(false, reqwest::header::HeaderMap::new()).unwrap(),
            ));
            assert_eq!(
                store.get(&name, 4).await.unwrap(),
                if missing { b"null" } else { b"data" }
            );
            let request = received.await.unwrap();
            assert!(
                request.starts_with(&format!("get {path} http/1.1")),
                "{request}"
            );
            assert!(request.contains(&format!("\r\naccept: {accept}\r\n")));
            assert!(!request.contains("\r\nauthorization:"));
        }
        let store = Store::github_public("example/plugins", "extension", "extension").unwrap();
        assert!(
            store
                .address("plugin.json")
                .unwrap()
                .as_str()
                .ends_with("?ref=extension")
        );
        for name in [
            "store-blob-../x",
            "store-release-../x--v1.2.3",
            "store-release-x--v1/2/3",
        ] {
            assert_eq!(store.api_address(name), Err(Error::Name));
        }
    }
}
