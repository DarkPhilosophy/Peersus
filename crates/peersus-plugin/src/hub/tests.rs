//! Behavior tests drive real channels through the real hub. Providers answer calls the
//! way a transport would, so every assertion is about what a participant can observe.
use super::*;
use peersus_api::manifest::{Export, Import, Member};
use serde_json::{Value, json};

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

/// Capabilities, each described: a name alone is refused.
fn capabilities(values: &[&str]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|c| ((*c).to_owned(), format!("Lets the plugin use {c}.")))
        .collect()
}

/// A member that needs `capability` (or nothing) and says what it does.
fn member(capability: Option<&str>) -> Member {
    Member {
        capability: capability.map(str::to_owned),
        description: "Does what its name says.".into(),
    }
}

/// A manifest that exports `svc` v1 with method `act` (capability `cap`) and event `changed`.
fn provider(consumers: &[&str]) -> Manifest {
    Manifest {
        capabilities: capabilities(&["cap"]),
        exports: vec![Export {
            service: "svc".into(),
            version: 1,
            description: "A test service.".into(),
            consumers: names(consumers),
            methods: [
                ("act".into(), member(Some("cap"))),
                ("open".into(), member(None)),
            ]
            .into(),
            events: [("changed".into(), member(Some("cap")))].into(),
        }],
        ..Manifest::new(peersus_api::manifest::Execution::Process)
    }
}
/// A manifest that imports `svc` from `from`.
fn consumer(from: &str) -> Manifest {
    Manifest {
        capabilities: capabilities(&["cap"]),
        imports: vec![Import {
            provider: from.into(),
            service: "svc".into(),
            version: 1,
            methods: names(&["act", "open"]),
            events: names(&["changed"]),
        }],
        ..Manifest::new(peersus_api::manifest::Execution::Process)
    }
}
/// Register and connect `owner`, granting the plain names in `grants` as permissions of the
/// provider it imports (every fixture here imports exactly one), which is what a grant means:
/// a permission of whoever requires it.
fn join(hub: &Hub, owner: &str, manifest: Manifest, grants: &[&str]) -> Channel {
    let permissions = grants_of(&manifest, grants);
    let generation = hub.register(owner, manifest, permissions).unwrap();
    hub.connect(owner, generation).unwrap()
}
/// The permissions `names` stand for when granted to a participant with `manifest`: each is a
/// permission of the single provider that manifest imports. A participant that imports
/// nobody can be granted nothing, and `names` must then be empty.
fn grants_of(manifest: &Manifest, names: &[&str]) -> BTreeSet<Permission> {
    let provider = manifest.imports.first().map(|i| i.provider.clone());
    names
        .iter()
        .map(|name| {
            Permission::new(
                provider
                    .clone()
                    .expect("a fixture that grants imports a provider"),
                *name,
            )
            .unwrap()
        })
        .collect()
}
fn call(id: u64, provider: &str, method: &str) -> Operation {
    Operation::Call {
        id,
        provider: provider.into(),
        service: "svc".into(),
        version: 1,
        method: method.into(),
        args: json!([2, 3]),
        timeout_ms: 1000,
    }
}

/// What a call refused only for want of the user's grant reports: the permission the
/// provider's own member names, owned by the provider, whatever else the caller holds.
fn missing(owner: &str, name: &str) -> Result<()> {
    Err(Error::MissingPermission(provider_permission(owner, name)))
}

#[test]
fn a_guest_reply_has_the_same_neutral_outcome_for_any_foreign_permission_name() {
    let hub = Hub::default();
    let _other = join(&hub, "other", provider(&[]), &[]);
    let mut own = provider(&["self"]);
    own.imports = consumer("self").imports;
    let requester = join(&hub, "self", own, &[]);
    for (id, name) in [(1, "cap"), (2, "not-declared")] {
        requester.submit(call(id, "self", "open")).unwrap();
        let Some(Delivery::Call { ticket, .. }) = requester.try_recv().unwrap() else {
            panic!("the self-call's open method was not delivered");
        };
        requester
            .submit(Operation::Reply {
                ticket,
                result: Outcome::MissingPermission {
                    permission: Permission::new("other", name).unwrap(),
                },
            })
            .unwrap();
        assert_eq!(
            requester.try_recv().unwrap(),
            Some(Delivery::Result {
                id,
                result: Outcome::NotAccepted {}
            })
        );
    }
}
fn publish() -> Operation {
    Operation::Publish {
        service: "svc".into(),
        version: 1,
        event: "changed".into(),
        data: json!(5),
    }
}
fn subscribe(provider: &str) -> Operation {
    Operation::Subscribe {
        provider: provider.into(),
        service: "svc".into(),
        version: 1,
        event: "changed".into(),
    }
}
fn ticket(delivery: Option<Delivery>) -> u64 {
    match delivery {
        Some(Delivery::Call { ticket, .. }) => ticket,
        other => panic!("expected a call, got {other:?}"),
    }
}
fn reply(channel: &Channel, ticket: u64, value: Value) {
    channel
        .submit(Operation::Reply {
            ticket,
            result: Outcome::ok(value),
        })
        .unwrap();
}
fn error_code(delivery: Option<Delivery>) -> String {
    match delivery {
        Some(Delivery::Result {
            result: Outcome::Error { code, .. },
            ..
        }) => code,
        other => panic!("expected an error result, got {other:?}"),
    }
}

/// `b` exports to `a`; `a` imports from `b` and the user granted `a` the capability.
fn pair() -> (Hub, Channel, Channel) {
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    (hub, a, b)
}

#[test]
fn plugin_calls_plugin_through_the_hub_and_the_provider_sees_who_asked() {
    let (_hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    let Some(Delivery::Call {
        ticket,
        caller,
        args,
        ..
    }) = b.try_recv().unwrap()
    else {
        panic!("provider received no call")
    };
    // The hub stamped the caller from the channel; the message could not carry one.
    assert_eq!(caller, "a");
    assert_eq!(args, json!([2, 3]));
    reply(&b, ticket, json!(5));
    assert_eq!(
        a.try_recv().unwrap(),
        Some(Delivery::Result {
            id: 1,
            result: Outcome::ok(json!(5))
        })
    );
}

#[test]
fn a_private_export_is_unreachable_even_with_import_and_user_grant() {
    // Exporting is the author's choice: with no consumers listed the API is private.
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&[]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    assert_eq!(a.submit(call(1, "b", "act")), Err(Error::Denied));
    assert_eq!(a.submit(call(2, "b", "open")), Err(Error::Denied));
    assert!(
        b.try_recv().unwrap().is_none(),
        "nothing reached the provider"
    );
}

#[test]
fn call_refusals_name_the_providers_missing_permission_without_delivering_the_call() {
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &[]);
    let refused = a.submit(call(1, "b", "act")).unwrap_err();
    a.reject(1, &refused).unwrap();
    assert_eq!(
        serde_json::to_value(a.try_recv().unwrap().unwrap()).unwrap(),
        json!({
            "type":"result", "id": 1,
            "result":{"status":"missing-permission","permission":{"owner":"b","name":"cap"}}
        })
    );
    assert!(b.try_recv().unwrap().is_none());
}

