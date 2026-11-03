//! Proof that a failing plugin cannot harm the application.
//!
//! These tests run real processes through the real supervisor and hub. The deliberately
//! broken plugin is `crash-test-plugin`; build it with:
//!   cargo test -p peersus-plugin-host --features crash-test-plugin
//! Without that feature the tests are skipped, so a normal build never compiles a broken
//! plugin.
#![cfg(all(unix, feature = "crash-test-plugin"))]

use peersus_plugin_api::{
    hub::Hub,
    manifest::{Export, Import, Manifest},
    message::{Delivery, Operation, Outcome},
};
use peersus_plugin_host::{
    Event, Plugin, Service,
    supervisor::{Policy, Verdict},
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crash-test-plugin"))
}
fn exporting() -> Manifest {
    Manifest {
        exports: vec![Export {
            service: "svc".into(),
            version: 1,
            description: "A test service.".into(),
            consumers: names(&["app"]),
            methods: [(
                "double".into(),
                peersus_plugin_api::manifest::Member {
                    capability: None,
                    description: "Doubles a number.".into(),
                },
            )]
            .into(),
            events: Default::default(),
        }],
        ..Manifest::default()
    }
}
fn importing(from: &[&str]) -> Manifest {
    Manifest {
        imports: from
            .iter()
            .map(|provider| Import {
                provider: (*provider).into(),
                service: "svc".into(),
                version: 1,
                methods: names(&["double"]),
                events: BTreeSet::new(),
            })
            .collect(),
        ..Manifest::default()
    }
}
fn fast() -> Policy {
    Policy {
        first_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(200),
        max_failures: 3,
        window: Duration::from_secs(60),
        healthy_after: Duration::from_secs(10),
    }
}
fn plugin(id: &str, mode: &str, directory: &Path) -> Plugin {
    Plugin {
        id: id.into(),
        manifest: exporting(),
        grants: BTreeSet::new(),
        program: Some(binary()),
        arguments: vec![mode.into()],
        directory: directory.to_path_buf(),
        policy: fast(),
        handoff: None,
    }
}
/// Short private directory: a Unix socket path has a small length limit.
fn private_directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("pp")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}
async fn wait_for(
    events: &mut tokio::sync::watch::Receiver<Event>,
    what: impl Fn(&Event) -> bool,
) -> Event {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = events.borrow_and_update().clone();
            if what(&current) {
                return current;
            }
            events.changed().await.unwrap();
        }
    })
    .await
    .expect("expected plugin event never happened")
}
/// True once the plugin has been launched, whether or not it has connected since.
///
/// Health is published on a channel that keeps only the latest value, and a healthy plugin
/// connects within milliseconds, so `Started` can be replaced by `Connected` before an
/// observer ever sees it. What is guaranteed is "at least launched", not that each step is
/// seen. A plugin that cannot connect never moves past `Started`, and tests about that
/// compare against `Started` exactly instead of using this.
fn launched(event: &Event) -> bool {
    matches!(event, Event::Started | Event::Connected)
}
/// Ask `provider` to double 21 and return what the application observes.
async fn call(
    hub: &Hub,
    app: &peersus_plugin_api::hub::Channel,
    provider: &str,
    id: u64,
) -> Delivery {
    app.submit(Operation::Call {
        id,
        provider: provider.into(),
        service: "svc".into(),
        version: 1,
        method: "double".into(),
        args: json!(21),
        timeout_ms: 5000,
    })
    .unwrap();
    let _ = hub;
    tokio::time::timeout(Duration::from_secs(10), app.receive())
        .await
        .expect("the application never heard back")
        .unwrap()
}
fn alive(pid: i32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z "))
            .unwrap_or(false)
}

