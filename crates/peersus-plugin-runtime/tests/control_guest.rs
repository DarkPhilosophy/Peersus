#![cfg(unix)]

use peersus_plugin::frame::{self, CONTROL_LIMIT};
use peersus_plugin_runtime::{run, socket::SocketTransport};
use serde_json::{Value, json};
use std::{collections::VecDeque, io::Write, net::Shutdown, os::unix::net::UnixStream, thread, time::Duration};

struct Worker {
    control: UnixStream,
    data: Vec<UnixStream>,
    task: Option<thread::JoinHandle<Result<(), wasmi::Error>>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.control.shutdown(Shutdown::Both);
        for data in &self.data { let _ = data.shutdown(Shutdown::Both); }
        if let Some(task) = self.task.take() {
            let result = task.join();
            if !thread::panicking() { assert!(result.unwrap().is_ok()); }
        }
    }
}
fn read(stream: &mut UnixStream) -> Value {
    let mut bytes = vec![0; CONTROL_LIMIT];
    let count = frame::read_blocking_into(stream, &mut bytes, CONTROL_LIMIT).unwrap();
    serde_json::from_slice(&bytes[..count]).unwrap()
}
fn send(stream: &mut UnixStream, value: Value) {
    frame::write_blocking(stream, &value, CONTROL_LIMIT).unwrap();
}
fn answer(stream: &mut UnixStream, method: &str, value: Value) -> Value {
    let call = read(stream);
    assert_eq!(call["method"], method, "unexpected guest operation: {call}");
    send(stream, json!({"type":"result","id":call["id"],"result":{"status":"ok","value":value}}));
    call
}

/// Runs the separately built, real guest, not a replacement WAT or a receiver mock.
#[test]
#[ignore = "requires PEERSUS_TEST_CONTROL_MODULE pointing to the built control guest"]
fn real_guest_serves_two_live_controllers_without_waiting_for_eof() {
    let bytes = std::fs::read(std::env::var_os("PEERSUS_TEST_CONTROL_MODULE").expect("guest build required")).unwrap();
    let (control, mut remote) = UnixStream::pair().unwrap();
    remote.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut guests = VecDeque::new();
    let mut data = Vec::new();
    for _ in 0..2 {
        let (guest, sender) = UnixStream::pair().unwrap();
        guest.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sender.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        guests.push_back(guest);
        data.push(sender);
    }
    let mut transport = SocketTransport::from_connected(control, "fixture".into(), move || {
        guests.pop_front().ok_or_else(|| std::io::Error::other("unexpected stream"))
    }).unwrap();
    let task = thread::spawn(move || run(&bytes, &mut transport));
    let mut worker = Worker { control: remote.try_clone().unwrap(), data, task: Some(task) };
    let _hello = read(&mut remote);
    answer(&mut remote, "get", json!({"settings":{}}));
    answer(&mut remote, "set", json!({}));
    for ticket in [1, 2] {
        send(&mut remote, json!({"type":"call","ticket":ticket,"method":"receive","args":{"peer":format!("controller-{ticket}")}}));
        answer(&mut remote, "get", json!({"settings":{"independent":true}}));
        let hello = read(&mut worker.data[ticket - 1]);
        assert_eq!(hello["ticket"], ticket);
        if ticket == 1 {
            worker.data[0].write_all(b"{\"t\":\"move\",\"dx\":").unwrap();
        }
    }
    worker.data[1].write_all(b"{\"t\":\"move\",\"dx\":2,\"dy\":0}\n").unwrap();
    let second = answer(&mut remote, "inject", json!({}));
    assert_eq!(second["args"]["owner"], "controller-2");
    assert_eq!(second["args"]["pointer"], "own");
    worker.data[0].write_all(b"1,\"dy\":0}\n").unwrap();
    let first = answer(&mut remote, "inject", json!({}));
    assert_eq!(first["args"]["owner"], "controller-1");
    assert_eq!(first["args"]["events"][0]["dx"], 1);
    worker.data[0].shutdown(Shutdown::Write).unwrap();
    let release = answer(&mut remote, "release", json!({}));
    assert_eq!(release["args"]["owner"], "controller-1");
    let reply = read(&mut remote);
    assert_eq!(reply["ticket"], 1);
    assert_eq!(reply["result"]["status"], "ok");
    worker.data[1].write_all(b"{\"t\":\"move\",\"dx\":3,\"dy\":0}\n").unwrap();
    let second = answer(&mut remote, "inject", json!({}));
    assert_eq!(second["args"]["owner"], "controller-2");
    assert_eq!(second["args"]["events"][0]["dx"], 3);
}