#[test]
fn only_a_caller_that_may_call_the_member_is_told_which_permission_it_lacks() {
    // None of these callers holds a grant. A hub that looked at the grant before the route
    // would answer every one with "missing permission b.cap", and teach callers that may not
    // use the member at all the name of its guard. Only the caller that imports the member
    // and is listed for it is told; every other refusal stays a plain one.
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&["listed", "partial", "stranger"]), &[]);
    let listed = join(&hub, "listed", consumer("b"), &[]);
    // Imports the service, but only the member that needs nothing.
    let mut only_open = consumer("b");
    only_open.imports[0].methods = names(&["open"]);
    only_open.imports[0].events = BTreeSet::new();
    let partial = join(&hub, "partial", only_open, &[]);
    // Imports nothing from `b`.
    let stranger = join(
        &hub,
        "stranger",
        Manifest::new(peersus_api::manifest::Execution::Process),
        &[],
    );
    // Imports everything, but `b` does not list it.
    let unlisted = join(&hub, "unlisted", consumer("b"), &[]);
    for (who, caller) in [
        ("partial", &partial),
        ("stranger", &stranger),
        ("unlisted", &unlisted),
    ] {
        assert_eq!(
            caller.submit(call(1, "b", "act")),
            Err(Error::Denied),
            "{who} called a guarded method"
        );
        assert_eq!(
            caller.submit(subscribe("b")),
            Err(Error::Denied),
            "{who} subscribed to a guarded event"
        );
    }
    // The same method and the same missing grant: only the listed importer is told.
    assert_eq!(listed.submit(call(1, "b", "act")), missing("b", "cap"));
    assert_eq!(listed.submit(subscribe("b")), missing("b", "cap"));
    // What the application is told to warn about follows the same line.
    assert_eq!(
        lock(&hub.0).missing_permissions,
        BTreeSet::from([("listed".to_owned(), provider_permission("b", "cap"))])
    );
    assert!(b.try_recv().unwrap().is_none());
}

#[test]
fn the_user_must_also_grant_the_capability_and_can_revoke_it_at_any_time() {
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &[]);
    // The author opened the API but the user has not approved it.
    assert_eq!(a.submit(call(1, "b", "act")), missing("b", "cap"));
    // A method that needs no capability is available once the author exports it.
    a.submit(call(2, "b", "open")).unwrap();
    assert!(matches!(b.try_recv().unwrap(), Some(Delivery::Call { .. })));
    hub.set_permissions("a", grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    a.submit(call(3, "b", "act")).unwrap();
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    assert_eq!(a.submit(call(4, "b", "act")), missing("b", "cap"));
    // A permission can only be granted for a provider the participant imports: `a` imports
    // `b`, so a grant of `stranger`'s permission is refused whatever its name.
    assert_eq!(
        hub.set_permissions("a", [Permission::new("stranger", "cap").unwrap()].into()),
        Err(Error::Denied)
    );
    // And the application's own permissions are always grantable, because it is the one
    // provider every participant may import.
    assert!(
        hub.set_permissions("a", [Permission::new(HOST, "cap").unwrap()].into())
            .is_ok()
    );
}

#[test]
fn a_restart_must_not_give_back_access_the_user_withdrew() {
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    // `a` is launched with the permission, exactly as the supervisor registers it.
    let launch_grants = grants_of(&consumer("b"), &["cap"]);
    let generation = hub
        .register("a", consumer("b"), launch_grants.clone())
        .unwrap();
    let a = hub.connect("a", generation).unwrap();
    a.submit(call(1, "b", "act")).unwrap();
    // The user withdraws it.
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    assert_eq!(a.submit(call(2, "b", "act")), missing("b", "cap"));
    // `a` crashes and the supervisor registers the next generation. The supervisor only
    // remembers what it was launched with, so that is what it offers.
    let generation = hub.register("a", consumer("b"), launch_grants).unwrap();
    let restarted = hub.connect("a", generation).unwrap();
    // The withdrawn access must still be gone. The missing `b.cap` specifically proves the
    // grant check ran; any other error would mean this test failed for an unrelated reason.
    assert_eq!(
        restarted.submit(call(3, "b", "act")),
        missing("b", "cap"),
        "restart restored access the user had withdrawn"
    );
}

#[test]
fn a_withdrawal_made_while_the_participant_is_between_processes_still_holds_after_it_restarts() {
    // The sequence that matters: granted, the process dies, the user withdraws the grant while
    // nothing is registered (the supervisor is waiting out its pause), and the next process is
    // started from what the supervisor remembers, which is the OLD grant.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let launch_grants = grants_of(&consumer("b"), &["cap"]);
    let generation = hub
        .register("a", consumer("b"), launch_grants.clone())
        .unwrap();
    let a = hub.connect("a", generation).unwrap();
    a.submit(call(1, "b", "act")).unwrap();
    // The process ends: the supervisor revokes the registration, and `a` is not there now.
    hub.revoke("a");
    // The user withdraws while it is not there. This must be recorded, not refused.
    hub.set_permissions("a", BTreeSet::new())
        .expect("a decision about a participant must be recordable between its processes");
    let generation = hub.register("a", consumer("b"), launch_grants).unwrap();
    let restarted = hub.connect("a", generation).unwrap();
    assert_eq!(
        restarted.submit(call(2, "b", "act")),
        missing("b", "cap"),
        "a withdrawal made between two processes was lost"
    );
}