/// Boot an application with one healthy plugin and `mode` as a second, failing one.
async fn run(
    mode: &str,
) -> (
    Hub,
    peersus_plugin_api::hub::Channel,
    Service,
    Service,
    tokio::sync::watch::Receiver<Event>,
    tokio::sync::watch::Receiver<Event>,
    tempfile::TempDir,
) {
    let directory = private_directory();
    let hub = Hub::default();
    let generation = hub
        .register("app", importing(&["good", "bad"]), BTreeSet::new())
        .unwrap();
    let app = hub.connect("app", generation).unwrap();
    let good = Service::start(hub.clone(), plugin("good", "ok", directory.path())).unwrap();
    let bad = Service::start(hub.clone(), plugin("bad", mode, directory.path())).unwrap();
    let good_events = good.events();
    let bad_events = bad.events();
    (hub, app, good, bad, good_events, bad_events, directory)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_plugin_answers_through_the_real_process_and_hub() {
    let (hub, app, good, bad, mut good_events, _bad_events, _directory) = run("exit").await;
    wait_for(&mut good_events, launched).await;
    // Wait until the plugin has connected: the hub only routes to a live connection.
    let reply = loop {
        match call(&hub, &app, "good", 1).await {
            Delivery::Result {
                result: Outcome::Error { code, .. },
                ..
            } if code == "unavailable" => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => break other,
        }
    };
    assert_eq!(
        reply,
        Delivery::Result {
            id: 1,
            result: Outcome::ok(json!(42))
        }
    );
    good.shutdown().await;
    bad.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_aborts_is_restarted_then_given_up_on_and_nothing_else_is_affected() {
    let (hub, app, good, bad, mut good_events, mut bad_events, _directory) = run("abort").await;
    // It really did start and really did die: the test would pass vacuously otherwise.
    wait_for(&mut bad_events, launched).await;
    wait_for(&mut bad_events, |e| matches!(e, Event::Failed { .. })).await;
    // It keeps failing, so the supervisor stops trying instead of looping forever.
    wait_for(&mut bad_events, |e| *e == Event::GaveUp).await;
    // The application and the healthy plugin never noticed.
    wait_for(&mut good_events, launched).await;
    let reply = loop {
        match call(&hub, &app, "good", 7).await {
            Delivery::Result {
                result: Outcome::Error { code, .. },
                ..
            } if code == "unavailable" => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => break other,
        }
    };
    assert_eq!(
        reply,
        Delivery::Result {
            id: 7,
            result: Outcome::ok(json!(42))
        }
    );
    // Calling the dead plugin reports unavailability promptly; it never hangs the caller.
    app.submit(Operation::Call {
        id: 8,
        provider: "bad".into(),
        service: "svc".into(),
        version: 1,
        method: "double".into(),
        args: json!(1),
        timeout_ms: 5000,
    })
    .expect_err("a provider that gave up cannot be called");
    good.shutdown().await;
    bad.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_exits_with_an_error_is_contained_the_same_way() {
    let (_hub, _app, good, bad, _good_events, mut bad_events, _directory) = run("exit").await;
    wait_for(&mut bad_events, |e| matches!(e, Event::Failed { .. })).await;
    wait_for(&mut bad_events, |e| *e == Event::GaveUp).await;
    good.shutdown().await;
    bad.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_hangs_is_stopped_from_outside_and_the_application_stays_responsive() {
    let (hub, app, good, bad, mut good_events, mut bad_events, _directory) = run("hang").await;
    wait_for(&mut bad_events, launched).await;
    // The hung plugin never answers, yet the application keeps serving the healthy one.
    wait_for(&mut good_events, launched).await;
    let reply = loop {
        match call(&hub, &app, "good", 3).await {
            Delivery::Result {
                result: Outcome::Error { code, .. },
                ..
            } if code == "unavailable" => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => break other,
        }
    };
    assert_eq!(
        reply,
        Delivery::Result {
            id: 3,
            result: Outcome::ok(json!(42))
        }
    );
    // A call to the hung plugin ends with a timeout result instead of blocking forever.
    let started = Instant::now();
    app.submit(Operation::Call {
        id: 4,
        provider: "bad".into(),
        service: "svc".into(),
        version: 1,
        method: "double".into(),
        args: json!(1),
        timeout_ms: 300,
    })
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), app.receive())
        .await
        .expect("a hung plugin must not hang its caller")
        .unwrap();
    assert!(matches!(
        outcome,
        Delivery::Result { result: Outcome::Error { code, .. }, .. } if code == "timeout"
    ));
    assert!(started.elapsed() < Duration::from_secs(4));
    // Shutdown ends the hung process even though it ignores everything.
    tokio::time::timeout(Duration::from_secs(10), bad.shutdown())
        .await
        .expect("shutdown must not wait for a hung plugin");
    good.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_plugin_also_ends_the_children_it_started() {
    let directory = private_directory();
    let pid_file = directory.path().join("grandchild.pid");
    let hub = Hub::default();
    // The plugin writes its grandchild's pid to a file whose path it receives as an
    // argument, so the test never has to modify its own environment.
    let mut orphaning = plugin("orphan", "orphan", directory.path());
    orphaning.arguments = vec!["orphan".into(), pid_file.display().to_string()];
    let service = Service::start(hub.clone(), orphaning).unwrap();
    let mut events = service.events();
    wait_for(&mut events, launched).await;
    let grandchild: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            break pid;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        alive(grandchild),
        "the grandchild must really be running before the test"
    );
    service.shutdown().await;
    // Killing only the leader would leave `sleep` running; the group kill must not.
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(grandchild) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(grandchild), "a child of the plugin outlived it");
}

/// Distinct process ids the plugin has recorded, in the order it started.
fn pids(log: &Path) -> Vec<u32> {
    std::fs::read_to_string(log)
        .map(|text| text.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

async fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "the expected state never came");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_requested_restart_runs_a_new_process_and_the_plugin_comes_back_usable() {
    let directory = private_directory();
    let log = directory.path().join("pids");
    let hub = Hub::default();
    let mut healthy = plugin("alpha", "ok", directory.path());
    healthy.arguments = vec!["ok".into(), log.display().to_string()];
    let service = Service::start(hub, healthy).unwrap();
    let mut events = service.events();
    wait_for(&mut events, |e| *e == Event::Connected).await;
    wait_until(|| pids(&log).len() == 1).await;
    let first = pids(&log)[0];

    assert!(
        service.restart(),
        "a running service accepts a restart request"
    );
    wait_until(|| pids(&log).len() == 2).await;
    let second = pids(&log)[1];
    assert_ne!(first, second, "a restart must run a different process");
    wait_for(&mut events, |e| *e == Event::Connected).await;
    assert!(!alive(first as i32), "the old process must be gone");
    service.shutdown().await;
}

/// A call from `app` waiting in `alpha`'s queue, then an immediate restart request.
async fn queue_a_call_then_restart(mode_args: Vec<String>) -> (Service, tempfile::TempDir) {
    let directory = private_directory();
    let hub = Hub::default();
    let generation = hub
        .register("app", importing(&["alpha"]), BTreeSet::new())
        .unwrap();
    let app = hub.connect("app", generation).unwrap();
    let mut slow = plugin("alpha", "ack", directory.path());
    slow.arguments = mode_args;
    let service = Service::start(hub, slow).unwrap();
    let mut events = service.events();
    wait_for(&mut events, |e| *e == Event::Connected).await;
    app.submit(Operation::Call {
        id: 1,
        provider: "alpha".into(),
        service: "svc".into(),
        version: 1,
        method: "double".into(),
        args: json!(1),
        timeout_ms: 20_000,
    })
    .unwrap();
    assert!(service.restart());
    // `app` must stay alive for the call to stay queued; the caller keeps it.
    std::mem::forget(app);
    (service, directory)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_waiting_for_the_plugin_still_reaches_it_when_a_restart_is_requested() {
    let directory = private_directory();
    let acked = directory.path().join("acked");
    let (service, _held) =
        queue_a_call_then_restart(vec!["ack".into(), acked.display().to_string()]).await;
    // The plugin reads slowly, so a stop that did not wait for what was already on its way
    // would end it before it read anything, every time.
    wait_until(|| acked.exists()).await;
    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wait_before_a_requested_restart_is_bounded_so_a_plugin_that_ignores_it_is_still_stopped()
 {
    let directory = private_directory();
    let log = directory.path().join("pids");
    let hub = Hub::default();
    // `hang` never reads and never exits: only the cap can end the wait.
    let mut stuck = plugin("alpha", "hang", directory.path());
    stuck.arguments = vec!["hang".into(), log.display().to_string()];
    let service = Service::start(hub, stuck).unwrap();
    let mut events = service.events();
    wait_for(&mut events, |e| *e == Event::Connected).await;
    wait_until(|| pids(&log).len() == 1).await;
    let first = pids(&log)[0];
    let asked = Instant::now();
    assert!(service.restart());
    // The restart does happen, and a new process is what shows it: an event can be replaced
    // by the next before anyone sees it, a process id in a log cannot.
    wait_until(|| pids(&log).len() == 2).await;
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "a plugin that ignored the request held the restart for {:?}",
        asked.elapsed()
    );
    assert!(!alive(first as i32), "the old process outlived the window");
    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_leaves_by_itself_when_told_to_restart_is_restarted_and_never_counted_failed()
{
    let directory = private_directory();
    let log = directory.path().join("pids");
    let hub = Hub::default();
    let generation = hub
        .register("app", importing(&["alpha"]), BTreeSet::new())
        .unwrap();
    let app = hub.connect("app", generation).unwrap();
    let mut polite = plugin("alpha", "pidlog-exit", directory.path());
    polite.arguments = vec!["pidlog-exit".into(), log.display().to_string()];
    let service = Service::start(hub, polite).unwrap();
    let mut events = service.events();
    // Four rounds: the fast policy gives up after three failures inside a minute, so if
    // leaving in answer to a restart counted as one, the plugin would be given up on before
    // the last round. Giving up is final, so it cannot be missed the way a passing state can.
    for round in 1..=4usize {
        wait_until(|| pids(&log).len() == round).await;
        wait_for(&mut events, |e| *e == Event::Connected).await;
        assert!(service.restart());
        // The plugin is waiting on a delivery; hand it one, so it leaves in the window.
        app.submit(Operation::Call {
            id: round as u64,
            provider: "alpha".into(),
            service: "svc".into(),
            version: 1,
            method: "double".into(),
            args: json!(1),
            timeout_ms: 20_000,
        })
        .unwrap();
        wait_until(|| pids(&log).len() == round + 1).await;
    }
    // Still supervised and coming back: a plugin that was given up on never connects again.
    wait_for(&mut events, |e| *e == Event::Connected).await;
    assert!(
        !matches!(*events.borrow(), Event::GaveUp),
        "the plugin was given up on after polite restarts"
    );
    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_on_purpose_never_uses_up_the_budget_meant_for_real_failures() {
    let directory = private_directory();
    let log = directory.path().join("pids");
    let hub = Hub::default();
    let mut healthy = plugin("alpha", "ok", directory.path());
    healthy.arguments = vec!["ok".into(), log.display().to_string()];
    // The fast policy gives up after three failures inside a minute. Ten deliberate restarts
    // are far more than that, and none of them may count, or a user changing a few settings
    // would switch their plugin off.
    let service = Service::start(hub, healthy).unwrap();
    let mut events = service.events();
    wait_for(&mut events, |e| *e == Event::Connected).await;
    for round in 1..=10usize {
        wait_until(|| pids(&log).len() == round).await;
        assert!(service.restart());
        wait_until(|| pids(&log).len() == round + 1).await;
    }
    wait_for(&mut events, |e| *e == Event::Connected).await;
    assert_ne!(
        *events.borrow(),
        Event::GaveUp,
        "deliberate restarts were counted as failures"
    );
    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_plugin_is_reported_as_connected_after_it_is_launched() {
    let (_hub, _app, good, bad, mut good_events, _bad_events, _directory) = run("exit").await;
    wait_for(&mut good_events, launched).await;
    // Launched is not the same as usable; the second report is the one that means the
    // hub can reach it.
    wait_for(&mut good_events, |e| *e == Event::Connected).await;
    good.shutdown().await;
    bad.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_never_connects_is_launched_but_never_reported_as_connected() {
    let (_hub, _app, good, bad, _good_events, mut bad_events, _directory) =
        run("never-connect").await;
    wait_for(&mut bad_events, |e| *e == Event::Started).await;
    // Give it far longer than a healthy plugin needs to connect, then check it did not.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        *bad_events.borrow(),
        Event::Started,
        "a process that never said hello must not be reported as connected"
    );
    bad.shutdown().await;
    good.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_cannot_even_be_launched_gives_up_instead_of_looking_stopped() {
    let directory = private_directory();
    let hub = Hub::default();
    let mut missing = plugin("missing", "ok", directory.path());
    missing.program = Some(PathBuf::from("/nonexistent/peersus-plugin"));
    let service = Service::start(hub, missing).unwrap();
    let mut events = service.events();
    // A bad program path is a failure with a restart schedule, ending in GaveUp, never
    // silently reported as a plugin the user stopped.
    wait_for(&mut events, |e| matches!(e, Event::Failed { .. })).await;
    wait_for(&mut events, |e| *e == Event::GaveUp).await;
    service.shutdown().await;
    let _ = Verdict::GiveUp;
}

/// An application that calls `plugin_id` (running `mode`) and a channel on which every
/// accepted data connection is handed over, as the real application receives them.
async fn with_data_plugin(
    mode: &str,
) -> (
    Hub,
    peersus_plugin_api::hub::Channel,
    Service,
    tokio::sync::mpsc::Receiver<peersus_plugin_host::Accepted>,
    tempfile::TempDir,
) {
    let directory = private_directory();
    let hub = Hub::default();
    let generation = hub
        .register("app", importing(&["data"]), BTreeSet::new())
        .unwrap();
    let app = hub.connect("app", generation).unwrap();
    let (handoff, accepted) = tokio::sync::mpsc::channel(8);
    let mut data = plugin("data", mode, directory.path());
    data.handoff = Some(handoff);
    let service = Service::start(hub.clone(), data).unwrap();
    (hub, app, service, accepted, directory)
}

/// Call the plugin with `text` as its argument, retrying while it is still connecting, and
/// return the id the call was made under.
async fn call_with(app: &peersus_plugin_api::hub::Channel, id: u64, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let sent = app.submit(Operation::Call {
            id,
            provider: "data".into(),
            service: "svc".into(),
            version: 1,
            method: "double".into(),
            args: json!(text),
            timeout_ms: 5000,
        });
        if sent.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the plugin never became callable"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_sent_over_a_plugins_data_connection_arrive_bound_to_the_call_that_asked() {
    use tokio::io::AsyncReadExt;
    let (_hub, app, service, mut accepted, _directory) = with_data_plugin("data").await;
    // The id the application chose is deliberately not a small number the hub could also
    // have produced for a ticket, so the two cannot be confused.
    call_with(&app, 77, "hello over the data connection").await;
    let mut connection = tokio::time::timeout(Duration::from_secs(10), accepted.recv())
        .await
        .expect("no data connection was handed over")
        .expect("the handoff closed");
    // The connection says which plugin presented it, and that is the channel's owner.
    assert_eq!(connection.plugin, "data");
    let peersus_plugin_host::data::Right::Hub(right) = &connection.right else {
        panic!("a hub ticket must arrive as the hub's right")
    };
    assert_eq!(right.caller(), "app");
    assert_eq!(
        right.call_id(),
        77,
        "the connection must be tied to the number the caller gave its call"
    );
    let mut received = String::new();
    connection
        .stream
        .read_to_string(&mut received)
        .await
        .unwrap();
    assert_eq!(received, "hello over the data connection");
    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_with_a_made_up_token_gets_no_data_connection_through() {
    let (_hub, app, service, mut accepted, _directory) = with_data_plugin("data-wrong-token").await;
    call_with(&app, 5, "must never arrive").await;
    // The call ends one of two ways. A refused data connection is closed by the host at
    // once, so the plugin finishes waiting and answers `Ok`. An accepted one stays open
    // until somebody reads it, so the plugin waits and the call ends in a timeout. Both are
    // legitimate results of the call; what this test is about is only what it left behind.
    let ended = tokio::time::timeout(Duration::from_secs(10), app.receive())
        .await
        .expect("the call never ended")
        .unwrap();
    assert!(matches!(ended, Delivery::Result { .. }));
    // By now the host has decided about the data connection (the plugin does not answer
    // before it has, and a plugin stuck on a connection that was accepted is what makes the
    // call time out). A connection that was handed over is therefore already in the queue,
    // so no waiting is needed and none is done: this is a plain look at the queue.
    assert!(
        accepted.try_recv().is_err(),
        "a connection with a wrong token was handed over"
    );
    // And the plugin really was refused rather than merely slow: it received the answer
    // that a refused connection produces.
    assert!(
        matches!(
            ended,
            Delivery::Result {
                result: Outcome::Ok { .. },
                ..
            }
        ),
        "the plugin should have been turned away promptly, not left waiting: {ended:?}"
    );
    service.shutdown().await;
}
