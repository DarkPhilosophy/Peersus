use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

pub(super) async fn selected(name: &str, bytes: &[u8]) -> (tempfile::TempDir, SelectedFile) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    tokio::fs::write(&path, bytes).await.unwrap();
    let file = SelectedFile::open(&path).await.unwrap();
    (dir, file)
}
pub(super) fn credential() -> Credential {
    Credential::new("fixture-not-a-real-token".into()).unwrap()
}
pub(super) async fn fixture(
    status: u16,
    body: String,
    extra: &str,
) -> (Uploader, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Url::parse(&format!(
        "http://{}/api/upload",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let extra = extra.to_owned();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        request
    });
    (Uploader::with_endpoint(endpoint, false).unwrap(), task)
}
pub(super) async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let header_end = loop {
        let byte = stream.read_u8().await.unwrap();
        request.push(byte);
        if request.ends_with(b"\r\n\r\n") {
            break request.len();
        }
        assert!(request.len() < 16384);
    };
    let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(length < MAX_FILE as usize + 16384);
    request.resize(header_end + length, 0);
    stream.read_exact(&mut request[header_end..]).await.unwrap();
    request
}
pub(super) fn result() -> String {
    r#"{"success":true,"anonymous":false,"files":[{"id":"abc-12","url":"https://poo.wang/f/abc-12","name":"photo.png"}]}"#.into()
}

#[tokio::test]
async fn sends_exact_contract_once_without_origin_cookie_or_media_mime_mismatch() {
    let (_dir, file) = selected("photo.png", b"fixture image bytes").await;
    let (uploader, server) = fixture(200, result(), "").await;
    assert_eq!(
        uploader.upload(file, credential()).await.unwrap(),
        "https://poo.wang/f/abc-12"
    );
    let request = server.await.unwrap();
    let text = String::from_utf8(request).unwrap();
    let (headers, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("POST /api/upload HTTP/1.1\r\n"));
    let headers = headers.to_ascii_lowercase();
    assert!(headers.contains(&format!(
        "authorization: bearer {}\r\n",
        credential().0.as_str()
    )));
    assert!(!headers.contains("origin:"));
    assert!(!headers.contains("cookie:"));
    assert!(body.contains("name=\"files\"; filename=\"photo.png\""));
    assert!(body.contains("fixture image bytes"));
    assert!(body.contains("name=\"burnMode\"\r\n\r\nBurn after 24 hours"));
    assert!(body.contains("name=\"showcase\"\r\n\r\nfalse"));
    assert!(!body.to_ascii_lowercase().contains("content-type:"));
}
#[tokio::test]
async fn maps_errors_and_never_follows_redirects() {
    for (status, expected) in [
        (401, Error::Authentication),
        (403, Error::Authentication),
        (413, Error::TooLarge),
        (415, Error::FileType),
        (429, Error::RateLimited),
        (503, Error::Status(503)),
        (307, Error::Status(307)),
    ] {
        let (_dir, file) = selected("sample.txt", b"hello").await;
        let (uploader, server) = fixture(
            status,
            "do not display hostile server messages".into(),
            "Location: https://evil.invalid/steal\r\n",
        )
        .await;
        assert_eq!(
            uploader.upload(file, credential()).await.unwrap_err(),
            expected
        );
        server.await.unwrap();
    }
}
#[tokio::test]
async fn rejects_response_over_bound_and_malicious_link() {
    for (body, expected) in [
        ("x".repeat(MAX_RESPONSE + 1), Error::ResponseSize),
        (
            result().replace("https://poo.wang/f/abc-12", "https://evil.invalid/f/abc-12"),
            Error::Response,
        ),
    ] {
        let (_dir, file) = selected("sample.txt", b"hello").await;
        let (uploader, server) = fixture(200, body, "").await;
        assert_eq!(
            uploader.upload(file, credential()).await.unwrap_err(),
            expected
        );
        server.await.unwrap();
    }
}
#[tokio::test]
async fn changed_pinned_file_fails_before_network() {
    let (dir, file) = selected("sample.txt", b"original").await;
    tokio::fs::write(dir.path().join("sample.txt"), b"modified")
        .await
        .unwrap();
    let endpoint = Url::parse("http://127.0.0.1:1/api/upload").unwrap();
    let uploader = Uploader::with_endpoint(endpoint, false).unwrap();
    assert_eq!(
        uploader.upload(file, credential()).await.unwrap_err(),
        Error::Changed
    );
}
#[tokio::test]
async fn empty_and_oversized_files_are_rejected_without_upload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large");
    let file = tokio::fs::File::create(&path).await.unwrap();
    assert!(matches!(SelectedFile::open(&path).await, Err(Error::Size)));
    file.set_len(MAX_FILE + 1).await.unwrap();
    assert!(matches!(SelectedFile::open(&path).await, Err(Error::Size)));
}
#[tokio::test]
async fn cancellation_drops_the_active_request_without_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uploader = Uploader::with_endpoint(
        Url::parse(&format!(
            "http://{}/api/upload",
            listener.local_addr().unwrap()
        ))
        .unwrap(),
        false,
    )
    .unwrap();
    let (seen, received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        seen.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let (_dir, file) = selected("cancel.txt", b"hello").await;
    let request = tokio::spawn(async move { uploader.upload(file, credential()).await });
    received.await.unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    server.await.unwrap();
}
#[test]
fn only_canonical_links_and_single_file_success_are_accepted() {
    for url in [
        "http://poo.wang/f/abc-12",
        "https://poo.wang.evil/f/abc-12",
        "https://poo.wang@evil/f/abc-12",
        "https://poo.wang:443/f/abc-12",
        "https://poo.wang/f/abc-12?x=1",
        "https://poo.wang/f/abc-12#fragment",
        "javascript:alert(1)",
    ] {
        assert_eq!(
            parse_result(
                result()
                    .replace("https://poo.wang/f/abc-12", url)
                    .as_bytes()
            ),
            Err(Error::Response)
        );
    }
    assert_eq!(
        parse_result(br#"{"success":false,"files":[]}"#),
        Err(Error::Response)
    );
    assert_eq!(
        parse_result(br#"{"success":true,"files":[]}"#),
        Err(Error::Response)
    );
    assert_eq!(safe_name("../../bad\r\n\"file.png"), ".._.._bad___file.png");
    assert!(Credential::new("x\r\ny".into()).is_err());
}
#[test]
fn declared_manifest_has_no_secrets_and_is_valid() {
    let manifest: serde_json::Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
    assert_eq!(manifest["platforms"], serde_json::json!(["linux", "macos"]));
    #[cfg(feature = "desktop")]
    {
        let parsed: peersus_plugin_api::manifest::Manifest =
            serde_json::from_value(manifest).unwrap();
        parsed.validate().unwrap();
    }
}

#[tokio::test]
async fn production_client_refuses_cleartext_before_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut uploader = Uploader::production().unwrap();
    uploader.endpoint = Url::parse(&format!(
        "http://{}/api/upload",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let (_dir, file) = selected("cleartext.txt", b"must never be sent").await;
    assert_eq!(
        uploader.upload(file, credential()).await.unwrap_err(),
        Error::Network
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