#[test]
fn a_grant_made_while_the_participant_is_between_processes_applies_to_the_next_one() {
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let first = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let _ = hub.connect("a", first).unwrap();
    hub.revoke("a");
    hub.set_permissions("a", grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    let generation = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let next = hub.connect("a", generation).unwrap();
    assert!(next.submit(call(1, "b", "act")).is_ok());
}

#[test]
fn forgetting_a_participant_leaves_a_package_of_the_same_name_nothing_to_inherit() {
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let first = hub
        .register("a", consumer("b"), grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    let _ = hub.connect("a", first).unwrap();
    // Taking it away for good: its process goes AND what it was granted goes with it.
    hub.forget("a");
    // A different package under the same name is registered with no grants of its own.
    let generation = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let next = hub.connect("a", generation).unwrap();
    assert_eq!(
        next.submit(call(1, "b", "act")),
        missing("b", "cap"),
        "the new package inherited what the removed one was granted"
    );
}

#[test]
fn revoking_alone_still_keeps_the_grants_so_a_restarted_process_has_them() {
    // The control for the test above, and the reason `revoke` is not simply changed: a restart
    // revokes the old process and registers the new one, and must find its grants where it left
    // them. If forgetting were what revoke does, every restart would silently lose them.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let first = hub
        .register("a", consumer("b"), grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    let _ = hub.connect("a", first).unwrap();
    hub.revoke("a");
    let generation = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let next = hub.connect("a", generation).unwrap();
    assert!(next.submit(call(1, "b", "act")).is_ok());
}

#[test]
fn a_grant_recorded_between_processes_for_a_provider_the_next_one_does_not_import_is_dropped() {
    // While nothing is registered the manifest is unknown, so the static check cannot be made
    // then. It is made when the next process registers, which keeps only grants that still
    // mean something. The proof is what happens afterwards: if the grant for `c` were merely
    // set aside rather than dropped, a LATER process that does import `c` would find it
    // waiting, holding access the user never gave it for that manifest.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let _c = join(&hub, "c", provider(&["a"]), &[]);
    let first = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let _ = hub.connect("a", first).unwrap();
    hub.revoke("a");
    hub.set_permissions("a", [provider_permission("c", "cap")].into())
        .unwrap();
    // The next process imports only `b`: the grant for `c` means nothing and is dropped.
    let second = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let _ = hub.connect("a", second).unwrap();
    hub.revoke("a");
    // A later process imports `c` too, offered nothing at launch.
    let mut both = consumer("b");
    both.imports.push(consumer("c").imports.remove(0));
    let third = hub.register("a", both, BTreeSet::new()).unwrap();
    let a = hub.connect("a", third).unwrap();
    assert_eq!(
        a.submit(call(1, "c", "act")),
        missing("c", "cap"),
        "a grant for a provider that a process did not import came back later"
    );
}

/// A grant naming the provider whose permission it is, as the user gives one.
fn provider_permission(owner: &str, name: &str) -> Permission {
    Permission::new(owner, name).unwrap()
}

#[test]
fn a_participant_cannot_even_be_granted_a_permission_in_its_own_name_space() {
    // First barrier, at grant time: `a` imports `b`, not itself, so the user cannot grant `a`
    // a permission OF `a`. Such a grant would mean nothing, and refusing it keeps the
    // approval screen from offering one.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let _a = join(&hub, "a", consumer("b"), &[]);
    assert_eq!(
        hub.set_permissions("a", [provider_permission("a", "cap")].into()),
        Err(Error::Denied)
    );
}

#[test]
fn a_permission_of_a_third_party_with_the_same_name_does_not_open_the_providers_guard() {
    // Second barrier, at call time, and it must hold on its own: `a` imports BOTH `b` and `c`
    // and the user grants `a` the permission `cap` of `c`. `b` guards `act` with ITS `cap`.
    // Same name, a different owner: a name alone is not whose permission it is, so this must
    // not satisfy `b`. Otherwise any plugin could pass the application's own guard on `send`
    // just by holding a permission with the same name from somewhere else.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let mut both = consumer("b");
    both.imports.push(consumer("c").imports.remove(0));
    let a = join(&hub, "a", both, &[]);
    hub.set_permissions("a", [provider_permission("c", "cap")].into())
        .expect("a permission of a provider the participant imports is storable");
    // The refusal names `b`'s permission, the one its guard needs, and not the `c` one that
    // `a` does hold.
    assert_eq!(
        a.submit(call(1, "b", "act")),
        missing("b", "cap"),
        "a permission of another owner, with the same name, satisfied the provider's guard"
    );
}

#[test]
fn a_grant_on_the_providers_own_permission_does_open_the_call() {
    // The control for the test above, and it is not the trivial kind: it grants something and
    // expects a call to GO THROUGH, so a hub that simply refuses everything fails it. It is
    // the same call, the same names, the same caller; only whose permission was granted
    // differs.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &[]);
    hub.set_permissions("a", [provider_permission("b", "cap")].into())
        .unwrap();
    assert_eq!(a.submit(call(1, "b", "act")), Ok(()));
}

#[test]
fn a_grant_given_before_the_provider_exists_applies_once_it_does() {
    // The application sets what it may call before any plugin has started, so a grant for a
    // provider that is not registered yet must be storable. If it were refused, the
    // application's own grants would all be refused at start and everything guarded would be
    // denied forever.
    let hub = Hub::default();
    let a = join(&hub, "a", consumer("b"), &[]);
    hub.set_permissions("a", [provider_permission("b", "cap")].into())
        .unwrap();
    assert_eq!(
        a.submit(call(1, "b", "act")),
        Err(Error::UnknownProvider),
        "nobody is there to call yet"
    );
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    assert_eq!(a.submit(call(2, "b", "act")), Ok(()));
}

#[test]
fn a_grant_to_a_provider_the_caller_does_not_import_is_refused() {
    // The only static check: a participant can be granted permissions of the providers it
    // actually imports, and nobody else's. Whether the provider declares that permission is
    // checked when a call is made, because only then is the provider known to be there.
    let hub = Hub::default();
    let _a = join(&hub, "a", consumer("b"), &[]);
    assert!(
        hub.set_permissions("a", [provider_permission("stranger", "cap")].into())
            .is_err(),
        "a grant for a provider the caller never imports was accepted"
    );
}

#[test]
fn a_larger_manifest_after_withdrawal_does_not_reopen_access_and_new_capabilities_start_denied() {
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let generation = hub
        .register("a", consumer("b"), grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    let _first = hub.connect("a", generation).unwrap();
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    // A new version of `a` declares an extra capability and its launcher offers both.
    let mut bigger = consumer("b");
    bigger.capabilities = capabilities(&["cap", "more"]);
    let offered = grants_of(&bigger, &["cap", "more"]);
    let generation = hub.register("a", bigger, offered).unwrap();
    let restarted = hub.connect("a", generation).unwrap();
    // The user's earlier decision stands: nothing is granted, including the new one.
    assert_eq!(restarted.submit(call(1, "b", "act")), missing("b", "cap"));
    // The new capability only becomes usable through an explicit grant.
    hub.set_permissions("a", grants_of(&consumer("b"), &["cap", "more"]))
        .unwrap();
    restarted.submit(call(2, "b", "act")).unwrap();
}

#[test]
fn a_different_launch_baseline_never_overrides_a_decision_the_user_already_made() {
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let cap = || grants_of(&consumer("b"), &["cap"]);
    // First launch with no baseline grants, then the user grants access.
    let generation = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let _first = hub.connect("a", generation).unwrap();
    hub.set_permissions("a", cap()).unwrap();
    // The application's baseline later changes to nothing; a restart must keep what the
    // user granted rather than apply the new baseline.
    let generation = hub.register("a", consumer("b"), BTreeSet::new()).unwrap();
    let restarted = hub.connect("a", generation).unwrap();
    restarted.submit(call(1, "b", "act")).unwrap();
    // And the reverse: a more generous baseline cannot override a withdrawal.
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    let generation = hub.register("a", consumer("b"), cap()).unwrap();
    let again = hub.connect("a", generation).unwrap();
    assert_eq!(again.submit(call(2, "b", "act")), missing("b", "cap"));
}

#[test]
fn a_grant_the_provider_never_declared_is_stored_but_opens_nothing() {
    // The static check at grant time is only that the provider is one the participant imports.
    // Whether the provider requires a permission of that name is known when a call is made,
    // and until then a grant for a name nobody asks for is harmless: it matches no guard.
    let hub = Hub::default();
    let _b = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &[]);
    hub.set_permissions("a", [provider_permission("b", "nope")].into())
        .expect("a grant naming an imported provider is storable whatever its name");
    // The refusal names what the provider's member needs, not the unrelated name granted.
    assert_eq!(a.submit(call(1, "b", "act")), missing("b", "cap"));
}

#[test]
fn imports_must_name_the_provider_so_a_lookalike_service_gets_nothing() {
    let hub = Hub::default();
    let real = join(&hub, "real", provider(&["a"]), &[]);
    let fake = join(&hub, "fake", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("real"), &["cap"]);
    a.submit(call(1, "real", "act")).unwrap();
    assert!(matches!(
        real.try_recv().unwrap(),
        Some(Delivery::Call { .. })
    ));
    // `fake` exports the same service and lists `a`, but `a` never imported from it.
    assert_eq!(a.submit(call(2, "fake", "act")), Err(Error::Denied));
    assert!(fake.try_recv().unwrap().is_none());
}

#[test]
fn contract_violations_fail_closed_and_never_reach_the_provider() {
    let (_hub, a, b) = pair();
    let mut wrong_version = call(1, "b", "act");
    if let Operation::Call { version, .. } = &mut wrong_version {
        *version = 2;
    }
    assert_eq!(a.submit(wrong_version), Err(Error::Version));
    assert_eq!(a.submit(call(2, "b", "missing")), Err(Error::UnknownMethod));
    assert_eq!(
        a.submit(call(3, "nobody", "act")),
        Err(Error::UnknownProvider)
    );
    let mut large = call(4, "b", "act");
    if let Operation::Call { args, .. } = &mut large {
        *args = json!("x".repeat(MAX_QUEUE * 1024));
    }
    assert_eq!(a.submit(large), Err(Error::Oversize));
    assert_eq!(
        a.submit(call(0, "b", "act")),
        Err(Error::Timeout),
        "id zero is invalid"
    );
    assert!(b.try_recv().unwrap().is_none());
}

#[test]
fn a_plugin_in_the_middle_cannot_reach_what_was_not_exported_to_it() {
    // The old in-process broker decided "is this the host?" from a slot that was empty
    // while a plugin ran, so a plugin calling onward looked like the host. Identity now
    // belongs to the channel: host -> a -> b must be judged as `a`, and `a` was never
    // listed by `b`.
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&[HOST]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    let host_generation = hub
        .register_host(HOST, consumer("b"), grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    // The host really is listed and succeeds.
    host.submit(call(1, "b", "act")).unwrap();
    assert!(matches!(b.try_recv().unwrap(), Some(Delivery::Call { caller, .. }) if caller == HOST));
    // Plugin `a` is not listed, so it is refused no matter who asked `a` to run.
    assert_eq!(a.submit(call(1, "b", "act")), Err(Error::Denied));
    assert!(b.try_recv().unwrap().is_none());
}

#[test]
fn a_message_cannot_make_one_participant_speak_for_another() {
    let (_hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    // A reply for someone else's ticket is refused: only the provider it was delivered to.
    assert_eq!(
        a.submit(Operation::Reply {
            ticket: t,
            result: Outcome::ok(json!("forged"))
        }),
        Err(Error::Denied)
    );
    reply(&b, t, json!("real"));
    assert!(matches!(
        a.try_recv().unwrap(),
        Some(Delivery::Result { result: Outcome::Ok { value }, .. }) if value == json!("real")
    ));
}

#[test]
fn a_data_right_exists_only_for_a_call_the_hub_accepted_and_only_for_its_provider() {
    let (_hub, a, b) = pair();
    // No call yet: there is nothing to attach a data connection to.
    assert_eq!(b.open_data(1).err(), Some(Error::Cancelled));
    // An earlier call uses up the hub's first ticket, so the call under test gets a ticket
    // that is certainly not the id its caller chose for it.
    a.submit(call(1, "b", "act")).unwrap();
    let first = ticket(b.try_recv().unwrap());
    reply(&b, first, json!(0));
    let _ = a.try_recv().unwrap();
    a.submit(call(41, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    assert_ne!(
        t, 41,
        "the test needs a ticket that differs from the caller's id"
    );
    // The caller is not the provider of this call, so it cannot open the data side.
    assert_eq!(a.open_data(t).err(), Some(Error::Denied));
    // A ticket nobody was given is refused, not guessed at.
    assert_eq!(b.open_data(t + 1000).err(), Some(Error::Cancelled));
    // The provider the call was delivered to can.
    let right = b.open_data(t).unwrap();
    assert_eq!(right.caller(), "a");
    assert_eq!(right.ticket(), t);
    // It also names the call as the CALLER numbered it. A caller that parked something for
    // this call (such as a network stream) knows it by that number, not by the hub's ticket.
    assert_eq!(right.call_id(), 41);
}

#[test]
fn a_data_right_can_be_taken_only_once() {
    let (_hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    let _first = b.open_data(t).unwrap();
    // A second connection for the same call would be a second stream into one accepted
    // request; it is refused instead of silently multiplexed.
    assert_eq!(b.open_data(t).err(), Some(Error::Busy));
}

#[test]
fn a_data_right_does_not_survive_the_call_ending() {
    // Answered, cancelled, expired and revoked calls must all take the right with them.
    let (hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    reply(&b, t, json!(1));
    assert_eq!(b.open_data(t).err(), Some(Error::Cancelled), "answered");

    a.submit(call(2, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    a.submit(Operation::Cancel { id: 2 }).unwrap();
    assert_eq!(b.open_data(t).err(), Some(Error::Cancelled), "cancelled");
    assert_eq!(
        b.try_recv().unwrap(),
        Some(Delivery::Cancelled { ticket: t }),
        "the provider is told the call was cancelled"
    );

    a.submit(call(3, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    lock(&hub.0).expire(Instant::now() + Duration::from_secs(2));
    assert_eq!(b.open_data(t).err(), Some(Error::Cancelled), "expired");
    assert_eq!(
        b.try_recv().unwrap(),
        Some(Delivery::Cancelled { ticket: t }),
        "the provider is told the call expired"
    );

    a.submit(call(4, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    hub.revoke("a");
    assert_eq!(
        b.open_data(t).err(),
        Some(Error::Cancelled),
        "caller revoked"
    );
}

#[test]
fn a_call_past_its_deadline_yields_no_data_right_even_if_nothing_has_swept_it() {
    let (_hub, a, b) = pair();
    let mut quick = call(1, "b", "act");
    if let Operation::Call { timeout_ms, .. } = &mut quick {
        *timeout_ms = 1;
    }
    a.submit(quick).unwrap();
    let t = ticket(b.try_recv().unwrap());
    // Let the deadline really pass. Nothing here calls the sweep, so the call is still
    // recorded; only `open_data` itself can notice that it is already too late.
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(b.open_data(t).err(), Some(Error::Cancelled));
}

#[test]
fn a_restarted_provider_cannot_claim_the_data_right_of_its_previous_generation() {
    let hub = Hub::default();
    let old = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(old.try_recv().unwrap());
    // The provider crashes and the supervisor registers the next generation of it.
    let generation = hub
        .register("b", provider(&["a"]), BTreeSet::new())
        .unwrap();
    let fresh = hub.connect("b", generation).unwrap();
    // The caller was told at once that its provider went away, and the ticket is gone.
    assert_eq!(error_code(a.try_recv().unwrap()), "stopped");
    assert_eq!(fresh.open_data(t).err(), Some(Error::Cancelled));
    // The dead generation's own channel is revoked too.
    assert_eq!(old.open_data(t).err(), Some(Error::Cancelled));
}

#[test]
fn a_host_call_requires_the_generation_it_was_approved_for() {
    let hub = Hub::default();
    let _old = join(&hub, "b", provider(&[HOST]), &[]);
    let previous_generation = hub.registration_generation("b").unwrap();
    let host_generation = hub
        .register_host(HOST, consumer("b"), BTreeSet::new())
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    let generation = hub
        .register("b", provider(&[HOST]), BTreeSet::new())
        .unwrap();
    let provider = hub.connect("b", generation).unwrap();

    assert_eq!(
        host.submit_call_to_generation(previous_generation, call(1, "b", "open")),
        Err(Error::Unavailable)
    );
    // The instance it approved is gone: that is said first, before anything about the
    // guards of the one that replaced it.
    assert_eq!(
        host.submit_call_to_generation(previous_generation, call(1, "b", "act")),
        Err(Error::Unavailable)
    );
    // A matching generation is not permission to call a guarded export.
    assert_eq!(
        host.submit_call_to_generation(generation, call(1, "b", "act")),
        missing("b", "cap")
    );
    assert_eq!(provider.try_recv().unwrap(), None);
    // The refused call did not reserve the ID, and the right instance can answer it.
    host.submit_call_to_generation(generation, call(1, "b", "open"))
        .unwrap();
    reply(&provider, ticket(provider.try_recv().unwrap()), json!(5));
    assert_eq!(
        host.try_recv().unwrap(),
        Some(Delivery::Result {
            id: 1,
            result: Outcome::ok(json!(5))
        })
    );
}

#[test]
fn only_the_host_can_submit_a_generation_bound_call() {
    let (hub, guest, provider) = pair();
    let generation = hub.registration_generation("b").unwrap();
    assert_eq!(
        guest.submit_call_to_generation(generation, call(1, "b", "open")),
        Err(Error::Denied)
    );
    assert_eq!(provider.try_recv().unwrap(), None);
}

#[test]
fn withdrawing_the_grant_after_acceptance_does_not_cancel_a_call_already_made() {
    // Documented rule: an accepted call is a commitment made while it was allowed. Its
    // data right follows the call, so it also finishes; only NEW calls are refused.
    let (hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    assert!(b.open_data(t).is_ok(), "the accepted call still completes");
    assert_eq!(
        a.submit(call(2, "b", "act")),
        missing("b", "cap"),
        "new calls are refused"
    );
}

#[test]
fn timeouts_and_cancellation_withdraw_work_and_late_replies_are_refused() {
    let (hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    lock(&hub.0).expire(Instant::now() + Duration::from_secs(2));
    assert_eq!(error_code(a.try_recv().unwrap()), "timeout");
    assert_eq!(
        b.try_recv().unwrap(),
        Some(Delivery::Cancelled { ticket: t })
    );
    assert_eq!(
        b.submit(Operation::Reply {
            ticket: t,
            result: Outcome::ok(Value::Null)
        }),
        Err(Error::Cancelled)
    );
    // Work that never started is withdrawn without the provider ever seeing it.
    a.submit(call(2, "b", "act")).unwrap();
    a.submit(Operation::Cancel { id: 2 }).unwrap();
    assert!(b.try_recv().unwrap().is_none());
    a.submit(call(3, "b", "act")).unwrap();
    lock(&hub.0).expire(Instant::now() + Duration::from_secs(2));
    assert!(
        b.try_recv().unwrap().is_none(),
        "expired queued work never ran"
    );
    assert_eq!(error_code(a.try_recv().unwrap()), "timeout");
    // A duplicate id from one caller is refused while the first is pending.
    a.submit(call(9, "b", "act")).unwrap();
    assert_eq!(a.submit(call(9, "b", "act")), Err(Error::QueueFull));
}

#[test]
fn a_provider_that_dies_tells_callers_immediately_and_old_channels_become_unusable() {
    let (hub, a, b) = pair();
    a.submit(call(1, "b", "act")).unwrap();
    hub.revoke("b");
    assert_eq!(error_code(a.try_recv().unwrap()), "stopped");
    assert_eq!(
        b.try_recv(),
        Err(Error::Cancelled),
        "revoked channel cannot receive"
    );
    assert_eq!(b.submit(publish()), Err(Error::Cancelled), "nor send");
    assert_eq!(a.submit(call(2, "b", "act")), Err(Error::UnknownProvider));
}

#[test]
fn a_restarted_plugin_starts_clean_and_the_old_generation_cannot_interfere() {
    let hub = Hub::default();
    let old = join(&hub, "b", provider(&["a"]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    a.submit(call(1, "b", "act")).unwrap();
    // The plugin crashed and the supervisor registered a new generation.
    let generation = hub
        .register("b", provider(&["a"]), BTreeSet::new())
        .unwrap();
    assert_eq!(error_code(a.try_recv().unwrap()), "stopped");
    let fresh = hub.connect("b", generation).unwrap();
    // The stale channel was revoked and cannot acquire the new one's work.
    assert_eq!(old.try_recv(), Err(Error::Cancelled));
    drop(old);
    a.submit(call(2, "b", "act")).unwrap();
    assert!(matches!(
        fresh.try_recv().unwrap(),
        Some(Delivery::Call { .. })
    ));
    // A connection for a generation that is no longer current is refused.
    assert!(hub.connect("b", generation + 99).is_err());
}

#[test]
fn only_one_live_channel_per_participant() {
    let hub = Hub::default();
    let generation = hub.register("b", provider(&[]), BTreeSet::new()).unwrap();
    let first = hub.connect("b", generation).unwrap();
    assert!(matches!(hub.connect("b", generation), Err(Error::Busy)));
    first.disconnect();
    assert!(hub.connect("b", generation).is_ok());
}

#[test]
fn events_need_an_explicit_subscription_and_the_same_contract_checks() {
    let (hub, a, b) = pair();
    b.submit(publish()).unwrap();
    assert!(
        a.try_recv().unwrap().is_none(),
        "no automatic subscriptions"
    );
    a.submit(subscribe("b")).unwrap();
    b.submit(publish()).unwrap();
    assert!(
        matches!(a.try_recv().unwrap(), Some(Delivery::Event { data, .. }) if data == json!(5))
    );
    // A plugin may publish only what it exported.
    assert_eq!(
        b.submit(Operation::Publish {
            service: "svc".into(),
            version: 1,
            event: "private".into(),
            data: Value::Null
        }),
        Err(Error::UnknownMethod)
    );
    // Revoking the user's grant stops new subscriptions immediately.
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    assert_eq!(a.submit(subscribe("b")), missing("b", "cap"));
}

#[test]
fn withdrawing_a_grant_also_ends_existing_subscriptions_and_discards_queued_events() {
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&["a", "c"]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    let c = join(&hub, "c", consumer("b"), &["cap"]);
    a.submit(subscribe("b")).unwrap();
    c.submit(subscribe("b")).unwrap();
    b.submit(publish()).unwrap();
    // Both subscribers have one protected event waiting. The user then withdraws access
    // from `a` only.
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    assert!(
        a.try_recv().unwrap().is_none(),
        "an event queued before the withdrawal must not be delivered afterwards"
    );
    // Later events do not reach `a` either, even though it subscribed while allowed.
    b.submit(publish()).unwrap();
    assert!(a.try_recv().unwrap().is_none());
    // `c` kept its grant, so it is unaffected and still receives everything.
    assert!(matches!(
        c.try_recv().unwrap(),
        Some(Delivery::Event { .. })
    ));
    assert!(matches!(
        c.try_recv().unwrap(),
        Some(Delivery::Event { .. })
    ));
    // Granting again does not silently resurrect the old subscription: it must be asked
    // for anew.
    hub.set_permissions("a", grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    b.submit(publish()).unwrap();
    assert!(a.try_recv().unwrap().is_none());
    a.submit(subscribe("b")).unwrap();
    b.submit(publish()).unwrap();
    assert!(matches!(
        a.try_recv().unwrap(),
        Some(Delivery::Event { .. })
    ));
}

#[test]
fn a_refused_call_is_answered_with_a_terminal_result_instead_of_silence() {
    let (hub, a, _b) = pair();
    hub.set_permissions("a", BTreeSet::new()).unwrap();
    let refusal = a.submit(call(11, "b", "act")).unwrap_err();
    // A refusal is decided before the call is recorded, so no timeout would ever cover
    // it. Reporting it is the transport's job and the hub provides the means. A call closed
    // only for want of the user's grant is answered with the permission it lacks.
    a.reject(11, &refusal).unwrap();
    assert_eq!(
        a.try_recv().unwrap(),
        Some(Delivery::Result {
            id: 11,
            result: Outcome::MissingPermission {
                permission: provider_permission("b", "cap")
            }
        })
    );
    // Each other kind of refusal maps to its own recognizable code.
    for (id, error, code) in [
        (12, Error::UnknownProvider, "unavailable"),
        (13, Error::UnknownMethod, "unknown-method"),
        (14, Error::Version, "version"),
        (15, Error::Oversize, "too-large"),
        (16, Error::QueueFull, "busy"),
        (17, Error::Denied, "denied"),
    ] {
        a.reject(id, &error).unwrap();
        assert_eq!(error_code(a.try_recv().unwrap()), code, "{error:?}");
    }
}

#[test]
fn reporting_a_refusal_fails_loudly_when_the_participant_cannot_receive_it() {
    let (hub, a, _b) = pair();
    // Fill the participant's own queue, as a misbehaving plugin could.
    a.submit(subscribe("b")).unwrap();
    for _ in 0..MAX_QUEUE {
        _b.submit(publish()).unwrap();
    }
    // The answer has nowhere to go. The transport must learn this so it can drop the
    // connection rather than leave the participant waiting forever.
    assert_eq!(a.reject(1, &Error::Denied), Err(Error::QueueFull));
    // A channel that no longer exists also reports failure instead of pretending.
    hub.revoke("a");
    assert!(a.reject(2, &Error::Denied).is_err());
}

#[test]
fn a_flood_of_events_cannot_starve_a_reply_slot() {
    let (_hub, a, b) = pair();
    a.submit(subscribe("b")).unwrap();
    a.submit(call(1, "b", "act")).unwrap();
    let t = ticket(b.try_recv().unwrap());
    for _ in 0..MAX_QUEUE - 1 {
        b.submit(publish()).unwrap();
    }
    // The queue is full except for the slot reserved for the pending call's result. One
    // more event is missed by `a`; it is never put in the slot the result was promised.
    b.submit(publish()).unwrap();
    reply(&b, t, json!(5));
    for _ in 0..MAX_QUEUE - 1 {
        assert!(matches!(
            a.try_recv().unwrap(),
            Some(Delivery::Event { .. })
        ));
    }
    assert!(matches!(
        a.try_recv().unwrap(),
        Some(Delivery::Result { id: 1, .. })
    ));
}

#[test]
fn a_subscriber_that_does_not_read_costs_nobody_else_an_event() {
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&["a", "c"]), &[]);
    let a = join(&hub, "a", consumer("b"), &["cap"]);
    let c = join(&hub, "c", consumer("b"), &["cap"]);
    a.submit(subscribe("b")).unwrap();
    c.submit(subscribe("b")).unwrap();
    // `a` reads every event as it comes; `c` reads nothing until its queue is full.
    for _ in 0..MAX_QUEUE {
        b.submit(publish()).unwrap();
        assert!(matches!(
            a.try_recv().unwrap(),
            Some(Delivery::Event { .. })
        ));
    }
    // The next event is `c`'s to miss, and only `c`'s: the publisher is not refused and
    // `a` is told, for as many events as follow.
    for _ in 0..3 {
        b.submit(publish()).unwrap();
        assert!(matches!(
            a.try_recv().unwrap(),
            Some(Delivery::Event { .. })
        ));
    }
    // What `c` was already sent is still there, no more than its queue holds, and it is
    // still subscribed: once it reads, the next event reaches it again.
    let mut waiting = 0;
    while c.try_recv().unwrap().is_some() {
        waiting += 1;
    }
    assert_eq!(waiting, MAX_QUEUE);
    b.submit(publish()).unwrap();
    assert!(matches!(
        c.try_recv().unwrap(),
        Some(Delivery::Event { .. })
    ));
}

#[test]
fn a_publisher_that_does_not_read_its_own_events_is_not_refused_either() {
    let hub = Hub::default();
    let mut manifest = provider(&["me", "a"]);
    manifest.imports = consumer("me").imports;
    let me = join(&hub, "me", manifest, &["cap"]);
    let a = join(&hub, "a", consumer("me"), &["cap"]);
    me.submit(subscribe("me")).unwrap();
    a.submit(subscribe("me")).unwrap();
    for _ in 0..MAX_QUEUE + 1 {
        me.submit(publish()).unwrap();
        assert!(matches!(
            a.try_recv().unwrap(),
            Some(Delivery::Event { .. })
        ));
    }
}

#[test]
fn a_call_to_oneself_reserves_room_for_its_own_result() {
    let hub = Hub::default();
    let mut manifest = provider(&["me"]);
    manifest.imports = consumer("me").imports;
    let me = join(&hub, "me", manifest, &["cap"]);
    me.submit(subscribe("me")).unwrap();
    for _ in 0..MAX_QUEUE - 1 {
        me.submit(publish()).unwrap();
    }
    assert_eq!(
        me.submit(call(1, "me", "act")),
        Err(Error::QueueFull),
        "one free slot cannot hold both the call and its result"
    );
}

#[test]
fn two_participants_can_use_the_same_call_id_so_the_id_alone_names_no_one() {
    // Call ids are chosen by each caller and only have to be unique among that caller's own
    // calls. So a plugin can number a call to itself exactly like the application's call.
    // Anything that wants to find "the application's call 77" by its number must therefore
    // also check WHO made it, which is what the right says through `caller()`.
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&[HOST]), &[]);
    let host_generation = hub
        .register_host(HOST, consumer("b"), grants_of(&consumer("b"), &["cap"]))
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    let mut self_calling = provider(&["me"]);
    self_calling.imports = consumer("me").imports;
    let me = join(&hub, "me", self_calling, &["cap"]);

    host.submit(call(77, "b", "act")).unwrap();
    me.submit(call(77, "me", "act")).unwrap();
    let to_b = ticket(b.try_recv().unwrap());
    let to_me = ticket(me.try_recv().unwrap());

    let the_applications = b.open_data(to_b).unwrap();
    let the_plugins_own = me.open_data(to_me).unwrap();
    // Both carry the same number...
    assert_eq!(the_applications.call_id(), 77);
    assert_eq!(the_plugins_own.call_id(), 77);
    // ...and only the caller, stamped by the hub, says whose call each one is.
    assert_eq!(the_applications.caller(), HOST);
    assert_eq!(the_plugins_own.caller(), "me");
}

#[tokio::test]
async fn a_waiting_receiver_wakes_for_a_timeout_without_being_polled() {
    let (_hub, a, b) = pair();
    let mut op = call(1, "b", "act");
    if let Operation::Call { timeout_ms, .. } = &mut op {
        *timeout_ms = 20;
    }
    a.submit(op).unwrap();
    let _ = b.try_recv().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), a.receive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(error_code(Some(result)), "timeout");
}

#[tokio::test]
async fn a_waiting_receiver_is_released_when_its_channel_is_revoked() {
    let (hub, a, _b) = pair();
    let waiting = a.clone();
    let task = tokio::spawn(async move { waiting.receive().await });
    tokio::task::yield_now().await;
    hub.revoke("a");
    let outcome = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome, Err(Error::Cancelled));
}

#[test]
fn registration_rejects_invalid_manifests_and_overlong_grants() {
    let hub = Hub::default();
    let mut bad = provider(&[]);
    bad.manifest_version = 9;
    assert_eq!(hub.register("x", bad, BTreeSet::new()), Err(Error::Version));
    // A grant at registration follows the same rule as any other: it names a provider the
    // participant imports. `x` imports nobody, so a permission of `b` is refused.
    assert_eq!(
        hub.register(
            "x",
            provider(&[]),
            [Permission::new("b", "cap").unwrap()].into()
        ),
        Err(Error::InvalidManifest)
    );
    assert_eq!(
        hub.register("bad name", provider(&[]), BTreeSet::new()),
        Err(Error::InvalidManifest)
    );
    assert!(
        hub.manifests().is_empty(),
        "rejected registrations leave nothing behind"
    );
}

#[test]
fn the_host_id_is_reserved_so_a_plugin_can_never_register_as_the_application() {
    let hub = Hub::default();
    // Whoever registers a plugin chooses its id. If that could be the application's id,
    // a plugin would inherit every right the application has, so the id is reserved.
    assert_eq!(
        hub.register(HOST, consumer("b"), BTreeSet::new()),
        Err(Error::InvalidManifest)
    );
    assert_eq!(
        hub.register(HOST, provider(&["a"]), BTreeSet::new()),
        Err(Error::InvalidManifest)
    );
    assert!(hub.manifests().is_empty());
}

#[test]
fn only_register_host_can_use_the_host_id_and_no_other_id() {
    let hub = Hub::default();
    assert!(
        hub.register_host("not-the-host", consumer("b"), BTreeSet::new())
            .is_err()
    );
    assert!(
        hub.register_host(HOST, consumer("b"), BTreeSet::new())
            .is_ok()
    );
}

#[test]
fn the_application_can_offer_a_service_and_a_plugin_calls_it_with_a_stamped_identity() {
    let hub = Hub::default();
    // The application exports a narrow service to plugin `a` only.
    let host_gen = hub
        .register_host(HOST, provider(&["a"]), BTreeSet::new())
        .unwrap();
    let host = hub.connect(HOST, host_gen).unwrap();
    let a = join(&hub, "a", consumer(HOST), &["cap"]);
    a.submit(call(1, HOST, "act")).unwrap();
    // The application answers like any provider, and sees who asked as the hub stamped
    // it, which is what lets it judge the *device* side on a caller it can trust.
    let Some(Delivery::Call { ticket, caller, .. }) = host.try_recv().unwrap() else {
        panic!("the application received no call")
    };
    assert_eq!(caller, "a");
    reply(&host, ticket, json!("done"));
    assert!(matches!(
        a.try_recv().unwrap(),
        Some(Delivery::Result { result: Outcome::Ok { value }, .. }) if value == json!("done")
    ));
    // A plugin not listed by the application's export is refused like for any provider.
    let c = join(&hub, "c", consumer(HOST), &["cap"]);
    assert_eq!(c.submit(call(2, HOST, "act")), Err(Error::Denied));
}

#[test]
fn only_the_host_can_use_an_explicitly_declared_long_action_budget() {
    use peersus_api::contributions::{Action, Source};
    let hub = Hub::default();
    let mut manifest = provider(&[HOST, "ordinary"]);
    manifest.contributions.actions.push(Action {
        id: "choose".into(),
        label: "Choose a file".into(),
        short_label: None,
        input: peersus_api::contributions::ActionInput::None,
        target: Source {
            service: "svc".into(),
            method: "act".into(),
        },
        capability: Some("cap".into()),
        needs_device: false,
        destructive: false,
        timeout_ms: 60_000,
        chord: None,
        help: None,
    });
    let target = join(&hub, "target", manifest, &[]);
    let host_manifest = consumer("target");
    let generation = hub
        .register_host(
            HOST,
            host_manifest.clone(),
            grants_of(&host_manifest, &["cap"]),
        )
        .unwrap();
    let host = hub.connect(HOST, generation).unwrap();
    let ordinary = join(&hub, "ordinary", consumer("target"), &["cap"]);
    let request = |id, milliseconds, method| {
        let mut operation = call(id, "target", method);
        if let Operation::Call { timeout_ms, .. } = &mut operation {
            *timeout_ms = milliseconds;
        }
        operation
    };
    assert_eq!(
        ordinary.submit(request(1, 60_000, "act")),
        Err(Error::Timeout)
    );
    assert_eq!(host.submit(request(1, 60_001, "act")), Err(Error::Timeout));
    assert_eq!(host.submit(request(2, 60_000, "open")), Err(Error::Timeout));
    host.submit(request(3, 60_000, "act")).unwrap();
    let ticket = ticket(target.try_recv().unwrap());
    reply(&target, ticket, json!("completed"));
    assert!(matches!(
        host.try_recv().unwrap(),
        Some(Delivery::Result {
            id: 3,
            result: Outcome::Ok { .. }
        })
    ));
}

#[test]
fn versioned_execution_selects_a_module_without_changing_its_service_contract() {
    for kind in ["wasm", "native_library"] {
        let declaration = json!({
            "manifest_version": 1,
            "capabilities": {"use.echo": "Lets it echo text back."},
            "imports": [],
            "exports": [{"service":"echo","version":1,"description":"Echoes text.","consumers":["host"],
                "methods":{"echo":{"capability":"use.echo","description":"Does the thing it is named for."}},"events":{}}],
            "execution": {"kind": kind, "abi": 1}
        });
        let module: Manifest = serde_json::from_value(declaration)
            .expect("a supported module execution declaration must be accepted");
        module.validate().unwrap();
        let hub = Hub::default();
        let generation = hub.register("module", module, BTreeSet::new()).unwrap();
        let provider = hub.connect("module", generation).unwrap();
        let consumer = Manifest {
            imports: vec![Import {
                provider: "module".into(),
                service: "echo".into(),
                version: 1,
                methods: names(&["echo"]),
                events: BTreeSet::new(),
            }],
            ..Manifest::new(peersus_api::manifest::Execution::Process)
        };
        let caller_generation = hub.register_host(HOST, consumer, BTreeSet::new()).unwrap();
        let caller = hub.connect(HOST, caller_generation).unwrap();
        let call = || Operation::Call {
            id: 1,
            provider: "module".into(),
            service: "echo".into(),
            version: 1,
            method: "echo".into(),
            args: json!("payload"),
            timeout_ms: 1000,
        };
        assert_eq!(caller.submit(call()), missing("module", "use.echo"));
        hub.set_permissions(
            HOST,
            [Permission::new("module", "use.echo").unwrap()].into(),
        )
        .unwrap();
        caller.submit(call()).unwrap();
        assert!(matches!(
            provider.try_recv().unwrap(),
            Some(Delivery::Call { .. })
        ));
    }
}

/// Hand `operation` to the hub as `channel` at a chosen moment, as [`Channel::submit`] does at
/// the present one.
fn at(channel: &Channel, operation: Operation, now: Instant) -> Result<()> {
    lock(&channel.hub).submit(&channel.identity, operation, now, None, None)
}

/// Spend all of `channel`'s rate at `now`, then go on until it is suspended.
fn flood(channel: &Channel, now: Instant) {
    for _ in 0..BURST {
        at(channel, publish(), now).unwrap();
    }
    for _ in 0..SUSPEND_AFTER {
        assert_eq!(at(channel, publish(), now), Err(Error::RateLimited));
    }
}

#[test]
fn an_extension_over_its_rate_is_refused_while_the_others_go_on() {
    let (hub, a, b) = pair();
    let now = Instant::now();
    for _ in 0..BURST {
        at(&b, publish(), now).unwrap();
    }
    assert_eq!(at(&b, publish(), now), Err(Error::RateLimited));
    // Another participant's rate is its own.
    at(&a, call(1, "b", "act"), now).unwrap();
    // A reply ends work the extension was given, so it is never held back.
    let ticket = ticket(b.try_recv().unwrap());
    at(
        &b,
        Operation::Reply {
            ticket,
            result: Outcome::ok(json!(1)),
        },
        now,
    )
    .unwrap();
    assert!(matches!(
        a.try_recv().unwrap(),
        Some(Delivery::Result { id: 1, .. })
    ));
    // The rate comes back with time, up to a burst and no further.
    let later = now + Duration::from_secs(1);
    for _ in 0..BURST {
        at(&b, publish(), later).unwrap();
    }
    assert_eq!(at(&b, publish(), later), Err(Error::RateLimited));
    at(&b, publish(), later + COST).unwrap();
    assert!(
        hub.take_suspended().is_empty(),
        "a few refusals are not a suspension"
    );
}

#[test]
fn an_extension_that_keeps_flooding_is_suspended_named_once_and_heard_again_afterwards() {
    let (hub, a, b) = pair();
    let now = Instant::now();
    flood(&b, now);
    assert_eq!(hub.take_suspended(), ["b"]);
    // Time alone does not bring its rate back while it is suspended.
    let during = now + SUSPENSION - Duration::from_millis(1);
    assert_eq!(at(&b, publish(), during), Err(Error::RateLimited));
    // Nobody is left waiting on an extension that is not listened to.
    assert_eq!(at(&a, call(1, "b", "act"), during), Err(Error::Unavailable));
    assert!(
        hub.take_suspended().is_empty(),
        "one suspension is told once, whatever the extension goes on sending"
    );
    // Afterwards it is an ordinary participant again.
    let after = now + SUSPENSION;
    at(&b, publish(), after).unwrap();
    at(&a, call(2, "b", "act"), after).unwrap();
}

#[test]
fn refusals_with_quiet_between_them_never_add_up_to_a_suspension() {
    let (hub, _a, b) = pair();
    let mut now = Instant::now();
    for _ in 0..=SUSPEND_AFTER {
        for _ in 0..BURST {
            at(&b, publish(), now).unwrap();
        }
        assert_eq!(at(&b, publish(), now), Err(Error::RateLimited));
        now += Duration::from_secs(2);
    }
    assert!(hub.take_suspended().is_empty());
    at(&b, publish(), now).unwrap();
}

#[test]
fn a_restart_keeps_a_suspension_and_a_removal_forgets_it() {
    let (hub, _a, b) = pair();
    let now = Instant::now();
    flood(&b, now);
    drop(b);
    // The same extension, started again: the suspension is about it, not about one process.
    let b = join(&hub, "b", provider(&["a"]), &[]);
    assert_eq!(at(&b, publish(), now), Err(Error::RateLimited));
    drop(b);
    // Taken away altogether: another package under the same name starts with nothing held
    // against it, and the person is not told about one that is gone.
    hub.forget("b");
    assert!(hub.take_suspended().is_empty());
    let b = join(&hub, "b", provider(&["a"]), &[]);
    at(&b, publish(), now).unwrap();
}

#[test]
fn the_application_itself_is_not_limited() {
    let hub = Hub::default();
    let b = join(&hub, "b", provider(&[HOST]), &[]);
    let manifest = consumer("b");
    let grants = grants_of(&manifest, &["cap"]);
    let generation = hub.register_host(HOST, manifest, grants).unwrap();
    let host = hub.connect(HOST, generation).unwrap();
    let now = Instant::now();
    for id in 1..=u64::from(BURST) + 10 {
        at(&host, call(id, "b", "act"), now).unwrap();
        reply(&b, ticket(b.try_recv().unwrap()), json!(null));
        assert!(host.try_recv().unwrap().is_some());
    }
}

#[test]
fn a_transport_is_told_how_long_to_leave_a_suspended_extension_unread() {
    let (_hub, _a, b) = pair();
    assert_eq!(b.retry_after(), None, "nothing was refused yet");
    flood(&b, Instant::now());
    let wait = b
        .retry_after()
        .expect("a suspended extension is left unread");
    assert!(wait <= SUSPENSION && wait > SUSPENSION - Duration::from_secs(10));
}
