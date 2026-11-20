use super::*;
use peersus_api::{manifest::Import, message::Outcome};
use serde_json::{Value, json};

const QUERIES: &[(&str, &str)] = &[
    (CONFIG_SERVICE, "get"),
    (EXTENSION_SERVICE, "tos"),
    (EXTENSION_SERVICE, "outside_store"),
    (PLUGINS_SERVICE, "list"),
    (PLATFORM_SERVICE, "inspect"),
    (
        crate::status_service::STATUS_SERVICE,
        crate::status_service::STATUS_OWN,
    ),
    (
        crate::status_service::STATUS_SERVICE,
        crate::status_service::STATUS_LIST,
    ),
    (
        crate::transfer_events::TRANSFERS_SERVICE,
        crate::transfer_events::TRANSFERS_ACTIVE,
    ),
];

fn caller_spec(index: usize) -> PluginSpec {
    let mut spec = plugin_with_guarded_action();
    spec.id = PluginId::new(format!("poll-{index}")).unwrap();
    let service = format!("polled-{index}");
    spec.manifest.exports[0].service = service.clone();
    spec.manifest.contributions.actions[0].target.service = service;
    let mut imports = std::collections::BTreeMap::<&str, BTreeSet<String>>::new();
    for &(service, method) in QUERIES
        .iter()
        .chain(&[(crate::page_offers::ACTIONS_SERVICE, "withhold")])
    {
        imports.entry(service).or_default().insert(method.into());
    }
    spec.manifest
        .imports
        .extend(imports.into_iter().map(|(service, methods)| Import {
            provider: HOST.into(),
            service: service.into(),
            version: 1,
            methods,
            events: BTreeSet::new(),
        }));
    spec
}

async fn ask(
    runtime: &mut Runtime,
    caller: &Channel,
    id: u64,
    service: &str,
    method: &str,
    args: Value,
) -> bool {
    caller
        .submit(Operation::Call {
            id,
            provider: HOST.into(),
            service: service.into(),
            version: 1,
            method: method.into(),
            args,
            timeout_ms: 5_000,
        })
        .unwrap();
    let delivery = runtime.host.try_recv().unwrap().unwrap();
    let publish = runtime.handle_host_delivery(delivery).await;
    if publish {
        runtime.publish();
    }
    assert!(
        matches!(caller.try_recv().unwrap(), Some(Delivery::Result { id: answered, result: Outcome::Ok { .. } }) if answered == id),
        "missing successful reply for {service}.{method}"
    );
    publish
}

