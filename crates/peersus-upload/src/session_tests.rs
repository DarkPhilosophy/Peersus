use super::tests::{credential, read_request, result, selected};
use super::*;
use tokio::{io::AsyncWriteExt, net::TcpListener};

#[tokio::test]
async fn large_upload_uses_session_then_exact_parts_then_completion() {
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
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in [
            format!(
                r#"{{"token":"fixture-session","partSize":{PART_SIZE},"parts":2,"concurrency":4}}"#
            ),
            r#"{"success":true,"index":0,"receipt":"receipt-zero"}"#.into(),
            r#"{"success":true,"index":1,"receipt":"receipt-one"}"#.into(),
            result(),
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut stream).await);
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
        requests
    });
    let payload = vec![b'x'; PART_SIZE as usize + 5];
    let (_dir, file) = selected("large.txt", &payload).await;
    assert_eq!(
        uploader.upload(file, credential()).await.unwrap(),
        "https://poo.wang/f/abc-12"
    );
    let requests = server.await.unwrap();
    fn split(request: &[u8]) -> (&str, &[u8]) {
        let index = request.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
        (
            std::str::from_utf8(&request[..index]).unwrap(),
            &request[index + 4..],
        )
    }
    let (headers, body) = split(&requests[0]);
    assert!(headers.starts_with("POST /api/upload/session HTTP/1.1"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer ")
    );
    let value: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"name":"large.txt","type":"","size":PART_SIZE+5,"burnMode":RETENTION,"showcase":false})
    );
    for (index, request) in requests[1..3].iter().enumerate() {
        let (headers, body) = split(request);
        assert!(headers.starts_with(&format!(
            "PUT /api/upload/session/part?index={index} HTTP/1.1"
        )));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("x-upload-session: fixture-session")
        );
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        assert_eq!(
            body,
            if index == 0 {
                &payload[..PART_SIZE as usize]
            } else {
                &payload[PART_SIZE as usize..]
            }
        );
    }
    let (headers, body) = split(&requests[3]);
    assert!(headers.starts_with("POST /api/upload/session/complete HTTP/1.1"));
    assert!(!headers.to_ascii_lowercase().contains("authorization:"));
    let value: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"token":"fixture-session","receipts":["receipt-zero","receipt-one"]})
    );
}

#[tokio::test]
async fn malicious_session_cannot_allocate_unbounded_part_or_send_to_another_origin() {
    let payload = vec![0; PART_SIZE as usize + 1];
    for session in [
        r#"{"token":"fixture","partSize":999999999999,"parts":1}"#,
        r#"{"token":"fixture","partSize":3145728,"parts":999999999}"#,
        r#"{"token":"fixture","partSize":0,"parts":1}"#,
        r#"{"token":"fixture\r\ninjected","partSize":3145728,"parts":2}"#,
    ] {
        let (_dir, file) = selected("large.txt", &payload).await;
        let (uploader, server) = super::tests::fixture(200, session.into(), "").await;
        assert_eq!(
            uploader.upload(file, credential()).await.unwrap_err(),
            Error::Response
        );
        let request = server.await.unwrap();
        assert!(request.starts_with(b"POST /api/upload/session HTTP/1.1"));
    }
}
