//! A child process exercises the real broker under the process-wide diagnostic backend.
use peersus_api::{
    manifest::{Export, Import, Manifest, Member},
    message::{Delivery, Operation, Outcome},
};
use peersus_plugin::hub::Hub;
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

#[test]
fn diagnostic_broker_child() {
    if std::env::var_os("PEERSUS_BROKER_DIAGNOSTIC_CHILD").is_none() {
        return;
    }
    let _guard = peersus_diagnostics::init("broker-test").unwrap();
    let hub = Hub::default();
    let member = Member {
        capability: None,
        description: "Synthetic operation".into(),
    };
    let provider = Manifest {
        exports: vec![Export {
            service: "private-service-marker".into(),
            version: 1,
            description: "Synthetic service".into(),
            consumers: BTreeSet::from(["private-caller-marker".into()]),
            methods: BTreeMap::from([("private-method-marker".into(), member)]),
            events: BTreeMap::new(),
        }],
        ..Manifest::new(peersus_api::manifest::Execution::Process)
    };
    let caller = Manifest {
        imports: vec![Import {
            provider: "private-provider-marker".into(),
            service: "private-service-marker".into(),
            version: 1,
            methods: BTreeSet::from(["private-method-marker".into()]),
            events: BTreeSet::new(),
        }],
        ..Manifest::new(peersus_api::manifest::Execution::Process)
    };
    let generation = hub
        .register("private-provider-marker", provider, BTreeSet::new())
        .unwrap();
    let provider = hub.connect("private-provider-marker", generation).unwrap();
    let generation = hub
        .register("private-caller-marker", caller, BTreeSet::new())
        .unwrap();
    let caller = hub.connect("private-caller-marker", generation).unwrap();
    let operation = |id, method: &str| Operation::Call {
        id,
        provider: "private-provider-marker".into(),
        service: "private-service-marker".into(),
        version: 1,
        method: method.into(),
        args: serde_json::json!({"private-payload-marker":"private-value-marker"}),
        timeout_ms: 1000,
    };
    caller
        .submit(operation(17, "private-method-marker"))
        .unwrap();
    let Some(Delivery::Call { ticket, .. }) = provider.try_recv().unwrap() else {
        panic!("call not routed");
    };
    provider
        .submit(Operation::Reply {
            ticket,
            result: Outcome::error("private-code-marker", "private-error-marker"),
        })
        .unwrap();
    assert!(matches!(
        caller.try_recv().unwrap(),
        Some(Delivery::Result { id: 17, .. })
    ));
    assert!(caller.submit(operation(18, "not-imported-marker")).is_err());
}

#[test]
fn broker_diagnostics_correlate_calls_without_carrying_private_text() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "diagnostic_broker_child", "--nocapture"])
        .env("PEERSUS_BROKER_DIAGNOSTIC_CHILD", "1")
        .env_remove("PEERSUS_LOG")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    if peersus_diagnostics::DEVELOPMENT {
        assert!(stderr.contains("broker-route") && stderr.contains("call=17"));
        assert!(stderr.contains("ticket=1") && stderr.contains("broker-reply"));
        assert!(stderr.contains("number=18") && stderr.contains("unknown-method"));
        assert!(stderr.contains("connection=") && stderr.contains("generation="));
    } else {
        assert!(!stderr.contains("broker-route") && !stderr.contains("broker-submit"));
    }
    for private in [
        "private-service-marker",
        "private-caller-marker",
        "private-provider-marker",
        "private-method-marker",
        "private-payload-marker",
        "private-value-marker",
        "private-code-marker",
        "private-error-marker",
        "not-imported-marker",
    ] {
        assert!(
            !stderr.contains(private),
            "private diagnostic content escaped"
        );
    }
}