/// Count rebuild requests, not just window updates: an equal snapshot could still have been expensive to construct.
#[tokio::test]
async fn query_burst_replies_to_each_caller_without_rebuilding_global_state() {
    let specs: Vec<_> = (0..16).map(caller_spec).collect();
    let mut registry = Registry::default();
    for spec in &specs {
        registry.register(spec.clone()).unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    let grants = Grants::new([
        permission(HOST, PLUGINS_WATCH_CAPABILITY),
        permission(HOST, PLATFORM_READ_CAPABILITY),
        permission(HOST, crate::status_service::STATUS_WATCH_CAPABILITY),
        permission(HOST, crate::transfer_events::TRANSFERS_WATCH_CAPABILITY),
    ])
    .unwrap();
    let callers: Vec<_> = specs
        .iter()
        .map(|spec| {
            let generation = runtime
                .hub
                .register(spec.id.as_str(), spec.manifest.clone(), BTreeSet::new())
                .unwrap();
            runtime
                .set_grants(spec.id.clone(), None, grants.clone())
                .unwrap();
            runtime.hub.connect(spec.id.as_str(), generation).unwrap()
        })
        .collect();
    runtime.publish();
    let mut window = client.subscribe();
    window.borrow_and_update();
    let mut rebuilds = 0;
    for round in 0..12 {
        for (index, caller) in callers.iter().enumerate() {
            let (service, method) = QUERIES[(round + index) % QUERIES.len()];
            rebuilds += usize::from(
                ask(
                    &mut runtime,
                    caller,
                    round as u64 + 1,
                    service,
                    method,
                    json!({}),
                )
                .await,
            );
        }
    }
    assert_eq!(rebuilds, 0, "global rebuilds for 192 read-only calls");
    assert!(!window.has_changed().unwrap());
    for (id, expected_change) in [(13, true), (14, false)] {
        assert_eq!(
            ask(
                &mut runtime,
                &callers[0],
                id,
                crate::page_offers::ACTIONS_SERVICE,
                "withhold",
                json!({"actions":["go"]})
            )
            .await,
            expected_change
        );
        assert_eq!(window.has_changed().unwrap(), expected_change);
        let snapshot = window.borrow_and_update();
        assert!(
            snapshot.plugins[0]
                .manifest
                .contributions
                .actions
                .is_empty()
        );
        assert!(
            snapshot.plugins[1..]
                .iter()
                .all(|p| p.manifest.contributions.actions.len() == 1)
        );
    }
}

/// The stock hello guest suffices; a probe guest may also read its own configuration before replying to `greet`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PEERSUS_GREET_MODULE and PEERSUS_WASM_HOST"]
async fn sixteen_wasm_workers_answer_concurrent_actions_through_the_owner() {
    use peersus_core::model::{ActionState, PluginStatus};
    use std::time::{Duration, Instant};
    const COUNT: usize = 16;
    let module = std::path::PathBuf::from(std::env::var_os("PEERSUS_GREET_MODULE").unwrap())
        .canonicalize()
        .unwrap();
    let host = std::path::PathBuf::from(std::env::var_os("PEERSUS_WASM_HOST").unwrap())
        .canonicalize()
        .unwrap();
    let mut registry = Registry::default();
    let mut ids = Vec::new();
    for index in 0..COUNT {
        let mut spec = caller_spec(index);
        spec.manifest.execution = peersus_api::manifest::Execution::Wasm { abi: 1 };
        spec.manifest
            .imports
            .retain(|i| i.service == CONFIG_SERVICE);
        spec.manifest.exports[0].methods = [("greet".into(), member(None))].into();
        let action = &mut spec.manifest.contributions.actions[0];
        action.target.method = "greet".into();
        action.capability = None;
        spec.program = Some(module.clone());
        ids.push(spec.id.clone());
        registry.register(spec).unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let (runtime, client) = loopback_runtime(root.path(), registry).await;
    let modules = peersus_plugin::module::DesktopModules::beside(&host).unwrap();
    let owner = tokio::spawn(runtime.with_modules(std::sync::Arc::new(modules)).run());
    let workload = tokio::time::timeout(Duration::from_secs(45), async {
        let mut window = client.subscribe();
        window
            .wait_for(|s| {
                s.plugins.len() == COUNT
                    && s.plugins.iter().all(|p| p.status == PluginStatus::Running)
            })
            .await
            .map_err(|_| "owner stopped before workers were ready")?;
        let mut elapsed = Vec::new();
        for _ in 0..6 {
            let at = Instant::now();
            let mut actions = tokio::task::JoinSet::new();
            for plugin in ids.iter().cloned() {
                let client = client.clone();
                actions.spawn(async move {
                    client
                        .command(Command::InvokeAction {
                            plugin,
                            action: "go".into(),
                            peer: None,
                        })
                        .await
                });
            }
            while let Some(reply) = actions.join_next().await {
                if !matches!(reply, Ok(Ok(Reply::Applied))) {
                    return Err("a worker action was not admitted");
                }
            }
            let finished = window
                .wait_for(|s| {
                    s.actions.len() == COUNT
                        && s.actions.iter().all(|a| a.state != ActionState::Running)
                })
                .await
                .map_err(|_| "owner stopped during actions")?;
            if !finished
                .actions
                .iter()
                .all(|a| a.state == ActionState::Done)
            {
                return Err("a worker action failed");
            }
            elapsed.push(at.elapsed());
        }
        Ok(elapsed)
    })
    .await;
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
    eprintln!(
        "Six batches of {COUNT} independent workers: {:?}",
        workload.unwrap().unwrap()
    );
}
