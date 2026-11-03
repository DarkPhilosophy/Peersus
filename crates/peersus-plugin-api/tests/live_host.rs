use peersus_plugin_api::{hub::{Channel, Hub, HOST}, manifest::{Export, Import, Manifest, Member}, message::{Delivery, Operation, Outcome}, Error};
use serde_json::json;

fn provider() -> Manifest {
    Manifest { exports: vec![Export {
        service: "svc".into(), version: 1, description: "Live contract test.".into(), consumers: [HOST.into()].into(),
        methods: [("act".into(), Member { capability: None, description: "Complete a call.".into() })].into(),
        events: [("tick".into(), Member { capability: None, description: "Publish a tick.".into() })].into(),
    }], ..Manifest::default() }
}
fn host(providers: &[&str]) -> Manifest {
    Manifest { imports: providers.iter().map(|provider| Import {
        provider: (*provider).into(), service: "svc".into(), version: 1,
        methods: ["act".into()].into(), events: ["tick".into()].into(),
    }).collect(), ..Manifest::default() }
}
fn call(channel: &Channel, provider: &str, id: u64) -> Result<(), Error> {
    channel.submit(Operation::Call { id, provider: provider.into(), service: "svc".into(), version: 1, method: "act".into(), args: json!(null), timeout_ms: 5000 })
}

#[test]
fn adding_and_removing_imports_keeps_admitted_calls_and_unchanged_subscriptions_alive() {
    let hub = Hub::default();
    let generation = hub.register_host(HOST, host(&["one"]), Default::default()).unwrap();
    let app = hub.connect(HOST, generation).unwrap();
    let one = hub.connect("one", hub.register("one", provider(), Default::default()).unwrap()).unwrap();
    let two = hub.connect("two", hub.register("two", provider(), Default::default()).unwrap()).unwrap();
    call(&app, "one", 1).unwrap();
    let Delivery::Call { ticket, .. } = one.try_recv().unwrap().unwrap() else { panic!("first call not delivered"); };
    app.submit(Operation::Subscribe { provider: "one".into(), service: "svc".into(), version: 1, event: "tick".into() }).unwrap();
    assert_eq!(call(&app, "two", 2), Err(Error::Denied));
    app.update_host_manifest(host(&["one", "two"])).unwrap();
    one.submit(Operation::Reply { ticket, result: Outcome::ok(json!("unchanged call")) }).unwrap();
    assert_eq!(app.try_recv().unwrap(), Some(Delivery::Result { id: 1, result: Outcome::ok(json!("unchanged call")) }));
    one.submit(Operation::Publish { service: "svc".into(), version: 1, event: "tick".into(), data: json!(7) }).unwrap();
    assert!(matches!(app.try_recv().unwrap(), Some(Delivery::Event { data, .. }) if data == json!(7)));
    call(&app, "two", 2).unwrap();
    let Delivery::Call { ticket, .. } = two.try_recv().unwrap().unwrap() else { panic!("new import not applied"); };
    app.update_host_manifest(host(&["one"])).unwrap();
    assert_eq!(call(&app, "two", 3), Err(Error::Denied));
    two.submit(Operation::Reply { ticket, result: Outcome::ok(json!("admitted before removal")) }).unwrap();
    assert_eq!(app.try_recv().unwrap(), Some(Delivery::Result { id: 2, result: Outcome::ok(json!("admitted before removal")) }));
    app.update_host_manifest(host(&[])).unwrap();
    one.submit(Operation::Publish { service: "svc".into(), version: 1, event: "tick".into(), data: json!(8) }).unwrap();
    assert_eq!(app.try_recv().unwrap(), None, "a removed import must revoke its subscriptions");
}

#[test]
fn plugins_and_disconnected_host_channels_cannot_change_the_host_contract() {
    let hub = Hub::default();
    let app = hub.connect(HOST, hub.register_host(HOST, host(&[]), Default::default()).unwrap()).unwrap();
    let plugin = hub.connect("one", hub.register("one", provider(), Default::default()).unwrap()).unwrap();
    assert_eq!(plugin.update_host_manifest(host(&["one"])), Err(Error::Denied));
    let mut invalid = host(&["one"]); invalid.manifest_version = u32::MAX;
    assert!(app.update_host_manifest(invalid).is_err());
    assert_eq!(call(&app, "one", 1), Err(Error::Denied), "an invalid update must change nothing");
    app.disconnect();
    assert!(app.update_host_manifest(host(&["one"])).is_err());
}
