#![cfg(unix)]
use peersus_plugin::frame;
use peersus_plugin_runtime::{DataRequest, Transport, socket::SocketTransport};
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::Duration,
};
fn read_frame(stream: &mut UnixStream, limit: usize) -> serde_json::Value {
    let mut bytes = vec![0; limit];
    let length = frame::read_blocking_into(stream, &mut bytes, limit).unwrap();
    serde_json::from_slice(&bytes[..length]).unwrap()
}

#[test]
fn connected_descriptors_use_the_existing_control_and_data_authentication() {
    let (control, mut owner) = UnixStream::pair().unwrap();
    owner
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (data, mut receiver) = UnixStream::pair().unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let source = Arc::new(Mutex::new(Some(data)));
    let data_source = Arc::clone(&source);
    let mut transport =
        SocketTransport::from_connected(control, "private bootstrap".into(), move |_| {
            data_source
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| io::Error::other("no more descriptors"))
        })
        .unwrap();
    assert_eq!(
        read_frame(&mut owner, frame::CONTROL_LIMIT)["token"],
        "private bootstrap"
    );
    frame::write_blocking_bytes(
        &mut owner,
        br#"{"type":"actual delivery"}"#,
        frame::CONTROL_LIMIT,
    )
    .unwrap();
    let mut bytes = [0; 128];
    let amount = transport.receive(&mut bytes).unwrap();
    assert_eq!(&bytes[..amount], br#"{"type":"actual delivery"}"#);
    transport.send(br#"{"op":"actual operation"}"#).unwrap();
    let amount = frame::read_blocking_into(&mut owner, &mut bytes, frame::CONTROL_LIMIT).unwrap();
    assert_eq!(&bytes[..amount], br#"{"op":"actual operation"}"#);
    let mut stream = transport.open_data(DataRequest::Claim(u64::MAX)).unwrap();
    let hello = read_frame(&mut receiver, 1024);
    assert_eq!(hello["token"], "private bootstrap");
    assert_eq!(hello["ticket"].as_u64(), Some(0));
    assert_eq!(hello["claim"].as_u64(), Some(u64::MAX));
    stream.write_all(b"real descriptor data").unwrap();
    transport.shutdown_data(&mut stream).unwrap();
    let mut data = Vec::new();
    receiver.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"real descriptor data");
    drop(receiver);
    assert_eq!(stream.read(&mut bytes).unwrap(), 0);
    assert!(
        transport.open_data(DataRequest::Ticket(1)).is_err(),
        "a refused descriptor must not fall back to an ambient socket"
    );
}

#[test]
fn connected_transport_rejects_an_oversized_control_frame_before_reading_its_body() {
    let (control, mut owner) = UnixStream::pair().unwrap();
    let mut transport = SocketTransport::from_connected(control, "bootstrap".into(), |_| {
        Err(io::Error::other("denied"))
    })
    .unwrap();
    let _ = read_frame(&mut owner, frame::CONTROL_LIMIT);
    owner
        .write_all(&((frame::CONTROL_LIMIT + 1) as u32).to_be_bytes())
        .unwrap();
    let error = transport.receive(&mut [0; 32]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn transferred_descriptors_keep_the_socket_configuration_set_by_the_owner() {
    let (control, mut receiver) = UnixStream::pair().unwrap();
    let control_probe = control.try_clone().unwrap();
    control
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (data, mut data_receiver) = UnixStream::pair().unwrap();
    data.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    data.set_write_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut data = Some(data);
    let mut transport = SocketTransport::from_connected(control, "bootstrap".into(), move |_| {
        data.take()
            .ok_or_else(|| io::Error::other("descriptor unavailable"))
    })
    .unwrap();
    let _ = read_frame(&mut receiver, frame::CONTROL_LIMIT);
    assert_eq!(
        control_probe.write_timeout().unwrap(),
        Some(Duration::from_secs(2)),
        "an isolated worker must not reconfigure an owner-created socket"
    );
    let stream = transport.open_data(DataRequest::Ticket(7)).unwrap();
    let _ = read_frame(&mut data_receiver, 1024);
    assert_eq!(stream.read_timeout().unwrap(), Some(Duration::from_secs(3)));
    assert_eq!(
        stream.write_timeout().unwrap(),
        Some(Duration::from_secs(4))
    );
}

#[test]
fn a_transferred_canvas_descriptor_keeps_its_owner_configured_timeout() {
    let (control, _owner) = UnixStream::pair().unwrap();
    let (data, mut receiver) = UnixStream::pair().unwrap();
    let timeout = Duration::from_millis(40);
    data.set_write_timeout(Some(timeout)).unwrap();
    let mut data = Some(data);
    let claim = u64::MAX;
    let mut transport =
        SocketTransport::from_connected(control, "bootstrap".into(), move |request| {
            assert_eq!(request, DataRequest::CanvasClaim(claim));
            data.take()
                .ok_or_else(|| io::Error::other("descriptor unavailable"))
        })
        .unwrap();
    let stream = transport
        .open_data(DataRequest::CanvasClaim(claim))
        .unwrap();
    let hello = read_frame(&mut receiver, 1024);
    assert_eq!(hello["token"], "bootstrap");
    assert_eq!(hello["ticket"].as_u64(), Some(0));
    assert_eq!(hello["claim"].as_u64(), Some(claim));
    assert_eq!(stream.write_timeout().unwrap(), Some(timeout));
}
