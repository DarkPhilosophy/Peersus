use peersus_api::{Error, manifest::Manifest, message::Operation, permission::Permission};
use peersus_plugin::hub::{Channel, HOST, Hub};
use serde_json::json;
use std::time::Duration;

fn host_manifest() -> Manifest {
    serde_json::from_value(json!({"manifest_version":1,"execution":{"kind":"process"},"capabilities":{"interactive":"Explicit interactive operation"},"imports":[],"exports":[{"service":"interactive","version":1,"description":"Interactive host fixture","consumers":["upload"],"methods":{"choose":{"capability":"interactive","description":"Choose"}},"events":{}}]})).unwrap()
}
fn guest_manifest() -> Manifest {
    serde_json::from_value(json!({"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[{"provider":"host","service":"interactive","version":1,"methods":["choose"],"events":[]},{"provider":"other","service":"interactive","version":1,"methods":["choose"],"events":[]}],"exports":[]})).unwrap()
}
fn submit(channel: &Channel, provider: &str, id: u64, timeout_ms: u32) -> Result<(), Error> {
    channel.submit(Operation::Call {
        id,
        provider: provider.into(),
        service: "interactive".into(),
        version: 1,
        method: "choose".into(),
        args: json!({}),
        timeout_ms,
    })
}
fn deadlines(seconds: u64) -> std::collections::BTreeMap<(String, u32, String), Duration> {
    [(
        ("interactive".into(), 1, "choose".into()),
        Duration::from_secs(seconds),
    )]
    .into()
}
#[test]
fn only_the_live_host_may_extend_its_own_methods_without_bypassing_grants() {
    let hub = Hub::default();
    let host_generation = hub
        .register_host(HOST, host_manifest(), Default::default())
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    let guest_generation = hub
        .register("upload", guest_manifest(), Default::default())
        .unwrap();
    let guest = hub.connect("upload", guest_generation).unwrap();
    assert_eq!(submit(&guest, HOST, 1, 600000), Err(Error::Timeout));
    assert_eq!(
        guest.set_host_method_timeouts(deadlines(600)),
        Err(Error::Denied)
    );
    host.set_host_method_timeouts(deadlines(600)).unwrap();
    // A longer deadline is not a grant: the call is still refused for want of one, and says which.
    assert_eq!(
        submit(&guest, HOST, 1, 600000),
        Err(Error::MissingPermission(
            Permission::new(HOST, "interactive").unwrap()
        ))
    );
    hub.set_permissions(
        "upload",
        [Permission::new(HOST, "interactive").unwrap()].into(),
    )
    .unwrap();
    submit(&guest, HOST, 1, 600000).unwrap();
    assert!(host.try_recv().unwrap().is_some());
    assert_eq!(submit(&guest, HOST, 2, 600001), Err(Error::Timeout));
    assert_eq!(
        host.set_host_method_timeouts(deadlines(601)),
        Err(Error::Timeout)
    );
    submit(&guest, HOST, 2, 600000).unwrap();
    assert_eq!(
        host.set_host_method_timeouts(
            [(
                ("missing".into(), 1, "choose".into()),
                Duration::from_secs(60)
            )]
            .into()
        ),
        Err(Error::UnknownMethod)
    );
    let generation = hub
        .register_host(HOST, host_manifest(), Default::default())
        .unwrap();
    assert_eq!(
        host.set_host_method_timeouts(deadlines(600)),
        Err(Error::Cancelled)
    );
    let _new_host = hub.connect(HOST, generation).unwrap();
    assert_eq!(submit(&guest, HOST, 3, 600000), Err(Error::Timeout));
}
#[test]
fn host_timeout_overrides_do_not_extend_plugin_to_plugin_calls() {
    let hub = Hub::default();
    let generation = hub
        .register_host(HOST, host_manifest(), Default::default())
        .unwrap();
    let host = hub.connect(HOST, generation).unwrap();
    host.set_host_method_timeouts(deadlines(600)).unwrap();
    let generation = hub
        .register(
            "upload",
            guest_manifest(),
            [Permission::new("other", "interactive").unwrap()].into(),
        )
        .unwrap();
    let guest = hub.connect("upload", generation).unwrap();
    hub.register("other", host_manifest(), Default::default())
        .unwrap();
    assert_eq!(submit(&guest, "other", 1, 600000), Err(Error::Timeout));
}
