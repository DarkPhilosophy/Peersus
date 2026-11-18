//! The owner's own rules, tried without a window: what is offered, granted, refused and carried.
use super::*;
use peersus_api::{
    contributions::{Action, Source},
    manifest::{Export, Manifest, Member},
    message::{Delivery, Operation},
};

/// A member that needs `capability` (or nothing) and says what it does.
fn member(capability: Option<&str>) -> Member {
    Member {
        capability: capability.map(str::to_owned),
        description: "Does what its name says.".to_owned(),
    }
}

/// A plugin offering one action that needs capability `cap`, open to the application.
fn plugin_with_guarded_action() -> PluginSpec {
    let mut manifest = Manifest {
        capabilities: [("cap".to_owned(), "Lets the plugin go.".to_owned())].into(),
        exports: vec![Export {
            service: "svc".into(),
            version: 1,
            description: "A test service.".into(),
            consumers: [HOST.to_owned()].into(),
            methods: [("go".to_owned(), member(Some("cap")))].into(),
            events: Default::default(),
        }],
        ..Manifest::default()
    };
    manifest.contributions.actions.push(Action {
        id: "go".into(),
        label: "Go".into(),
        short_label: None,
        input: peersus_api::contributions::ActionInput::None,
        target: Source {
            service: "svc".into(),
            method: "go".into(),
        },
        capability: Some("cap".into()),
        needs_device: false,
        destructive: false,
        timeout_ms: peersus_api::contributions::default_action_timeout(),
        chord: None,
        help: None,
    });
    PluginSpec {
        id: PluginId::new("alpha").unwrap(),
        manifest,
        program: Some(PathBuf::from("/nonexistent")),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Default::default(),
    }
}

/// A paired device and what it was granted: for each plugin, the permissions given as
/// `(owner, name)` pairs.
fn peer_granted(id: &str, grants: &[(&str, &[(&str, &str)])]) -> Peer {
    Peer {
        id: PeerId(id.into()),
        name: id.into(),
        paired_at: 0,
        last_seen: None,
        online: true,
        sleeping: false,
        statuses: Vec::new(),
        grants: grants
            .iter()
            .map(|(plugin, given)| {
                (
                    PluginId::new(*plugin).unwrap(),
                    Grants::new(given.iter().map(|(o, n)| permission(o, n))).unwrap(),
                )
            })
            .collect(),
        plugin_values: Default::default(),
    }
}

fn registry_with_alpha() -> Registry {
    let mut registry = Registry::default();
    registry.register(plugin_with_guarded_action()).unwrap();
    registry
}

fn permission(owner: &str, name: &str) -> peersus_api::permission::Permission {
    peersus_api::permission::Permission::new(owner, name).unwrap()
}

/// A set of permissions, each given as `(owner, name)`.
fn set(values: &[(&str, &str)]) -> BTreeSet<peersus_api::permission::Permission> {
    values.iter().map(|(o, n)| permission(o, n)).collect()
}

/// What an extension may learn of its own agreement is one boolean, for itself alone, and the service is offered only to one that asked for it.
#[tokio::test]
async fn an_extension_learns_only_whether_its_own_agreement_was_accepted() {
    use serde_json::json;
    let mut spec = plugin_with_guarded_action();
    spec.manifest.imports.push(peersus_api::manifest::Import {
        provider: HOST.into(),
        service: EXTENSION_SERVICE.into(),
        version: 1,
        methods: ["tos".into(), "outside_store".into()].into(),
        events: Default::default(),
    });
    spec.manifest.agreements = Some(peersus_api::agreement::Agreements {
        pages: vec![
            peersus_api::agreement::Page {
                kind: peersus_api::agreement::Kind::Licence,
                title: "MIT".into(),
                explains: "You may use it.".into(),
                changes: "Nothing changes.".into(),
                spdx: Some("MIT".into()),
                links: vec![peersus_api::agreement::Link {
                    label: "Text".into(),
                    url: "https://opensource.org/license/mit".into(),
                }],
                body: vec![],
            },
            peersus_api::agreement::Page {
                kind: peersus_api::agreement::Kind::Notice,
                title: "Notice".into(),
                explains: "It does a thing.".into(),
                changes: "A page appears.".into(),
                spdx: None,
                links: vec![],
                body: vec![],
            },
        ],
    });
    let digest = peersus_core::agreements::digest(spec.manifest.agreements.as_ref().unwrap());
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec).unwrap();
    // Offered to the plugin that asked, with one method that takes no capability, and to nobody else.
    let exports = host_exports(&registry);
    let service = exports
        .iter()
        .find(|export| export.service == EXTENSION_SERVICE)
        .expect("the service was not offered to the plugin that imported it");
    assert_eq!(service.consumers, [plugin.as_str().to_owned()].into());
    assert_eq!(
        service.methods.keys().collect::<Vec<_>>(),
        ["outside_store", "tos"]
    );
    assert!(service.methods["tos"].capability.is_none());
    assert!(service.methods["outside_store"].capability.is_none());
    assert!(host_exports(&Registry::default()).is_empty());
    let root = tempfile::tempdir().unwrap();
    let network_settings = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..Default::default()
    };
    let endpoint = peersus_net::transport::bind(iroh::SecretKey::generate(), &network_settings)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        "Test".into(),
        peersus_net::peers::Peers::open(root.path().join("peers.json")).unwrap(),
        peersus_net::history::History::open(root.path().join("history.json")).unwrap(),
    );
    let settings =
        peersus_net::settings::SettingsStore::open(root.path().join("preferences.json")).unwrap();
    let grants =
        peersus_net::grants::StandaloneGrants::open(root.path().join("grants.json")).unwrap();
    let (mut runtime, _client) = Runtime::new(
        network,
        grants,
        settings,
        registry,
        root.path().join("sockets"),
    )
    .unwrap();
    let ask = |runtime: &mut Runtime, who: &str, args: serde_json::Value| {
        runtime
            .answer_plugin_call(who, EXTENSION_SERVICE, "tos", &args)
            .0
    };
    let accepted = |outcome: peersus_api::message::Outcome| match outcome {
        peersus_api::message::Outcome::Ok { value } => value,
        other => panic!("{other:?}"),
    };
    // Nothing accepted yet: false, and the answer holds that one field and nothing else (no digest, no pages).
    let value = accepted(ask(&mut runtime, plugin.as_str(), json!({})));
    assert_eq!(value, json!({"accepted": false}));
    // The person accepted exactly this agreement: true. Another agreement's digest, or an old one, would be false.
    runtime
        .accepted_agreements
        .insert(plugin.as_str().to_owned(), digest);
    assert_eq!(
        accepted(ask(&mut runtime, plugin.as_str(), json!({}))),
        json!({"accepted": true})
    );
    runtime
        .accepted_agreements
        .insert(plugin.as_str().to_owned(), "0".repeat(64));
    assert_eq!(
        accepted(ask(&mut runtime, plugin.as_str(), json!({}))),
        json!({"accepted": false})
    );
    // It takes no argument, and a name that is not an installed plugin is told nothing.
    assert!(matches!(
        ask(&mut runtime, plugin.as_str(), json!({"plugin": "other"})),
        peersus_api::message::Outcome::Error { .. }
    ));
    assert!(matches!(
        ask(&mut runtime, "stranger", json!({})),
        peersus_api::message::Outcome::Error { .. }
    ));
    // The person's own acceptance of the warning about installing from outside the store, a bare boolean too (`extension.tos.outside_store`):
    // false until they gave it, true after, for every extension alike, and not the extension's own agreement.
    let outside = |runtime: &mut Runtime, args: serde_json::Value| {
        runtime
            .answer_plugin_call(plugin.as_str(), EXTENSION_SERVICE, "outside_store", &args)
            .0
    };
    assert_eq!(
        accepted(outside(&mut runtime, json!({}))),
        json!({"accepted": false})
    );
    runtime
        .settings
        .update(&peersus_core::config::SettingsPatch {
            outside_store_risk_accepted: Some(true),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        accepted(outside(&mut runtime, json!({}))),
        json!({"accepted": true})
    );
    assert!(matches!(
        outside(&mut runtime, json!({"plugin": "other"})),
        peersus_api::message::Outcome::Error { .. }
    ));
}

/// An extension that shows words of its own asks for the person's language: offered only to one that imported the service,
/// answering the chosen language, else English with no device language known.
#[tokio::test]
async fn an_extension_learns_the_persons_language() {
    use serde_json::json;
    let mut spec = plugin_with_guarded_action();
    spec.manifest.imports.push(peersus_api::manifest::Import {
        provider: HOST.into(),
        service: LANGUAGE_SERVICE.into(),
        version: 1,
        methods: ["get".into()].into(),
        events: [LANGUAGE_CHANGED.into()].into(),
    });
    // The package translates its action's label, its description and what its capability lets it do into Portuguese; the
    // person will choose Brazilian Portuguese.
    let (_, granted_on) = spec.manifest.capabilities.first_key_value().unwrap();
    let granted_on = granted_on.clone();
    spec.languages = vec![
        peersus_api::language::Catalog::parse(&format!(
            "msgid \"\"\nmsgstr \"Language: pt\\n\"\n\nmsgid \"Go\"\nmsgstr \"Ir\"\n\n\
             msgid \"Does a thing\"\nmsgstr \"Faz uma coisa\"\n\nmsgid \"{granted_on}\"\nmsgstr \"Inofensivo\"\n"
        ))
        .unwrap(),
    ]
    .into();
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec).unwrap();
    let exports = host_exports(&registry);
    let service = exports
        .iter()
        .find(|export| export.service == LANGUAGE_SERVICE)
        .expect("the service was not offered to the plugin that imported it");
    assert_eq!(service.consumers, [plugin.as_str().to_owned()].into());
    // The person's language is theirs to tell: behind its own permission, for the answer and the event alike.
    assert_eq!(
        service.methods["get"].capability.as_deref(),
        Some(LANGUAGE_READ_CAPABILITY)
    );
    assert_eq!(
        service.events[LANGUAGE_CHANGED].capability.as_deref(),
        Some(LANGUAGE_READ_CAPABILITY)
    );
    assert!(host_exports(&Registry::default()).is_empty());
    let root = tempfile::tempdir().unwrap();
    let network_settings = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..Default::default()
    };
    let endpoint = peersus_net::transport::bind(iroh::SecretKey::generate(), &network_settings)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        "Test".into(),
        peersus_net::peers::Peers::open(root.path().join("peers.json")).unwrap(),
        peersus_net::history::History::open(root.path().join("history.json")).unwrap(),
    );
    let settings =
        peersus_net::settings::SettingsStore::open(root.path().join("preferences.json")).unwrap();
    let grants =
        peersus_net::grants::StandaloneGrants::open(root.path().join("grants.json")).unwrap();
    let (mut runtime, _client) = Runtime::new(
        network,
        grants,
        settings,
        registry,
        root.path().join("sockets"),
    )
    .unwrap();
    let ask = |runtime: &mut Runtime, args: serde_json::Value| match runtime
        .answer_plugin_call(plugin.as_str(), LANGUAGE_SERVICE, "get", &args)
        .0
    {
        peersus_api::message::Outcome::Ok { value } => Some(value),
        _ => None,
    };
    assert_eq!(
        ask(&mut runtime, json!({})),
        Some(json!({"language": "en"}))
    );
    runtime
        .settings
        .update(&peersus_core::config::SettingsPatch {
            language: Some(Some(
                peersus_core::config::Language::parse("pt-BR").unwrap(),
            )),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        ask(&mut runtime, json!({})),
        Some(json!({"language": "pt-br"}))
    );
    let label = |infos: Vec<peersus_core::model::PluginInfo>| {
        infos[0].manifest.contributions.actions[0].label.clone()
    };
    assert_eq!(
        label(runtime.window_plugin_infos()),
        "Ir",
        "the window shows the package's own translation"
    );
    assert_eq!(
        label(runtime.plugin_infos()),
        "Go",
        "another plugin is told the manifest as written"
    );
    // The description on its card is the package's translation too; what its capability lets it do is not: a person grants on
    // the manifest's own English.
    runtime.installed_packages.insert(
        plugin.clone(),
        peersus_api::package::Package {
            name: "Thing".into(),
            version: "1.0.0".into(),
            description: "Does a thing".into(),
            changelog: String::new(),
            author: String::new(),
            link: String::new(),
        },
    );
    let shown = runtime.window_plugin_infos().remove(0);
    assert_eq!(shown.package.unwrap().description, "Faz uma coisa");
    assert_eq!(
        shown
            .manifest
            .capabilities
            .first_key_value()
            .map(|(_, words)| words),
        Some(&granted_on)
    );
    // It takes no argument.
    assert_eq!(ask(&mut runtime, json!({"language": "de"})), None);
}

#[tokio::test]
async fn page_offers_route_the_same_source_keep_settings_and_withdraw_without_other_plugins() {
    use peersus_api::contributions::{Page, Placement, Surface};
    use serde_json::json;
    let mut spec = plugin_with_guarded_action();
    spec.manifest.contributions.pages.push(Page {
        id: "editor".into(),
        label: "Editor".into(),
        placement: Placement::Settings,
        icon: None,
        source: Some(Source {
            service: "svc".into(),
            method: "go".into(),
        }),
        surface: Some(Surface::Elements),
        needs_device: false,
        capability: Some("cap".into()),
        platforms: Vec::new(),
    });
    spec.manifest.imports.push(peersus_api::manifest::Import {
        provider: HOST.into(),
        service: crate::page_offers::SERVICE.into(),
        version: 1,
        methods: ["set".into()].into(),
        events: Default::default(),
    });
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec).unwrap();
    let exports = host_exports(&registry);
    let pages = exports
        .iter()
        .find(|export| export.service == crate::page_offers::SERVICE)
        .unwrap();
    assert_eq!(pages.consumers, [plugin.as_str().to_owned()].into());
    assert!(host_exports(&Registry::default()).is_empty());
    let root = tempfile::tempdir().unwrap();
    let network_settings = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..Default::default()
    };
    let endpoint = peersus_net::transport::bind(iroh::SecretKey::generate(), &network_settings)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        "Test".into(),
        peersus_net::peers::Peers::open(root.path().join("peers.json")).unwrap(),
        peersus_net::history::History::open(root.path().join("history.json")).unwrap(),
    );
    let settings =
        peersus_net::settings::SettingsStore::open(root.path().join("preferences.json")).unwrap();
    let grants =
        peersus_net::grants::StandaloneGrants::open(root.path().join("grants.json")).unwrap();
    let (mut runtime, _client) = Runtime::new(
        network,
        grants,
        settings,
        registry,
        root.path().join("sockets"),
    )
    .unwrap();
    let set = |runtime: &mut Runtime, args| {
        runtime
            .answer_plugin_call(plugin.as_str(), crate::page_offers::SERVICE, "set", &args)
            .0
    };
    assert!(matches!(
        set(
            &mut runtime,
            json!({"pages":[{"id":"editor-main","page":"editor","placement":"primary"}]})
        ),
        peersus_api::message::Outcome::Ok { .. }
    ));
    assert_eq!(
        runtime.page_source(&plugin, "editor-main").unwrap(),
        runtime.page_source(&plugin, "editor").unwrap()
    );
    let infos = runtime.plugin_infos();
    let pages = &infos[0].manifest.contributions.pages;
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].placement, Placement::Settings);
    assert_eq!(pages[1].placement, Placement::Primary);
    assert!(matches!(
        set(&mut runtime, json!({"pages":[]})),
        peersus_api::message::Outcome::Ok { .. }
    ));
    assert!(runtime.page_source(&plugin, "editor-main").is_err());
    assert!(runtime.page_source(&plugin, "editor").is_ok());
    set(
        &mut runtime,
        json!({"pages":[{"id":"editor-main","page":"editor","placement":"primary"}]}),
    );
    runtime.stop_plugin(&plugin).await;
    assert!(runtime.page_source(&plugin, "editor-main").is_err());
    assert_eq!(
        runtime.plugin_infos()[0].manifest.contributions.pages.len(),
        1
    );
}

#[test]
fn the_application_holds_nothing_until_the_user_has_granted_something() {
    let registry = registry_with_alpha();
    assert_eq!(host_grants(&registry, &[]), set(&[]));
    // Paired devices with no grants at all change nothing: default-deny.
    let peers = [
        peer_granted("p1", &[]),
        peer_granted("p2", &[("alpha", &[])]),
    ];
    assert_eq!(host_grants(&registry, &peers), set(&[]));
}

#[test]
fn one_devices_grant_lets_the_application_run_that_plugins_action() {
    let registry = registry_with_alpha();
    let peers = [peer_granted("p1", &[("alpha", &[("alpha", "cap")])])];
    assert_eq!(host_grants(&registry, &peers), set(&[("alpha", "cap")]));
}

#[test]
fn the_result_is_the_union_over_every_device_not_the_last_one_changed() {
    let mut registry = Registry::default();
    let mut alpha = plugin_with_guarded_action();
    alpha.manifest.capabilities.insert(
        "other".into(),
        "Lets the plugin do the second thing.".into(),
    );
    alpha.manifest.exports[0]
        .methods
        .insert("two".into(), member(Some("other")));
    alpha.manifest.contributions.actions.push(Action {
        id: "two".into(),
        label: "Two".into(),
        short_label: None,
        input: peersus_api::contributions::ActionInput::None,
        target: Source {
            service: "svc".into(),
            method: "two".into(),
        },
        capability: Some("other".into()),
        needs_device: false,
        destructive: false,
        timeout_ms: peersus_api::contributions::default_action_timeout(),
        chord: None,
        help: None,
    });
    registry.register(alpha).unwrap();
    // Each device grants a different capability. Revoking one device's grant must not
    // be able to wipe out what another device was given, so the whole picture is
    // recomputed from every device instead of patched with the latest change.
    let peers = [
        peer_granted("p1", &[("alpha", &[("alpha", "cap")])]),
        peer_granted("p2", &[("alpha", &[("alpha", "other")])]),
    ];
    assert_eq!(
        host_grants(&registry, &peers),
        set(&[("alpha", "cap"), ("alpha", "other")])
    );
    assert_eq!(
        host_grants(&registry, &peers[1..]),
        set(&[("alpha", "other")])
    );
}

#[test]
fn a_grant_for_one_plugin_never_opens_another_plugins_guard_of_the_same_name() {
    // Two plugins each declare their OWN `cap`, with whatever meaning each chose. The user
    // grants `alpha`'s. That says nothing about `beta`'s, so what the application holds is
    // a permission OF alpha and not a bare name that beta's guard would also match.
    let mut registry = Registry::default();
    registry.register(plugin_with_guarded_action()).unwrap();
    let mut beta = plugin_with_guarded_action();
    beta.id = PluginId::new("beta").unwrap();
    beta.manifest.exports[0].service = "svc2".into();
    beta.manifest.contributions.actions[0].target.service = "svc2".into();
    registry.register(beta).unwrap();
    let peers = [peer_granted("p1", &[("alpha", &[("alpha", "cap")])])];
    let held = host_grants(&registry, &peers);
    assert!(
        held.contains(&permission("alpha", "cap")),
        "alpha's own permission was not carried: {held:?}"
    );
    assert!(
        !held.contains(&permission("beta", "cap")),
        "a grant for alpha opened beta's guard: {held:?}"
    );
    assert_eq!(held.len(), 1, "{held:?}");
}

#[test]
fn every_refusal_the_hub_can_give_has_its_own_fixed_wording() {
    use peersus_api::Error;
    let words = [
        (Error::Denied, "The plugin does not allow this action"),
        (Error::QueueFull, "Too many operations are pending"),
        (Error::UnknownProvider, "The plugin is not running"),
        (Error::Unavailable, "The plugin is not running"),
        (
            Error::UnknownMethod,
            "The plugin does not offer this action",
        ),
        (Error::Version, "The plugin does not offer this action"),
        (Error::Timeout, "The request could not be sent"),
        (Error::Oversize, "The request could not be sent"),
    ];
    for (error, expected) in words {
        assert_eq!(refusal_text(&error), expected, "{error:?}");
    }
}

#[test]
fn a_denial_is_never_described_as_the_plugin_not_running() {
    use peersus_api::Error;
    // Telling a user that a plugin is not running when it actually refused them sends
    // them looking for a crash that did not happen.
    assert_ne!(
        refusal_text(&Error::Denied),
        refusal_text(&Error::Unavailable)
    );
    assert_ne!(
        refusal_text(&Error::UnknownMethod),
        refusal_text(&Error::Unavailable)
    );
}

#[test]
fn only_the_application_may_collect_what_the_application_parked() {
    assert!(may_collect(HOST));
    // Not another plugin, not something that merely looks like the host.
    for caller in ["me", "sink", "host2", "Host", "", " host"] {
        assert!(!may_collect(caller), "{caller:?} must not collect");
    }
}

#[test]
fn a_plugin_using_the_applications_call_number_cannot_collect_its_stream() {
    use crate::parked::{Owner, Parked};
    let now = std::time::Instant::now();
    let mut parked: Parked<&str> = Parked::default();
    let owner = Owner {
        peer: PeerId("p".into()),
        plugin: PluginId::new("sink").unwrap(),
    };
    // The application parks a peer's stream under its call number 77.
    parked
        .park(77, owner, "the peer's stream", now, Duration::from_secs(10))
        .unwrap();
    // A plugin has made a call to itself and numbered it 77 too, so it holds a genuine
    // right whose call number matches. Collecting is decided on WHO made the call
    // first, so the stream is never even looked up for it.
    let impostor = ("me", 77u64);
    let taken = if may_collect(impostor.0) {
        parked.claim(impostor.1, now)
    } else {
        None
    };
    assert_eq!(
        taken, None,
        "a plugin collected a stream that was not its own"
    );
    // And the stream is still there for the call it really belongs to.
    assert!(may_collect(HOST));
    assert_eq!(parked.claim(77, now), Some("the peer's stream"));
}

#[test]
fn a_stream_that_is_refused_leaves_no_record_and_one_that_is_announced_leaves_both() {
    let mut parked = crate::parked::Parked::<&str>::default();
    let mut receiving = HashMap::new();
    let now = std::time::Instant::now();
    let owner = || crate::parked::Owner {
        peer: PeerId("p".into()),
        plugin: PluginId::new("sink").unwrap(),
    };
    // The plugin could not be told: the stream is let go at once and nothing waits for a
    // result that this call will never bring.
    assert_eq!(
        keep_and_announce(&mut parked, &mut receiving, 1, owner(), "one", now, || {
            Err("The plugin is not running")
        }),
        Err("The plugin is not running")
    );
    assert_eq!(parked.claim(1, now), None);
    assert!(receiving.is_empty());
    // Told: the stream waits for the plugin and the call is known as a receive for it.
    for id in 2..2 + crate::parked::MAX_PARKED as u64 {
        keep_and_announce(
            &mut parked,
            &mut receiving,
            id,
            owner(),
            "kept",
            now,
            || Ok(()),
        )
        .unwrap();
    }
    assert_eq!(receiving.len(), crate::parked::MAX_PARKED);
    assert_eq!(receiving.get(&2).map(PluginId::as_str), Some("sink"));
    // No room to set another aside: it is refused before the plugin is told anything, and
    // however often a device tries again, nothing is recorded for it.
    for id in 1_000..1_010 {
        let mut told = false;
        assert_eq!(
            keep_and_announce(
                &mut parked,
                &mut receiving,
                id,
                owner(),
                "more",
                now,
                || {
                    told = true;
                    Ok(())
                }
            ),
            Err("Too many transfers are waiting for plugins")
        );
        assert!(!told);
    }
    assert_eq!(receiving.len(), crate::parked::MAX_PARKED);
    assert_eq!(parked.len(), crate::parked::MAX_PARKED);
}

fn receiving_registry() -> Registry {
    let mut registry = Registry::default();
    registry
        .register(plugin_that_receives(Some("sink.write")))
        .unwrap();
    registry
}

#[test]
fn a_device_the_user_granted_is_admitted_to_the_plugin_that_exports_the_service() {
    let registry = receiving_registry();
    let peers = [peer_granted("p", &[("sink", &[("sink", "sink.write")])])];
    let admitted = admit(&registry, &peers, &PeerId("p".into()), "sink.service", 1)
        .expect("a granted device must be admitted");
    assert_eq!(admitted.plugin.as_str(), "sink");
    assert_eq!(admitted.service, "sink.service");
    assert_eq!(admitted.version, 1);
}

#[test]
fn a_device_without_the_grant_is_refused_before_anything_is_set_aside() {
    let registry = receiving_registry();
    // Paired, but nothing granted: default-deny.
    let none = [peer_granted("p", &[])];
    assert_eq!(
        admit(&registry, &none, &PeerId("p".into()), "sink.service", 1).err(),
        Some("Plugin is not permitted for this device")
    );
    // Granted a different capability of the same plugin: still not this one.
    let wrong = [peer_granted("p", &[("sink", &[("sink", "sink.other")])])];
    assert_eq!(
        admit(&registry, &wrong, &PeerId("p".into()), "sink.service", 1).err(),
        Some("Plugin is not permitted for this device")
    );
    // Granted the right capability of ANOTHER plugin: says nothing about this one.
    let other = [peer_granted(
        "elsewhere",
        &[("elsewhere", &[("sink", "sink.write")])],
    )];
    assert_eq!(
        admit(
            &registry,
            &other,
            &PeerId("elsewhere".into()),
            "sink.service",
            1
        )
        .err(),
        Some("Plugin is not permitted for this device")
    );
}

#[test]
fn an_unknown_device_service_or_version_is_refused_with_its_own_words() {
    let registry = receiving_registry();
    let peers = [peer_granted("p", &[("sink", &[("sink", "sink.write")])])];
    assert_eq!(
        admit(
            &registry,
            &peers,
            &PeerId("stranger".into()),
            "sink.service",
            1
        )
        .err(),
        Some("Device is not paired")
    );
    assert_eq!(
        admit(
            &registry,
            &peers,
            &PeerId("p".into()),
            "nobody.handles.this",
            1
        )
        .err(),
        Some("No plugin handles this request")
    );
    // The same service at another version is not a best-effort match.
    assert_eq!(
        admit(&registry, &peers, &PeerId("p".into()), "sink.service", 2).err(),
        Some("No plugin handles this request")
    );
}

#[test]
fn what_the_supervisor_reports_maps_to_the_status_a_user_sees() {
    use peersus_core::model::PluginStatus::{Failed, Restarting, Running, Starting, Stopped};
    let pause = Duration::from_millis(250);
    assert_eq!(status_of(Some(&PluginEvent::Started)), Starting);
    assert_eq!(status_of(Some(&PluginEvent::Connected)), Running);
    assert_eq!(
        status_of(Some(&PluginEvent::Failed { restart_in: pause })),
        Restarting
    );
    assert_eq!(status_of(Some(&PluginEvent::GaveUp)), Failed);
    assert_eq!(status_of(Some(&PluginEvent::Stopped)), Stopped);
}

#[test]
fn a_launched_process_is_not_reported_as_running_until_it_has_connected() {
    use peersus_core::model::PluginStatus::Running;
    // A screen that calls a launched-but-silent plugin "running" shows a button whose
    // every press fails, so only a completed handshake may earn that word.
    assert_ne!(status_of(Some(&PluginEvent::Started)), Running);
    assert_eq!(status_of(Some(&PluginEvent::Connected)), Running);
}

#[test]
fn the_last_report_of_a_supervisor_that_has_ended_is_still_seen() {
    // A supervisor that gives up sends `GaveUp` and finishes in the same moment, which
    // drops its end of the channel. That last report is the one a user most needs, and
    // asking the channel whether anything changed fails once it is closed, so a closed
    // channel must not hide it.
    let (sender, mut receiver) = tokio::sync::watch::channel(PluginEvent::Stopped);
    sender.send_replace(PluginEvent::GaveUp);
    drop(sender);
    assert_eq!(observe(&mut receiver), Some(PluginEvent::GaveUp));
}

#[test]
fn a_report_is_seen_once_and_a_finished_supervisor_adds_nothing_after_it() {
    let (sender, mut receiver) = tokio::sync::watch::channel(PluginEvent::Stopped);
    sender.send_replace(PluginEvent::GaveUp);
    drop(sender);
    assert_eq!(observe(&mut receiver), Some(PluginEvent::GaveUp));
    // Seen already: asking again must say there is nothing new, or the application would
    // announce the same failure every second for ever.
    assert_eq!(observe(&mut receiver), None);
}

#[test]
fn a_live_supervisor_with_nothing_new_to_say_reports_nothing() {
    let (sender, mut receiver) = tokio::sync::watch::channel(PluginEvent::Started);
    // The value a receiver is created with counts as already seen.
    assert_eq!(observe(&mut receiver), None);
    sender.send_replace(PluginEvent::Connected);
    assert_eq!(observe(&mut receiver), Some(PluginEvent::Connected));
    assert_eq!(observe(&mut receiver), None);
}

#[test]
fn a_plugin_that_was_never_started_is_stopped_not_running() {
    // No supervisor entry at all (it failed to launch, or was never registered to run):
    // nothing says it is running, so it must not be reported as such.
    assert_eq!(status_of(None), peersus_core::model::PluginStatus::Stopped);
}

#[test]
fn a_grant_for_another_plugin_or_for_something_no_action_needs_never_leaks() {
    let registry = registry_with_alpha();
    let peers = [
        // Granted to a different plugin: says nothing about alpha.
        peer_granted("p1", &[("beta", &[("alpha", "cap")])]),
        // Granted to alpha, but no action of alpha needs it.
        peer_granted("p2", &[("alpha", &[("alpha", "unrelated")])]),
    ];
    assert_eq!(host_grants(&registry, &peers), set(&[]));
}

/// A plugin that takes what peers send, open to the application, with or without a
/// capability guarding `receive`.
fn plugin_that_receives(capability: Option<&str>) -> PluginSpec {
    PluginSpec {
        id: PluginId::new("sink").unwrap(),
        manifest: Manifest {
            capabilities: capability
                .map(|c| (c.to_owned(), format!("Lets the plugin use {c}.")))
                .into_iter()
                .collect(),
            exports: vec![Export {
                service: "sink.service".into(),
                version: 1,
                description: "A test service.".into(),
                consumers: [HOST.to_owned()].into(),
                methods: [(
                    crate::plugin_host::RECEIVE_METHOD.to_owned(),
                    member(capability),
                )]
                .into(),
                events: Default::default(),
            }],
            ..Manifest::default()
        },
        program: Some(PathBuf::from("/nonexistent")),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Default::default(),
    }
}

#[test]
fn what_the_user_grants_for_receive_is_what_lets_the_application_call_it() {
    let mut registry = Registry::default();
    registry
        .register(plugin_that_receives(Some("sink.write")))
        .unwrap();
    // The user allowed one device to use the plugin's `receive`. The hub only lets the
    // application call `receive` when it holds that capability, so it must be carried
    // over, exactly as the capability of an action is. A plugin whose only offer is
    // `receive` has no actions, so this cannot come from them.
    let peers = [peer_granted("p", &[("sink", &[("sink", "sink.write")])])];
    assert_eq!(
        host_grants(&registry, &peers),
        set(&[("sink", "sink.write")])
    );
}

#[test]
fn a_device_is_told_only_the_services_it_may_open_here_as_admission_would_take_them() {
    let mut registry = Registry::default();
    registry
        .register(plugin_that_receives(Some("sink.write")))
        .unwrap();
    let mut other = plugin_that_receives(Some("other.write"));
    other.id = PluginId::new("other").unwrap();
    other.manifest.exports[0].service = "other.service".into();
    registry.register(other).unwrap();
    // Allowed the sink's receiving permission only: that service is named, the other extension's is not.
    let allowed = peer_granted("p", &[("sink", &[("sink", "sink.write")])]);
    let running: BTreeSet<PluginId> = [
        PluginId::new("sink").unwrap(),
        PluginId::new("other").unwrap(),
    ]
    .into();
    // Registered but not running (stopped, failing, switched off): it would not answer, so nothing is said of it.
    assert!(super::admission::accepted_from(&registry, &BTreeSet::new(), &allowed).is_empty());
    let taken = super::admission::accepted_from(&registry, &running, &allowed);
    assert_eq!(
        taken,
        [peersus_net::streams::ServiceRef {
            service: "sink.service".into(),
            version: 1
        }]
    );
    // And it is exactly what admission lets through, service by service.
    for (service, admitted) in [("sink.service", true), ("other.service", false)] {
        assert_eq!(
            super::admission::admit(
                &registry,
                std::slice::from_ref(&allowed),
                &allowed.id,
                service,
                1
            )
            .is_ok(),
            admitted
        );
    }
    // A device allowed nothing is told nothing.
    let nothing = peer_granted("q", &[]);
    assert!(super::admission::accepted_from(&registry, &running, &nothing).is_empty());
}

#[test]
fn a_plugin_learns_which_of_its_own_services_a_device_takes_and_nothing_of_another_plugin() {
    let aliases = crate::aliases::Aliases::new();
    let peer = recognisable_peer(true);
    let clip = PluginId::new("clip").unwrap();
    let own = [peersus_net::streams::ServiceRef {
        service: "clip.share".into(),
        version: 1,
    }];
    let said = std::collections::BTreeMap::from([(
        peer.id.clone(),
        vec![
            peersus_net::streams::ServiceRef {
                service: "clip.share".into(),
                version: 1,
            },
            peersus_net::streams::ServiceRef {
                service: "secret.other".into(),
                version: 1,
            },
        ],
    )]);
    let listed = peers_listing_with(
        &aliases,
        &clip,
        &asks_send(),
        std::slice::from_ref(&peer),
        &Taken {
            own: &own,
            by_device: Some(&said),
        },
    );
    assert_eq!(listed[0]["accepts"], serde_json::json!(["clip.share"]));
    assert!(!listed.to_string().contains("secret.other"));
    // Another version of its service is not its service.
    let older = [peersus_net::streams::ServiceRef {
        service: "clip.share".into(),
        version: 2,
    }];
    let listed = peers_listing_with(
        &aliases,
        &clip,
        &asks_send(),
        std::slice::from_ref(&peer),
        &Taken {
            own: &older,
            by_device: Some(&said),
        },
    );
    assert_eq!(listed[0]["accepts"], serde_json::json!([]));
    // A device that said nothing: not known, so not said.
    let listed = peers_listing_with(
        &aliases,
        &clip,
        &asks_send(),
        std::slice::from_ref(&peer),
        &Taken {
            own: &own,
            by_device: Some(&std::collections::BTreeMap::new()),
        },
    );
    assert!(listed[0].get("accepts").is_none());
}

/// A status a device shows, in a chooser's test.
fn showing_status(plugin: &str, id: &str, label: &str) -> peersus_core::plugins::StatusInfo {
    use peersus_core::plugins::{StatusId, StatusInfo, StatusKey, StatusLabel};
    StatusInfo {
        key: StatusKey {
            plugin: PluginId::new(plugin).unwrap(),
            id: StatusId::new(id).unwrap(),
        },
        label: StatusLabel::new(label).unwrap(),
    }
}

#[test]
fn a_chooser_offers_only_what_its_pick_allows_and_says_why_not_in_the_applications_words() {
    use peersus_api::elements::{Pick, Presence, Refusal, Show, StatusMatch};
    let clip = PluginId::new("clip").unwrap();
    let mut peer = recognisable_peer(true);
    peer.statuses = vec![showing_status("player", "watching", "Watching a video")];
    let own = [peersus_net::streams::ServiceRef {
        service: "clip.share".into(),
        version: 1,
    }];
    let said = |services: &[&str]| {
        std::collections::BTreeMap::from([(
            peer.id.clone(),
            services
                .iter()
                .map(|service| peersus_net::streams::ServiceRef {
                    service: (*service).into(),
                    version: 1,
                })
                .collect::<Vec<_>>(),
        )])
    };
    let entries =
        |pick: &Pick, show: &Show, said: Option<&std::collections::BTreeMap<_, _>>, reads: bool| {
            device_entries(
                &clip,
                &asks_send(),
                std::slice::from_ref(&peer),
                &Taken {
                    own: &own,
                    by_device: said,
                },
                reads,
                pick,
                show,
            )
        };
    let refused = |pick: Pick, said: Option<&std::collections::BTreeMap<_, _>>, reads: bool| {
        entries(&pick, &Show::default(), said, reads)[0].refused
    };
    // Online and allowed: chosen; the window is given the owner's key to draw its picture by.
    let plain = entries(&Pick::default(), &Show::default(), None, false);
    assert_eq!(plain[0].device, peer.id.0);
    assert_eq!(
        (plain[0].presence, plain[0].refused),
        (Presence::Online, None)
    );
    // Not reachable the way it asks.
    let asleep = Pick {
        presence: vec![Presence::Sleeping],
        ..Pick::default()
    };
    assert_eq!(refused(asleep, None, false), Some(Refusal::Presence));
    // Not allowed everything it asks for this device, unless it does not ask that.
    let mut everything = asks_send();
    everything.insert(permission(HOST, "peers.more"));
    let not_all = device_entries(
        &clip,
        &everything,
        std::slice::from_ref(&peer),
        &Taken::default(),
        false,
        &Pick::default(),
        &Show::default(),
    );
    assert_eq!(not_all[0].refused, Some(Refusal::NotAllowed));
    let any = device_entries(
        &clip,
        &everything,
        std::slice::from_ref(&peer),
        &Taken::default(),
        false,
        &Pick {
            allowed: false,
            ..Pick::default()
        },
        &Show::default(),
    );
    assert_eq!(any[0].refused, None);
    // Its own service: chosen only where the device said it takes it; not said (an older device), as `peersus.peers` says.
    let takes = |service: &str| Pick {
        accepts: Some(service.into()),
        ..Pick::default()
    };
    assert_eq!(
        refused(takes("clip.share"), Some(&said(&["clip.share"])), false),
        None
    );
    assert_eq!(
        refused(takes("clip.share"), Some(&said(&[])), false),
        Some(Refusal::NotThere)
    );
    assert_eq!(refused(takes("clip.share"), None, false), None);
    // Another extension's service is never looked up, even where the device takes it: nothing is learnt of another's consent.
    assert_eq!(
        refused(
            takes("other.service"),
            Some(&said(&["other.service"])),
            false
        ),
        Some(Refusal::NotThere)
    );
    // By status: only with `status.watch`, and its words only then.
    let watching = |id: Option<&str>| Pick {
        status: Some(StatusMatch {
            plugin: "player".into(),
            id: id.map(Into::into),
        }),
        ..Pick::default()
    };
    assert_eq!(
        refused(watching(None), None, false),
        Some(Refusal::StatusNotAllowed)
    );
    assert_eq!(refused(watching(None), None, true), None);
    assert_eq!(refused(watching(Some("watching")), None, true), None);
    assert_eq!(
        refused(watching(Some("in-a-call")), None, true),
        Some(Refusal::NoStatus)
    );
    let words = Show {
        statuses: true,
        ..Show::default()
    };
    assert!(
        entries(&Pick::default(), &words, None, false)[0]
            .statuses
            .is_empty()
    );
    assert_eq!(
        entries(&Pick::default(), &words, None, true)[0].statuses,
        ["Watching a video"]
    );
    // A device it may not see is not in the chooser at all.
    let other = PluginId::new("other").unwrap();
    assert!(
        device_entries(
            &other,
            &asks_send(),
            std::slice::from_ref(&peer),
            &Taken::default(),
            true,
            &Pick::default(),
            &Show::default()
        )
        .is_empty()
    );
}

#[test]
fn nothing_is_carried_over_for_receive_until_the_user_has_granted_it() {
    let mut registry = Registry::default();
    registry
        .register(plugin_that_receives(Some("sink.write")))
        .unwrap();
    let nothing = [peer_granted("p", &[])];
    assert_eq!(host_grants(&registry, &nothing), set(&[]));
    // Another capability of the same plugin, or the right capability of another plugin,
    // says nothing about this one's `receive`.
    let wrong = [
        peer_granted("p", &[("sink", &[("sink", "sink.other")])]),
        peer_granted("q", &[("elsewhere", &[("sink", "sink.write")])]),
    ];
    assert_eq!(host_grants(&registry, &wrong), set(&[]));
}

#[test]
fn the_application_can_call_receive_on_a_plugin_that_offers_it() {
    let mut registry = Registry::default();
    registry
        .register(plugin_that_receives(Some("sink.write")))
        .unwrap();
    let imported = host_manifest(&registry)
        .imports
        .into_iter()
        .find(|i| i.provider == "sink" && i.service == "sink.service")
        .expect("the application must import the service a plugin receives through");
    assert!(
        imported
            .methods
            .contains(crate::plugin_host::RECEIVE_METHOD),
        "without this import the hub refuses every delivery to the plugin"
    );
}

#[test]
fn a_capability_that_guards_receive_is_declared_so_that_it_can_be_granted() {
    let mut registry = Registry::default();
    registry
        .register(plugin_that_receives(Some("sink.write")))
        .unwrap();
    assert!(
        host_manifest(&registry).declares("sink.write"),
        "an undeclared capability can never be granted, so deliveries would stay refused"
    );
}

#[test]
fn the_applications_own_manifest_is_always_one_it_would_accept_from_a_plugin() {
    // The application obeys the rules it sets: its manifest is validated like any other,
    // or it could not join its own hub. That includes the permission it guards `send`
    // with, which it must declare and describe like a plugin would.
    let mut registry = Registry::default();
    assert!(
        host_manifest(&registry).validate().is_ok(),
        "with no plugins"
    );
    registry.register(plugin_with_guarded_action()).unwrap();
    assert!(
        host_manifest(&registry).validate().is_ok(),
        "with an action"
    );
    registry
        .register(plugin_wanting_peers("clip", &["list", "send"]))
        .unwrap();
    let manifest = host_manifest(&registry);
    assert!(
        manifest.validate().is_ok(),
        "with a plugin that asks to send"
    );
    // The permission that guards sending is declared, described, and by the application.
    let description = manifest
        .capabilities
        .get(PEERS_SEND_CAPABILITY)
        .expect("the permission guarding send is not declared");
    assert!(
        description.split_whitespace().count() >= 5,
        "{description:?}"
    );
}

#[test]
fn a_plugin_that_does_not_receive_gets_no_receive_import() {
    let mut registry = Registry::default();
    registry.register(plugin_with_guarded_action()).unwrap();
    let manifest = host_manifest(&registry);
    assert!(
        manifest
            .imports
            .iter()
            .all(|i| !i.methods.contains(crate::plugin_host::RECEIVE_METHOD)),
        "nothing may be imported that the plugin never offered"
    );
}

#[test]
fn the_application_can_be_granted_what_a_plugin_action_requires() {
    let mut registry = Registry::default();
    registry.register(plugin_with_guarded_action()).unwrap();
    let hub = Hub::default();
    let host_generation = hub
        .register_host(HOST, host_manifest(&registry), BTreeSet::new())
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    let spec = plugin_with_guarded_action();
    let generation = hub
        .register("alpha", spec.manifest.clone(), BTreeSet::new())
        .unwrap();
    let alpha = hub.connect("alpha", generation).unwrap();
    let call = |id| Operation::Call {
        id,
        provider: "alpha".into(),
        service: "svc".into(),
        version: 1,
        method: "go".into(),
        args: serde_json::Value::Null,
        timeout_ms: 5_000,
    };
    // Nothing is granted yet: refused, which is the safe default.
    assert_eq!(host.submit(call(1)), Err(peersus_api::Error::Denied));
    // The user's decision to allow it must be expressible. If the application's own
    // manifest did not declare the capability, no grant could ever be recorded and
    // every guarded action would stay refused no matter what the user chose.
    hub.set_permissions(HOST, [permission("alpha", "cap")].into())
        .expect("the application must be able to hold the capability an action needs");
    host.submit(call(2)).unwrap();
    assert!(matches!(
        alpha.try_recv().unwrap(),
        Some(Delivery::Call { caller, .. }) if caller == HOST
    ));
}

/// A plugin whose only import is `service` at `version` from `provider`.
fn plugin_importing(
    name: &str,
    provider: &str,
    service: &str,
    version: u32,
    methods: &[&str],
) -> PluginSpec {
    use peersus_api::manifest::{Import, Manifest};
    PluginSpec {
        id: PluginId::new(name).unwrap(),
        manifest: Manifest {
            imports: vec![Import {
                provider: provider.to_owned(),
                service: service.to_owned(),
                version,
                methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                events: BTreeSet::new(),
            }],
            ..Manifest::default()
        },
        program: Some("unused".into()),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Default::default(),
    }
}

/// A plugin whose only import is the transfers service of the application, listening to these events.
fn plugin_listening_to_transfers(name: &str, events: &[&str]) -> PluginSpec {
    use peersus_api::manifest::{Import, Manifest};
    PluginSpec {
        id: PluginId::new(name).unwrap(),
        manifest: Manifest {
            imports: vec![Import {
                provider: HOST.to_owned(),
                service: crate::transfer_events::TRANSFERS_SERVICE.to_owned(),
                version: 1,
                methods: BTreeSet::new(),
                events: events.iter().map(|e| (*e).to_owned()).collect(),
            }],
            ..Manifest::default()
        },
        program: Some("unused".into()),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Default::default(),
    }
}

#[test]
fn transfers_are_offered_only_to_the_plugins_that_asked_for_them_and_there_is_only_reading() {
    use crate::transfer_events as events;
    let mut registry = Registry::default();
    registry
        .register(plugin_listening_to_transfers("fireworks", &["changed"]))
        .unwrap();
    registry
        .register(plugin_wanting_peers("clip", &["list"]))
        .unwrap();
    let exports = host_exports(&registry);
    let transfers = exports
        .iter()
        .find(|e| e.service == events::TRANSFERS_SERVICE)
        .expect("the service was not offered to the plugin that asked");
    assert_eq!(
        transfers.consumers,
        ["fireworks".to_owned()].into(),
        "a plugin that did not ask was offered the transfers"
    );
    // A plugin can only READ about transfers: the two reads are the only methods, each behind a permission, and neither can change anything.
    let mut reads: Vec<&str> = transfers.methods.keys().map(String::as_str).collect();
    reads.sort_unstable();
    assert_eq!(
        reads,
        ["active", "active-named"],
        "something other than a read can be called on transfers"
    );
    assert!(
        transfers
            .methods
            .values()
            .all(|member| member.capability.is_some()),
        "a read of transfers is open to anyone"
    );
    assert!(!transfers.description.trim().is_empty());
    // Nobody asked: nothing is offered.
    let mut none = Registry::default();
    none.register(plugin_wanting_peers("clip", &["list"]))
        .unwrap();
    assert!(
        host_exports(&none)
            .iter()
            .all(|e| e.service != events::TRANSFERS_SERVICE)
    );
}

#[test]
fn the_two_events_of_transfers_are_each_behind_a_permission_of_their_own_and_say_what_they_are_for()
{
    use crate::transfer_events as events;
    let mut registry = Registry::default();
    registry
        .register(plugin_listening_to_transfers(
            "fireworks",
            &["changed", "changed-named"],
        ))
        .unwrap();
    let exports = host_exports(&registry);
    let transfers = exports
        .iter()
        .find(|e| e.service == events::TRANSFERS_SERVICE)
        .unwrap();
    let guard = |event: &str| {
        transfers
            .events
            .get(event)
            .map(|member| member.capability.as_deref())
    };
    assert_eq!(
        guard(events::TRANSFERS_CHANGED),
        Some(Some(events::TRANSFERS_WATCH_CAPABILITY))
    );
    assert_eq!(
        guard(events::TRANSFERS_CHANGED_NAMED),
        Some(Some(events::TRANSFERS_NAMES_CAPABILITY))
    );
    assert_ne!(
        events::TRANSFERS_WATCH_CAPABILITY,
        events::TRANSFERS_NAMES_CAPABILITY,
        "watching and seeing names must be two decisions"
    );
    assert!(
        transfers
            .events
            .values()
            .all(|member| !member.description.trim().is_empty())
    );
    // No event is open: a plugin can be told nothing about a transfer without a permission.
    assert!(
        transfers
            .events
            .values()
            .all(|member| member.capability.is_some())
    );
}

#[test]
fn the_application_declares_both_permissions_so_a_person_can_be_asked_and_neither_is_granted_by_being_declared()
 {
    use crate::transfer_events as events;
    let mut registry = Registry::default();
    registry
        .register(plugin_listening_to_transfers(
            "fireworks",
            &["changed", "changed-named"],
        ))
        .unwrap();
    let manifest = host_manifest(&registry);
    assert!(manifest.validate().is_ok());
    for capability in [
        events::TRANSFERS_WATCH_CAPABILITY,
        events::TRANSFERS_NAMES_CAPABILITY,
    ] {
        assert!(
            manifest.declares(capability),
            "{capability} is not declared"
        );
    }
    // Declared is not granted: nothing is granted to the plugin until a person decides.
    assert!(
        host_grants(&registry, &[]).is_empty(),
        "a permission was granted without a decision"
    );
}

#[test]
fn a_plugin_that_listens_only_to_the_unnamed_event_cannot_even_be_granted_the_permission_to_see_names()
 {
    use crate::transfer_events as events;
    assert_eq!(
        grantable(&plugin_listening_to_transfers("quiet", &["changed"])),
        set(&[(HOST, events::TRANSFERS_WATCH_CAPABILITY)]),
        "a plugin that never asked for names was offered the permission to see them"
    );
    assert_eq!(
        grantable(&plugin_listening_to_transfers(
            "nosy",
            &["changed", "changed-named"]
        )),
        set(&[
            (HOST, events::TRANSFERS_WATCH_CAPABILITY),
            (HOST, events::TRANSFERS_NAMES_CAPABILITY)
        ])
    );
}

/// A plugin whose only import is the status service of the application, calling these methods and listening to these events.
fn plugin_using_statuses(name: &str, methods: &[&str], events: &[&str]) -> PluginSpec {
    use peersus_api::manifest::{Import, Manifest};
    PluginSpec {
        id: PluginId::new(name).unwrap(),
        manifest: Manifest {
            imports: vec![Import {
                provider: HOST.to_owned(),
                service: crate::status_service::STATUS_SERVICE.to_owned(),
                version: 1,
                methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                events: events.iter().map(|e| (*e).to_owned()).collect(),
            }],
            ..Manifest::default()
        },
        program: Some("unused".into()),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Default::default(),
    }
}

#[test]
fn a_plugin_can_only_be_granted_the_status_permissions_for_what_it_asked_to_do() {
    use crate::status_service as status;
    // Making a status and turning it on are two permissions: only the second is SEEN by the other devices.
    assert_eq!(
        grantable(&plugin_using_statuses("maker", &["define"], &[])),
        set(&[(HOST, status::STATUS_DEFINE_CAPABILITY)]),
        "a plugin that only makes statuses was offered the permission to show them"
    );
    assert_eq!(
        grantable(&plugin_using_statuses(
            "shower",
            &["define", "set", "clear"],
            &[]
        )),
        set(&[
            (HOST, status::STATUS_DEFINE_CAPABILITY),
            (HOST, status::STATUS_SET_CAPABILITY)
        ]),
        "set and clear are one permission"
    );
    // Reading what the others say is a third, and it is offered by the read and by the event alike.
    for (methods, events) in [(&["list"][..], &[][..]), (&[][..], &["changed"][..])] {
        assert_eq!(
            grantable(&plugin_using_statuses("reader", methods, events)),
            set(&[(HOST, status::STATUS_WATCH_CAPABILITY)]),
            "{methods:?} {events:?}"
        );
    }
    // `own` is the plugin's own and asks nothing; a plugin that imports nothing of it can be granted nothing.
    assert_eq!(
        grantable(&plugin_using_statuses("own", &["own"], &[])),
        set(&[])
    );
    assert_eq!(
        grantable(&plugin_using_statuses("none", &[], &[])),
        set(&[])
    );
}

#[test]
fn the_status_permissions_are_about_this_device_and_never_granted_for_one_of_the_others() {
    use crate::status_service as status;
    let everything =
        plugin_using_statuses("all", &["define", "set", "clear", "list"], &["changed"]);
    assert_eq!(
        grantable_without_a_device(&everything),
        set(&[
            (HOST, status::STATUS_DEFINE_CAPABILITY),
            (HOST, status::STATUS_SET_CAPABILITY),
            (HOST, status::STATUS_WATCH_CAPABILITY)
        ]),
        "a status permission was not offered once, for the whole application"
    );
    assert_eq!(
        grantable_for_a_device(&everything),
        set(&[]),
        "a status permission was offered for one device at a time"
    );
}

#[test]
fn statuses_are_offered_only_to_the_plugins_that_asked_for_them_and_each_method_but_own_is_behind_a_permission()
 {
    use crate::status_service as status;
    let mut registry = Registry::default();
    registry
        .register(plugin_using_statuses(
            "player",
            &["define", "set", "clear", "own"],
            &[],
        ))
        .unwrap();
    registry
        .register(plugin_wanting_peers("clip", &["list"]))
        .unwrap();
    let exports = host_exports(&registry);
    let service = exports
        .iter()
        .find(|e| e.service == status::STATUS_SERVICE)
        .expect("the service was not offered to the plugin that asked");
    assert_eq!(
        service.consumers,
        ["player".to_owned()].into(),
        "a plugin that did not ask was offered statuses"
    );
    let guard = |method: &str| {
        service
            .methods
            .get(method)
            .unwrap_or_else(|| panic!("{method} is not offered"))
            .capability
            .clone()
    };
    assert_eq!(
        guard("define").as_deref(),
        Some(status::STATUS_DEFINE_CAPABILITY)
    );
    assert_eq!(guard("set").as_deref(), Some(status::STATUS_SET_CAPABILITY));
    assert_eq!(
        guard("clear").as_deref(),
        Some(status::STATUS_SET_CAPABILITY),
        "clearing is turning off what set turned on"
    );
    assert_eq!(
        guard("list").as_deref(),
        Some(status::STATUS_WATCH_CAPABILITY)
    );
    assert_eq!(
        guard("own"),
        None,
        "what a plugin made itself is its own to read"
    );
    assert_eq!(
        service
            .events
            .get("changed")
            .and_then(|member| member.capability.as_deref()),
        Some(status::STATUS_WATCH_CAPABILITY),
        "the event that tells what the others say is open to anyone"
    );
    // Nobody asked: nothing is offered.
    let mut nobody = Registry::default();
    nobody
        .register(plugin_wanting_peers("clip", &["list"]))
        .unwrap();
    assert!(
        host_exports(&nobody)
            .iter()
            .all(|e| e.service != status::STATUS_SERVICE),
        "a service nobody asked for was offered"
    );
}

#[test]
fn the_application_describes_each_status_permission_in_its_own_manifest_in_the_words_of_the_one_copy()
 {
    use crate::status_service as status;
    let mut registry = Registry::default();
    registry
        .register(plugin_using_statuses(
            "all",
            &["define", "set", "list"],
            &["changed"],
        ))
        .unwrap();
    let manifest = host_manifest(&registry);
    for (capability, description) in [
        (
            status::STATUS_DEFINE_CAPABILITY,
            status::STATUS_DEFINE_DESCRIPTION,
        ),
        (
            status::STATUS_SET_CAPABILITY,
            status::STATUS_SET_DESCRIPTION,
        ),
        (
            status::STATUS_WATCH_CAPABILITY,
            status::STATUS_WATCH_DESCRIPTION,
        ),
    ] {
        assert_eq!(
            manifest.capabilities.get(capability).map(String::as_str),
            Some(description),
            "{capability}"
        );
    }
}

/// A plugin that asks the application for `peersus.peers` and nothing else.
fn plugin_wanting_peers(name: &str, methods: &[&str]) -> PluginSpec {
    plugin_importing(name, HOST, PEERS_SERVICE, 1, methods)
}

#[test]
fn nothing_the_application_offers_an_extension_is_unguarded_but_what_is_the_callers_own() {
    use peersus_api::manifest::{Import, Manifest};
    // One plugin that asks for every service the application has: every one of them is then offered.
    let mut services: BTreeSet<&str> = HOST_CAPABILITIES
        .iter()
        .map(|(service, ..)| *service)
        .collect();
    services.extend(CALLER_OWN_METHODS.iter().map(|(service, _)| *service));
    services.extend([
        PEERS_SERVICE,
        PLUGINS_SERVICE,
        PLATFORM_SERVICE,
        LANGUAGE_SERVICE,
        CONFIG_SERVICE,
    ]);
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: PluginId::new("everything").unwrap(),
            manifest: Manifest {
                imports: services
                    .iter()
                    .map(|service| Import {
                        provider: HOST.to_owned(),
                        service: (*service).to_owned(),
                        version: 1,
                        methods: Default::default(),
                        events: Default::default(),
                    })
                    .collect(),
                ..Manifest::default()
            },
            program: Some("unused".into()),
            arguments: Vec::new(),
            restart_policy: None,
            languages: Default::default(),
        })
        .unwrap();
    let exports = host_exports(&registry);
    let offered: BTreeSet<&str> = exports.iter().map(|e| e.service.as_str()).collect();
    assert_eq!(
        offered, services,
        "a service was not offered to the plugin that asked, or one that was not asked for was"
    );
    // And the hub accepts what the application then offers: one export it would refuse from a plugin (a description too long,
    // say) keeps the application's manifest from being updated, and then every page and action of every extension is refused.
    assert_eq!(host_manifest(&registry).validate(), Ok(()));
    for export in &exports {
        for (name, member) in export.methods.iter().chain(&export.events) {
            let own = CALLER_OWN_METHODS
                .iter()
                .any(|(service, method)| *service == export.service && method == name);
            match &member.capability {
                Some(capability) => {
                    assert!(
                        !own,
                        "{} {name} is the caller's own and yet guarded",
                        export.service
                    );
                    // Every guard is one the person can be shown and grant.
                    assert!(
                        host_capability_description(capability).is_some(),
                        "{} {name} is guarded by {capability}, which no screen can name",
                        export.service
                    );
                }
                None => assert!(
                    own,
                    "{} {name} is offered without a permission and is not the caller's own",
                    export.service
                ),
            }
        }
    }
}

#[test]
fn every_permission_of_the_application_is_decided_to_be_per_device_or_about_none() {
    // What a plugin may do for one device and not another: these, and only these, are granted per device.
    let per_device = [PEERS_SEND_CAPABILITY, PEERS_WATCH_CAPABILITY];
    for (service, capability, _) in HOST_CAPABILITIES {
        let permission = Permission::new(HOST, *capability).unwrap();
        assert_ne!(
            needs_no_device(&permission),
            per_device.contains(capability),
            "{service} {capability} must be either granted once (needs_no_device) or per device, and only one of them"
        );
    }
}

#[test]
fn every_guard_the_application_offers_can_be_granted_to_the_plugin_that_imports_it_and_only_to_it()
{
    use peersus_api::manifest::{Import, Manifest};
    let mut services: BTreeSet<&str> = HOST_CAPABILITIES
        .iter()
        .map(|(service, ..)| *service)
        .collect();
    services.extend(CALLER_OWN_METHODS.iter().map(|(service, _)| *service));
    services.extend([
        PEERS_SERVICE,
        PLUGINS_SERVICE,
        PLATFORM_SERVICE,
        LANGUAGE_SERVICE,
        CONFIG_SERVICE,
        crate::system_readings::SERVICE,
    ]);
    let spec = |methods: &[&str], events: &[&str], service: &str| PluginSpec {
        id: PluginId::new("asker").unwrap(),
        manifest: Manifest {
            imports: vec![Import {
                provider: HOST.to_owned(),
                service: service.to_owned(),
                version: 1,
                methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                events: events.iter().map(|e| (*e).to_owned()).collect(),
            }],
            ..Manifest::default()
        },
        program: Some("unused".into()),
        arguments: Vec::new(),
        restart_policy: None,
        languages: Default::default(),
    };
    let mut registry = Registry::default();
    for (index, service) in services.iter().enumerate() {
        let mut asker = spec(&[], &[], service);
        asker.id = PluginId::new(format!("asker{index}")).unwrap();
        registry.register(asker).unwrap();
    }
    let mut checked = 0;
    for export in host_exports(&registry) {
        for (name, member, event) in export
            .methods
            .iter()
            .map(|(n, m)| (n, m, false))
            .chain(export.events.iter().map(|(n, m)| (n, m, true)))
        {
            let Some(capability) = &member.capability else {
                continue;
            };
            let wanted = Permission::new(HOST, capability.as_str()).unwrap();
            let asking = if event {
                spec(&[], &[name], &export.service)
            } else {
                spec(&[name], &[], &export.service)
            };
            assert!(
                grantable(&asking).contains(&wanted),
                "{} {name} is guarded by {capability}, which the plugin that imports it can never be granted",
                export.service
            );
            checked += 1;
        }
        // Importing the service and nothing of it is offered nothing.
        assert!(
            grantable(&spec(&[], &[], &export.service)).is_empty(),
            "{}",
            export.service
        );
    }
    // Every part of the device, and seeing a device, among them.
    assert!(checked >= HOST_CAPABILITIES.len(), "{checked}");
}

#[test]
fn the_application_offers_nothing_when_no_plugin_asked_for_it() {
    let mut registry = Registry::default();
    registry.register(plugin_with_guarded_action()).unwrap();
    assert!(
        host_exports(&registry).is_empty(),
        "a service nobody asked for is surface for nothing"
    );
}

#[test]
fn the_application_offers_peers_only_to_the_plugins_that_asked_for_it() {
    let mut registry = Registry::default();
    registry
        .register(plugin_wanting_peers("clip", &["list", "send"]))
        .unwrap();
    registry
        .register(plugin_wanting_peers("lister", &["list"]))
        .unwrap();
    registry.register(plugin_with_guarded_action()).unwrap();
    let exports = host_exports(&registry);
    assert_eq!(exports.len(), 1);
    let peers = &exports[0];
    assert_eq!((peers.service.as_str(), peers.version), (PEERS_SERVICE, 1));
    // Exactly the plugins that imported it: not the one with only an action, and not
    // any plugin that happens to exist.
    assert_eq!(
        peers.consumers,
        ["clip".to_owned(), "lister".to_owned()].into()
    );
    // Listing is guarded by the device permission it lists by: a plugin learns of a device only through a grant for it.
    assert_eq!(
        peers.methods.get("list").map(|m| m.capability.as_deref()),
        Some(Some(PEERS_WATCH_CAPABILITY))
    );
    assert_eq!(
        peers.methods.get("send").map(|m| m.capability.as_deref()),
        Some(Some(PEERS_SEND_CAPABILITY))
    );
    // And every part of what the application offers says what it is for.
    assert!(!peers.description.trim().is_empty());
    assert!(
        peers
            .methods
            .values()
            .all(|m| !m.description.trim().is_empty())
    );
}

#[test]
fn the_transfers_export_and_its_permissions_follow_who_asks_for_them_in_both_directions() {
    use crate::transfer_events as events;
    // Nobody asks: the service is not in what the application offers.
    let mut registry = Registry::default();
    registry
        .register(plugin_wanting_peers("clip", &["list"]))
        .unwrap();
    assert!(
        host_exports(&registry)
            .iter()
            .all(|export| export.service != events::TRANSFERS_SERVICE)
    );
    // A plugin asks: the service appears with both events, and the manifest the application publishes declares what guards them.
    registry
        .register(plugin_listening_to_transfers(
            "fireworks",
            &["changed", "changed-named"],
        ))
        .unwrap();
    let exports = host_exports(&registry);
    let offered = exports
        .iter()
        .find(|export| export.service == events::TRANSFERS_SERVICE)
        .expect("asked for, and not offered");
    assert_eq!(offered.events.len(), 2);
    assert!(host_manifest(&registry).declares(events::TRANSFERS_WATCH_CAPABILITY));
    // The plugin is removed: the offer goes with it, from a manifest built again from the registry and not from one that was kept.
    registry.remove(&PluginId::new("fireworks").unwrap());
    assert!(
        host_exports(&registry)
            .iter()
            .all(|export| export.service != events::TRANSFERS_SERVICE),
        "a stale export went on offering transfers to nobody"
    );
}

#[test]
fn through_the_real_hub_a_plugin_without_the_permission_cannot_even_ask_to_listen() {
    use crate::transfer_events as events;
    let (host, fireworks, _hub) = hub_with_transfers_plugin(&[]);
    for event in [events::TRANSFERS_CHANGED, events::TRANSFERS_CHANGED_NAMED] {
        assert_eq!(
            fireworks.submit(listen_to(event)),
            Err(peersus_api::error::Error::Denied),
            "{event} was open to a plugin with no permission"
        );
    }
    tell(
        &host,
        events::TRANSFERS_CHANGED,
        serde_json::json!({ "id": "t1" }),
    );
    assert!(
        fireworks.try_recv().unwrap().is_none(),
        "a plugin with no permission was told something"
    );
}

#[test]
fn through_the_real_hub_watching_gets_the_unnamed_event_and_never_the_named_one() {
    use crate::transfer_events as events;
    let (host, fireworks, _hub) = hub_with_transfers_plugin(&[events::TRANSFERS_WATCH_CAPABILITY]);
    fireworks
        .submit(listen_to(events::TRANSFERS_CHANGED))
        .unwrap();
    assert_eq!(
        fireworks.submit(listen_to(events::TRANSFERS_CHANGED_NAMED)),
        Err(peersus_api::error::Error::Denied),
        "a plugin allowed only to watch could listen for names"
    );
    tell(
        &host,
        events::TRANSFERS_CHANGED,
        serde_json::json!({ "id": "t1", "state": "complete" }),
    );
    tell(
        &host,
        events::TRANSFERS_CHANGED_NAMED,
        serde_json::json!({ "id": "t1", "name": "secret.pdf" }),
    );
    let told = fireworks.try_recv().unwrap();
    assert!(
        matches!(&told, Some(Delivery::Event { data, .. }) if data["id"] == "t1" && data.get("name").is_none()),
        "{told:?}"
    );
    assert!(
        fireworks.try_recv().unwrap().is_none(),
        "the named event reached a plugin that was not allowed names"
    );
}

#[test]
fn through_the_real_hub_the_permission_to_see_names_is_what_lets_a_plugin_hear_the_named_event() {
    use crate::transfer_events as events;
    let (host, fireworks, _hub) = hub_with_transfers_plugin(&[events::TRANSFERS_NAMES_CAPABILITY]);
    fireworks
        .submit(listen_to(events::TRANSFERS_CHANGED_NAMED))
        .unwrap();
    tell(
        &host,
        events::TRANSFERS_CHANGED_NAMED,
        serde_json::json!({ "id": "t1", "name": "secret.pdf" }),
    );
    assert!(
        matches!(fireworks.try_recv().unwrap(), Some(Delivery::Event { data, .. }) if data["name"] == "secret.pdf")
    );
}

#[test]
fn through_the_real_hub_withdrawing_the_permission_ends_the_events_at_once() {
    use crate::transfer_events as events;
    let (host, fireworks, hub) = hub_with_transfers_plugin(&[events::TRANSFERS_WATCH_CAPABILITY]);
    fireworks
        .submit(listen_to(events::TRANSFERS_CHANGED))
        .unwrap();
    tell(
        &host,
        events::TRANSFERS_CHANGED,
        serde_json::json!({ "id": "t1" }),
    );
    hub.set_permissions("fireworks", BTreeSet::new()).unwrap();
    assert!(
        fireworks.try_recv().unwrap().is_none(),
        "an event queued before the withdrawal was delivered after it"
    );
    tell(
        &host,
        events::TRANSFERS_CHANGED,
        serde_json::json!({ "id": "t2" }),
    );
    assert!(
        fireworks.try_recv().unwrap().is_none(),
        "a plugin whose permission was withdrawn was still told"
    );
}

#[test]
fn every_text_the_application_exports_fits_in_what_a_manifest_allows() {
    let mut registry = Registry::default();
    registry
        .register(plugin_listening_to_transfers(
            "fireworks",
            &["changed", "changed-named"],
        ))
        .unwrap();
    registry
        .register(plugin_wanting_peers("clip", &["list", "send"]))
        .unwrap();
    registry
        .register(plugin_managing("watchdog", &["list", "restart"]))
        .unwrap();
    let limit = peersus_api::manifest::MAX_DESCRIPTION;
    for export in host_exports(&registry) {
        let mut texts = vec![(
            format!("{} (service)", export.service),
            export.description.clone(),
        )];
        for (member, details) in export.methods.iter().chain(&export.events) {
            texts.push((
                format!("{}.{member}", export.service),
                details.description.clone(),
            ));
        }
        for (what, text) in texts {
            let length = text.chars().count();
            assert!(
                length <= limit,
                "{what} is {length} characters and a manifest allows {limit}"
            );
        }
    }
}

/// A plugin that imports `peersus.plugins` from the application, with these methods.
fn plugin_managing(name: &str, methods: &[&str]) -> PluginSpec {
    plugin_importing(name, HOST, PLUGINS_SERVICE, 1, methods)
}

#[test]
fn the_application_offers_the_plugin_service_only_to_plugins_that_asked_for_it() {
    let mut registry = Registry::default();
    registry
        .register(plugin_managing("watchdog", &["list", "restart"]))
        .unwrap();
    registry
        .register(plugin_wanting_peers("clip", &["list", "send"]))
        .unwrap();
    let exports = host_exports(&registry);
    let find = |service: &str| exports.iter().find(|e| e.service == service);
    // Each service is offered to exactly the plugins that imported THAT service: asking
    // for the devices gives no right to see the plugins, and the other way round.
    assert_eq!(
        find(PLUGINS_SERVICE).map(|e| e.consumers.clone()),
        Some(["watchdog".to_owned()].into())
    );
    assert_eq!(
        find(PEERS_SERVICE).map(|e| e.consumers.clone()),
        Some(["clip".to_owned()].into())
    );
}

#[test]
fn nothing_about_plugins_is_offered_when_no_plugin_asked_for_it() {
    let mut registry = Registry::default();
    registry
        .register(plugin_wanting_peers("clip", &["list", "send"]))
        .unwrap();
    assert!(
        host_exports(&registry)
            .iter()
            .all(|e| e.service != PLUGINS_SERVICE)
    );
}

#[test]
fn listing_plugins_and_restarting_one_are_each_behind_a_permission_and_both_are_described() {
    let mut registry = Registry::default();
    registry
        .register(plugin_managing("watchdog", &["list", "restart"]))
        .unwrap();
    let exports = host_exports(&registry);
    let plugins = exports
        .iter()
        .find(|e| e.service == PLUGINS_SERVICE)
        .expect("the plugin service was not offered");
    // Which extensions are installed is the person's to tell: seeing them, and being told when one changes, is
    // behind a permission apart from the one that restarts them.
    assert_eq!(
        plugins.methods.get("list").map(|m| m.capability.as_deref()),
        Some(Some(PLUGINS_WATCH_CAPABILITY))
    );
    assert_eq!(
        plugins
            .events
            .get(PLUGINS_STATE_CHANGED)
            .map(|m| m.capability.as_deref()),
        Some(Some(PLUGINS_WATCH_CAPABILITY))
    );
    // Restarting can interrupt any plugin, so it is behind a permission of the
    // application's own, which a person must grant.
    assert_eq!(
        plugins
            .methods
            .get("restart")
            .map(|m| m.capability.as_deref()),
        Some(Some(PLUGINS_RESTART_CAPABILITY))
    );
    assert!(plugins.events.contains_key("state-changed"));
    assert!(!plugins.description.trim().is_empty());
    assert!(
        plugins
            .methods
            .values()
            .all(|m| !m.description.trim().is_empty())
    );
    assert!(
        plugins
            .events
            .values()
            .all(|m| !m.description.trim().is_empty())
    );
}

#[test]
fn the_permission_to_restart_says_plainly_how_much_it_allows() {
    // It lets a plugin interrupt ANY other plugin, which is among the most a plugin can
    // be allowed. The words a person reads must not understate it.
    let text = PLUGINS_RESTART_DESCRIPTION.to_lowercase();
    assert!(text.contains("any"), "{PLUGINS_RESTART_DESCRIPTION}");
    assert!(text.contains("plugin"), "{PLUGINS_RESTART_DESCRIPTION}");
    assert!(
        text.contains("interrupt") || text.contains("stop"),
        "{PLUGINS_RESTART_DESCRIPTION}"
    );
}

#[test]
fn the_applications_own_manifest_stays_valid_with_the_plugin_service_too() {
    let mut registry = Registry::default();
    registry
        .register(plugin_managing("watchdog", &["list", "restart"]))
        .unwrap();
    registry
        .register(plugin_wanting_peers("clip", &["list", "send"]))
        .unwrap();
    let manifest = host_manifest(&registry);
    assert!(manifest.validate().is_ok());
    // Each permission it guards a method with is declared and described by the
    // application, because the application is the author of both services.
    for capability in [PEERS_SEND_CAPABILITY, PLUGINS_RESTART_CAPABILITY] {
        assert!(
            manifest.declares(capability),
            "{capability} is not declared"
        );
    }
}

#[test]
fn a_plugin_that_imports_the_plugin_service_can_be_granted_the_permission_to_restart() {
    // What can be granted is derived from the roles a plugin plays: importing `restart`
    // makes the application's guard on it grantable, and importing `list` its guard on seeing them.
    assert_eq!(
        grantable(&plugin_managing("watchdog", &["list", "restart"])),
        set(&[
            (HOST, PLUGINS_RESTART_CAPABILITY),
            (HOST, PLUGINS_WATCH_CAPABILITY)
        ])
    );
    assert_eq!(
        grantable(&plugin_managing("viewer", &["list"])),
        set(&[(HOST, PLUGINS_WATCH_CAPABILITY)])
    );
}

fn standalone(plugin: &str, granted: &[(&str, &str)]) -> BTreeMap<PluginId, Grants> {
    BTreeMap::from([(
        PluginId::new(plugin).unwrap(),
        Grants::new(granted.iter().map(|(o, n)| permission(o, n))).unwrap(),
    )])
}

#[test]
fn restarting_is_granted_outright_and_never_for_a_device() {
    let spec = plugin_managing("watchdog", &["list", "restart"]);
    assert_eq!(
        grantable_without_a_device(&spec),
        set(&[
            (HOST, PLUGINS_RESTART_CAPABILITY),
            (HOST, PLUGINS_WATCH_CAPABILITY)
        ])
    );
    // It is about no device, so a device is not a place to grant it.
    assert_eq!(grantable_for_a_device(&spec), set(&[]));
    // And the other way round: sending to a device and seeing it are about a device and cannot be granted outright.
    let sender = plugin_wanting_peers("clip", &["list", "send"]);
    assert_eq!(grantable_without_a_device(&sender), set(&[]));
    assert_eq!(
        grantable_for_a_device(&sender),
        set(&[
            (HOST, PEERS_SEND_CAPABILITY),
            (HOST, PEERS_WATCH_CAPABILITY)
        ])
    );
}

#[test]
fn the_two_kinds_never_overlap_and_together_are_everything_grantable() {
    let mut spec = plugin_managing("both", &["list", "restart"]);
    spec.manifest.imports.push(
        plugin_wanting_peers("both", &["list", "send"])
            .manifest
            .imports
            .remove(0),
    );
    let all = grantable(&spec);
    let device = grantable_for_a_device(&spec);
    let none = grantable_without_a_device(&spec);
    assert!(device.is_disjoint(&none));
    assert_eq!(device.union(&none).cloned().collect::<BTreeSet<_>>(), all);
    assert_eq!(all.len(), 4);
}

#[test]
fn what_a_window_is_told_a_plugin_can_be_granted_is_exactly_the_owners_two_sets() {
    let mut spec = plugin_managing("both", &["list", "restart"]);
    spec.manifest.imports.push(
        plugin_wanting_peers("both", &["list", "send"])
            .manifest
            .imports
            .remove(0),
    );
    // A name the plugin merely listed in its own manifest: not grantable, so not shown.
    spec.manifest
        .capabilities
        .insert("just.listed".into(), "Only listed by the plugin.".into());

    let told = grantable_for_window(&spec);
    let device: BTreeSet<_> = told.per_device.iter().cloned().collect();
    let outright: BTreeSet<_> = told.outright.iter().cloned().collect();
    assert_eq!(device, grantable_for_a_device(&spec));
    assert_eq!(outright, grantable_without_a_device(&spec));
    assert!(
        device.is_disjoint(&outright),
        "a permission is offered both ways"
    );
    assert!(
        !device
            .iter()
            .chain(&outright)
            .any(|p| p.name == "just.listed"),
        "a name the plugin only listed was offered as a permission"
    );
    // The same plugin gives the same answer, so the window's rows do not reshuffle.
    assert_eq!(grantable_for_window(&spec), told);
}

#[test]
fn two_quick_flips_started_from_the_same_picture_never_undo_each_other() {
    let a = Permission::new("host", "peers.send").unwrap();
    let b = Permission::new("host", "plugins.restart").unwrap();
    let start = Grants::new([a.clone()]).unwrap();

    // The window saw {A} and flipped A off, then, before the answer came, flipped B on. Each is
    // applied to what the owner holds when it ARRIVES, not to the picture the window had.
    let after_first = applied(&start, &a, false).unwrap();
    let after_second = applied(&after_first, &b, true).unwrap();
    assert!(
        !after_second.allows(&a),
        "a withdrawal that was made was undone by the next flip"
    );
    assert!(after_second.allows(&b));
    assert_eq!(after_second.iter().count(), 1);

    // The other order gives the same end: the flips are about different permissions.
    let other = applied(&applied(&start, &b, true).unwrap(), &a, false).unwrap();
    assert_eq!(other, after_second);
    // Allowing what is allowed, or withdrawing what is not, changes nothing.
    assert_eq!(applied(&start, &a, true).unwrap(), start);
    assert_eq!(applied(&start, &b, false).unwrap(), start);
}

#[test]
fn a_plugin_holds_restart_at_the_hub_when_granted_outright_with_no_device_paired() {
    let spec = plugin_managing("watchdog", &["list", "restart"]);
    // No device is paired at all, which is the case that made this necessary.
    assert_eq!(
        plugin_grants(
            &spec,
            &[],
            &standalone("watchdog", &[(HOST, PLUGINS_RESTART_CAPABILITY)])
        ),
        set(&[(HOST, PLUGINS_RESTART_CAPABILITY)])
    );
    // Not granted, nothing held.
    assert_eq!(plugin_grants(&spec, &[], &BTreeMap::new()), set(&[]));
}

#[test]
fn what_was_granted_outright_to_another_plugin_says_nothing_about_this_one() {
    let spec = plugin_managing("watchdog", &["list", "restart"]);
    assert_eq!(
        plugin_grants(
            &spec,
            &[],
            &standalone("someone-else", &[(HOST, PLUGINS_RESTART_CAPABILITY)])
        ),
        set(&[])
    );
}

#[test]
fn a_plugin_that_never_imported_restart_does_not_hold_it_even_if_granted_outright() {
    let viewer = plugin_managing("viewer", &["list"]);
    assert_eq!(
        plugin_grants(
            &viewer,
            &[],
            &standalone("viewer", &[(HOST, PLUGINS_RESTART_CAPABILITY)])
        ),
        set(&[])
    );
}

#[test]
fn nothing_the_person_can_grant_lets_one_extension_hold_another_extensions_capability() {
    // A guarded method is for the application to call: there is no grant between two extensions, so an extension
    // that imports another's is offered nothing to be allowed and holds nothing at the hub, whatever was written.
    let spec = plugin_importing("consumer", "provider", "example.thing", 1, &["go"]);
    assert_eq!(grantable(&spec), set(&[]));
    assert_eq!(
        plugin_grants(
            &spec,
            &[],
            &standalone("consumer", &[("provider", "use.go")])
        ),
        set(&[])
    );
}

#[test]
fn a_plugin_holds_a_status_permission_at_the_hub_only_when_it_was_granted_outright_to_that_plugin()
{
    use crate::status_service as status;
    let spec = plugin_using_statuses("player", &["define", "set", "clear"], &[]);
    let both = [
        (HOST, status::STATUS_DEFINE_CAPABILITY),
        (HOST, status::STATUS_SET_CAPABILITY),
    ];
    // No device is paired at all: the permissions are about this device, so this must hold.
    assert_eq!(
        plugin_grants(&spec, &[], &standalone("player", &both)),
        set(&both)
    );
    // Granted one of them: that one only, so making a status is not turning it on.
    assert_eq!(
        plugin_grants(
            &spec,
            &[],
            &standalone("player", &[(HOST, status::STATUS_DEFINE_CAPABILITY)])
        ),
        set(&[(HOST, status::STATUS_DEFINE_CAPABILITY)])
    );
    // Not granted, nothing held: a plugin can do nothing to a status until the person says so.
    assert_eq!(plugin_grants(&spec, &[], &BTreeMap::new()), set(&[]));
    // What was granted to another plugin says nothing about this one.
    assert_eq!(
        plugin_grants(&spec, &[], &standalone("someone-else", &both)),
        set(&[])
    );
}

#[test]
fn a_plugin_that_never_imported_what_a_status_permission_guards_does_not_hold_it_even_if_granted() {
    use crate::status_service as status;
    // It reads and nothing else: being granted the permission to show a status gives it no way to do it.
    let reader = plugin_using_statuses("reader", &["list"], &[]);
    assert_eq!(
        plugin_grants(
            &reader,
            &[],
            &standalone(
                "reader",
                &[
                    (HOST, status::STATUS_SET_CAPABILITY),
                    (HOST, status::STATUS_DEFINE_CAPABILITY)
                ]
            )
        ),
        set(&[]),
        "a plugin held a permission for something it never asked to do"
    );
    assert_eq!(
        plugin_grants(
            &reader,
            &[],
            &standalone("reader", &[(HOST, status::STATUS_WATCH_CAPABILITY)])
        ),
        set(&[(HOST, status::STATUS_WATCH_CAPABILITY)])
    );
}

#[test]
fn a_device_saying_a_status_permission_was_granted_does_not_give_it_to_the_plugin() {
    use crate::status_service as status;
    // Nothing should ever store this on a device, but a stored file is only ever as good as what wrote it. These permissions are about
    // no device, so a device cannot supply them: otherwise a paired device could make a plugin show anything to everyone.
    let spec = plugin_using_statuses("player", &["define", "set"], &[]);
    let peer = granted_exactly("a", "player", HOST, status::STATUS_SET_CAPABILITY);
    assert_eq!(
        plugin_grants(&spec, std::slice::from_ref(&peer), &BTreeMap::new()),
        set(&[])
    );
}

/// A status that is on is said by every paired device, and without `status.set` its plugin can no longer turn it off: withdrawing
/// the permission turns it off, so nothing goes on being said without the person's leave.
#[tokio::test]
async fn withdrawing_the_permission_to_show_a_status_turns_off_what_it_showed() {
    use crate::status_service as status;
    use serde_json::json;
    let spec = plugin_using_statuses("player", &["define", "set", "clear"], &[]);
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec).unwrap();
    let root = tempfile::tempdir().unwrap();
    let network_settings = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..Default::default()
    };
    let endpoint = peersus_net::transport::bind(iroh::SecretKey::generate(), &network_settings)
        .await
        .unwrap();
    let network = NetworkHost::new(
        endpoint,
        "Test".into(),
        peersus_net::peers::Peers::open(root.path().join("peers.json")).unwrap(),
        peersus_net::history::History::open(root.path().join("history.json")).unwrap(),
    );
    let settings =
        peersus_net::settings::SettingsStore::open(root.path().join("preferences.json")).unwrap();
    let grants =
        peersus_net::grants::StandaloneGrants::open(root.path().join("grants.json")).unwrap();
    let (mut runtime, _client) = Runtime::new(
        network,
        grants,
        settings,
        registry,
        root.path().join("sockets"),
    )
    .unwrap();
    let granted =
        |names: &[&str]| Grants::new(names.iter().map(|name| permission(HOST, name))).unwrap();
    runtime
        .set_grants(
            plugin.clone(),
            None,
            granted(&[
                status::STATUS_DEFINE_CAPABILITY,
                status::STATUS_SET_CAPABILITY,
            ]),
        )
        .unwrap();
    let ask = |runtime: &mut Runtime, method: &str, args: serde_json::Value| {
        runtime
            .answer_plugin_call(plugin.as_str(), status::STATUS_SERVICE, method, &args)
            .0
    };
    for (method, args) in [
        (
            status::STATUS_DEFINE,
            json!({ "id": "busy", "label": "Busy" }),
        ),
        (status::STATUS_SET, json!({ "id": "busy" })),
    ] {
        assert!(matches!(
            ask(&mut runtime, method, args),
            peersus_api::message::Outcome::Ok { .. }
        ));
    }
    assert_eq!(runtime.statuses.active().len(), 1);
    // Another permission changing leaves what it showed alone.
    runtime
        .set_grants(
            plugin.clone(),
            None,
            granted(&[
                status::STATUS_DEFINE_CAPABILITY,
                status::STATUS_SET_CAPABILITY,
            ]),
        )
        .unwrap();
    assert_eq!(runtime.statuses.active().len(), 1);
    // `status.set` withdrawn: off at once.
    runtime
        .set_grants(
            plugin.clone(),
            None,
            granted(&[status::STATUS_DEFINE_CAPABILITY]),
        )
        .unwrap();
    assert!(runtime.statuses.active().is_empty());
}

#[test]
fn a_device_saying_restart_is_granted_does_not_give_it_to_the_plugin() {
    // Nothing should ever store this on a device, but a stored file is only ever as good
    // as what wrote it. The permission is about no device, so a device cannot supply it.
    let spec = plugin_managing("watchdog", &["list", "restart"]);
    let peers = [peer_granted(
        "p",
        &[("watchdog", &[(HOST, PLUGINS_RESTART_CAPABILITY)])],
    )];
    assert_eq!(device_grants(&spec, &peers), set(&[]));
}

#[test]
fn granting_outright_does_not_turn_a_device_permission_into_one_for_every_device() {
    // `peers.send` is about a device. Having it recorded outright must not make the
    // plugin hold it at the hub: that would grant it without any device's say.
    let spec = plugin_wanting_peers("clip", &["list", "send"]);
    assert_eq!(
        plugin_grants(
            &spec,
            &[],
            &standalone("clip", &[(HOST, PEERS_SEND_CAPABILITY)])
        ),
        set(&[])
    );
}

fn info(id: &str, status: peersus_core::model::PluginStatus) -> peersus_core::model::PluginInfo {
    // A manifest with recognisable content, so that a leak of any part of it is visible.
    let mut spec = plugin_wanting_peers(id, &["list", "send"]);
    spec.manifest.capabilities.insert(
        "secret.name".to_owned(),
        "A recognisable description.".to_owned(),
    );
    peersus_core::model::PluginInfo {
        id: PluginId::new(id).unwrap(),
        manifest: spec.manifest,
        standing: peersus_core::model::Standing::Ready,
        status,
        grantable: Default::default(),
        program: None,
        // A recognisable author and link, so that any leak of the package block into what plugins are told
        // about each other is caught by the test below.
        package: Some(peersus_api::package::Package {
            name: "Secret package".into(),
            version: "9.9.9".into(),
            description: String::new(),
            changelog: String::new(),
            author: "Secret author".into(),
            link: "https://secret.example/author".into(),
        }),
        agreed: false,
        from_store: false,
    }
}

#[test]
fn listing_plugins_says_each_ones_id_and_state_and_nothing_else() {
    use peersus_core::model::PluginStatus::{Failed, Running};
    let listed = plugins_listing(&[info("clip", Running), info("sink", Failed)]);
    assert_eq!(
        listed,
        serde_json::json!([
            {"id": "clip", "status": "running"},
            {"id": "sink", "status": "failed"},
        ])
    );
    // Not the manifest, what it declared, what it may do, or where it lives.
    let text = listed.to_string();
    for leaked in [
        "secret.name",
        "recognisable",
        "peersus.peers",
        "send",
        "program",
        "Secret author",
        "secret.example",
        "9.9.9",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked: {text}");
    }
}

#[test]
fn listing_no_plugins_is_an_empty_list_not_an_error() {
    assert_eq!(plugins_listing(&[]), serde_json::json!([]));
}

#[test]
fn a_restart_request_names_one_plugin_and_nothing_else_may_ride_along() {
    let ok = parse_restart(&serde_json::json!({"plugin": "clip"})).unwrap();
    assert_eq!(ok.as_str(), "clip");
    for bad in [
        serde_json::json!({}),
        serde_json::json!({"plugin": "clip", "force": true}),
        serde_json::json!({"plugin": ""}),
        serde_json::json!({"plugin": "../etc"}),
        serde_json::json!({"plugin": "Has Space"}),
        serde_json::json!({"plugin": 7}),
        serde_json::json!({"plugin": ["clip"]}),
        serde_json::json!("clip"),
        serde_json::Value::Null,
    ] {
        assert!(parse_restart(&bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn a_plugin_that_imports_something_almost_the_same_is_not_a_consumer() {
    let mut registry = Registry::default();
    registry
        .register(plugin_wanting_peers("real", &["list"]))
        .unwrap();
    // Another provider that happens to offer the same service name.
    registry
        .register(plugin_importing(
            "lookalike",
            "someone",
            PEERS_SERVICE,
            1,
            &["list"],
        ))
        .unwrap();
    // The application, but a different service.
    registry
        .register(plugin_importing(
            "elsewhere",
            HOST,
            "peersus.other",
            1,
            &["list"],
        ))
        .unwrap();
    // The right service at a version the application does not offer.
    registry
        .register(plugin_importing("stale", HOST, PEERS_SERVICE, 2, &["list"]))
        .unwrap();
    let exports = host_exports(&registry);
    assert_eq!(exports.len(), 1);
    assert_eq!(
        exports[0].consumers,
        ["real".to_owned()].into(),
        "only a plugin that really asked the application for this version may be served"
    );
}

/// The application and one plugin that asked for `peersus.peers`, joined to a hub as the
/// runtime joins them, with `granted` held by the plugin.
fn hub_with_peers_plugin(
    granted: &[&str],
) -> (peersus_plugin::hub::Channel, peersus_plugin::hub::Channel) {
    let mut registry = Registry::default();
    let mut spec = plugin_wanting_peers("clip", &["list", "send"]);
    // A plugin can only be granted what its own manifest declares.
    spec.manifest.capabilities.insert(
        PEERS_SEND_CAPABILITY.to_owned(),
        "Sends data to a device.".to_owned(),
    );
    registry.register(spec.clone()).unwrap();
    let hub = Hub::default();
    let host_generation = hub
        .register_host(HOST, host_manifest(&registry), BTreeSet::new())
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    // What a plugin asking the application to send holds is the APPLICATION's permission.
    let granted: BTreeSet<Permission> = granted.iter().map(|name| permission(HOST, name)).collect();
    let generation = hub
        .register("clip", spec.manifest.clone(), granted)
        .unwrap();
    let clip = hub.connect("clip", generation).unwrap();
    (host, clip)
}

/// The application and one plugin that listens to both events of `peersus.transfers`, joined to a hub as the runtime joins them, with `granted`
/// held by the plugin and the plugin already subscribed to the events its permissions allow it to ask for.
fn hub_with_transfers_plugin(
    granted: &[&str],
) -> (
    peersus_plugin::hub::Channel,
    peersus_plugin::hub::Channel,
    Hub,
) {
    use crate::transfer_events as events;
    let mut registry = Registry::default();
    let mut spec = plugin_listening_to_transfers("fireworks", &["changed", "changed-named"]);
    // A plugin can only be granted what its own manifest declares.
    for capability in [
        events::TRANSFERS_WATCH_CAPABILITY,
        events::TRANSFERS_NAMES_CAPABILITY,
    ] {
        spec.manifest.capabilities.insert(
            capability.to_owned(),
            "Lets the plugin hear about transfers.".to_owned(),
        );
    }
    registry.register(spec.clone()).unwrap();
    let hub = Hub::default();
    let host_generation = hub
        .register_host(HOST, host_manifest(&registry), BTreeSet::new())
        .unwrap();
    let host = hub.connect(HOST, host_generation).unwrap();
    let granted: BTreeSet<Permission> = granted.iter().map(|name| permission(HOST, name)).collect();
    let generation = hub
        .register("fireworks", spec.manifest.clone(), granted)
        .unwrap();
    let fireworks = hub.connect("fireworks", generation).unwrap();
    (host, fireworks, hub)
}

fn listen_to(event: &str) -> Operation {
    Operation::Subscribe {
        provider: HOST.into(),
        service: crate::transfer_events::TRANSFERS_SERVICE.into(),
        version: 1,
        event: event.into(),
    }
}

fn tell(host: &peersus_plugin::hub::Channel, event: &str, data: serde_json::Value) {
    let _ = host.submit(Operation::Publish {
        service: crate::transfer_events::TRANSFERS_SERVICE.into(),
        version: 1,
        event: event.into(),
        data,
    });
}

fn call_peers(id: u64, method: &str) -> Operation {
    Operation::Call {
        id,
        provider: HOST.into(),
        service: PEERS_SERVICE.into(),
        version: 1,
        method: method.into(),
        args: serde_json::Value::Null,
        timeout_ms: 5_000,
    }
}

#[test]
fn a_plugin_that_asked_can_neither_list_devices_nor_send_until_it_is_granted() {
    let (_host, clip) = hub_with_peers_plugin(&[]);
    // Both are refused at the hub, before the application is asked: a plugin allowed nothing for any device is not
    // told that any device exists.
    for (id, method) in [(1, "list"), (2, "send")] {
        assert_eq!(
            clip.submit(call_peers(id, method)),
            Err(peersus_api::Error::Denied),
            "{method}"
        );
    }
}

#[test]
fn seeing_devices_and_sending_to_them_are_each_opened_only_by_their_own_permission() {
    let reaches = |grants: &[&str], method: &str| {
        let (host, clip) = hub_with_peers_plugin(grants);
        clip.submit(call_peers(1, method)).is_ok()
            && matches!(
                host.try_recv().unwrap(),
                Some(Delivery::Call { caller, method: called, .. }) if caller == "clip" && called == method
            )
    };
    assert!(reaches(&[PEERS_WATCH_CAPABILITY], "list"));
    assert!(
        !reaches(&[PEERS_WATCH_CAPABILITY], "send"),
        "seeing a device is not sending to it"
    );
    assert!(reaches(&[PEERS_SEND_CAPABILITY], "send"));
    assert!(
        !reaches(&[PEERS_SEND_CAPABILITY], "list"),
        "sending is not being told every device"
    );
}

fn clip_spec() -> PluginSpec {
    let mut spec = plugin_wanting_peers("clip", &["list", "send"]);
    spec.manifest.capabilities.insert(
        PEERS_SEND_CAPABILITY.to_owned(),
        "Sends data to a device.".to_owned(),
    );
    spec
}

// What can be granted to a plugin is derived from the ROLES it plays, never from what it
// declares for itself: it is a permission of its own for what it offers, and a permission
// of the application for what it asks the application to do.

#[test]
fn what_can_be_granted_is_its_own_guards_and_the_applications_guards_it_imports() {
    // A plugin that asks the application to list devices and send to them: the application's guards on
    // both, and nothing else, because it offers nothing the application would call.
    assert_eq!(
        grantable(&clip_spec()),
        set(&[
            (HOST, PEERS_SEND_CAPABILITY),
            (HOST, PEERS_WATCH_CAPABILITY)
        ])
    );
    // A plugin that takes what devices send, guarded by its own permission: that
    // permission is the plugin's own, and the application's `send` is not offered to it
    // because it imports nothing from the application.
    assert_eq!(
        grantable(&plugin_that_receives(Some("sink.write"))),
        set(&[("sink", "sink.write")])
    );
    // A plugin that offers nothing guarded and imports nothing guarded has nothing to grant.
    assert_eq!(grantable(&plugin_that_receives(None)), set(&[]));
}

#[test]
fn a_permission_the_plugin_merely_declares_for_itself_is_not_grantable_as_the_applications() {
    // The plugin declares its own `peers.send` and imports nothing guarded by the
    // application. Granting that name must not produce the application's permission:
    // the name is the plugin's, with whatever meaning it chose.
    let mut spec = plugin_wanting_peers("evil", &["list"]);
    spec.manifest.capabilities.insert(
        PEERS_SEND_CAPABILITY.to_owned(),
        "Totally harmless.".to_owned(),
    );
    let got = grantable(&spec);
    assert!(
        !got.contains(&permission(HOST, PEERS_SEND_CAPABILITY)),
        "a declared name became the application's permission: {got:?}"
    );
}

/// What a plugin holds at the hub from what devices granted, when nothing was granted
/// outright.
fn device_grants(spec: &PluginSpec, peers: &[Peer]) -> BTreeSet<Permission> {
    plugin_grants(spec, peers, &BTreeMap::new())
}

#[test]
fn declaring_the_applications_name_does_not_change_whose_permission_the_hub_is_told_of() {
    // A plugin that imports `send` AND also declares its own `peers.send`: what it holds at
    // the hub is still the application's permission, once granted, and only that.
    let spec = clip_spec();
    let grant = [peer_granted(
        "p",
        &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])],
    )];
    assert_eq!(
        device_grants(&spec, &grant),
        set(&[(HOST, PEERS_SEND_CAPABILITY)])
    );
    // And a grant of a permission of the same name but of the plugin itself is a
    // different thing, and carries nothing to the hub for `send`.
    let own = [peer_granted(
        "p",
        &[("clip", &[("clip", PEERS_SEND_CAPABILITY)])],
    )];
    assert_eq!(device_grants(&spec, &own), set(&[]));
}

#[test]
fn a_plugin_holds_nothing_at_the_hub_until_a_device_granted_it_something() {
    let spec = clip_spec();
    assert_eq!(device_grants(&spec, &[]), set(&[]));
    let none = [peer_granted("p", &[])];
    assert_eq!(device_grants(&spec, &none), set(&[]));
}

#[test]
fn a_grant_on_any_one_device_lets_the_plugin_reach_send_at_the_hub() {
    let spec = clip_spec();
    let peers = [
        peer_granted("a", &[]),
        peer_granted("b", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
    ];
    assert_eq!(
        device_grants(&spec, &peers),
        set(&[(HOST, PEERS_SEND_CAPABILITY)])
    );
}

#[test]
fn what_was_granted_to_another_plugin_says_nothing_about_this_one() {
    let spec = clip_spec();
    let peers = [peer_granted(
        "a",
        &[("other", &[(HOST, PEERS_SEND_CAPABILITY)])],
    )];
    assert_eq!(device_grants(&spec, &peers), set(&[]));
}

#[test]
fn only_capabilities_the_plugin_really_asks_the_application_for_are_carried() {
    // It declared and was granted a capability, but never imported anything guarded by it.
    let mut spec = plugin_wanting_peers("clip", &["list"]);
    spec.manifest
        .capabilities
        .insert("clip.private".to_owned(), "Keeps private notes.".to_owned());
    spec.manifest.capabilities.insert(
        PEERS_SEND_CAPABILITY.to_owned(),
        "Sends data to a device.".to_owned(),
    );
    let peers = [peer_granted(
        "a",
        &[(
            "clip",
            &[("clip", "clip.private"), (HOST, PEERS_SEND_CAPABILITY)],
        )],
    )];
    assert_eq!(
        device_grants(&spec, &peers),
        set(&[]),
        "a plugin that only lists must not hold the capability that guards sending"
    );
}

#[test]
fn withdrawing_one_devices_grant_keeps_what_another_device_gave() {
    let spec = clip_spec();
    let both = [
        peer_granted("a", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
        peer_granted("b", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
    ];
    assert_eq!(
        device_grants(&spec, &both),
        set(&[(HOST, PEERS_SEND_CAPABILITY)])
    );
    let after = [
        peer_granted("a", &[]),
        peer_granted("b", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]),
    ];
    assert_eq!(
        device_grants(&spec, &after),
        set(&[(HOST, PEERS_SEND_CAPABILITY)])
    );
}

#[test]
fn a_device_that_granted_something_else_does_not_give_the_plugin_send() {
    let spec = clip_spec();
    // The entry for this plugin exists, so it is consulted, but it does not allow sending.
    let peers = [peer_granted("a", &[("clip", &[("clip", "clip.other")])])];
    assert_eq!(
        device_grants(&spec, &peers),
        set(&[]),
        "holding another capability on a device must not carry the one that guards sending"
    );
}

/// A device whose every field is recognisable in text, so a leak cannot hide.
fn recognisable_peer(online: bool) -> Peer {
    Peer {
        id: PeerId("KEY-9f3a77c1d2e4".into()),
        name: "Alex's laptop".into(),
        paired_at: 1_700_000_123,
        last_seen: Some(1_700_999_456),
        online,
        sleeping: false,
        statuses: Vec::new(),
        grants: [(
            PluginId::new("clip").unwrap(),
            Grants::new([
                permission(HOST, PEERS_SEND_CAPABILITY),
                permission(HOST, PEERS_WATCH_CAPABILITY),
            ])
            .unwrap(),
        )]
        .into(),
        plugin_values: Default::default(),
    }
}

/// What a plugin that sends to devices asks for each of them.
fn asks_send() -> BTreeSet<Permission> {
    [permission(HOST, PEERS_SEND_CAPABILITY)].into()
}

#[test]
fn a_plugin_is_told_of_a_device_only_once_it_was_allowed_to_see_it() {
    let aliases = crate::aliases::Aliases::new();
    let mut peer = recognisable_peer(true);
    // Allowed to see this device: listed.
    let clip = PluginId::new("clip").unwrap();
    assert_eq!(
        peers_listing(&aliases, &clip, &asks_send(), std::slice::from_ref(&peer))
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    // Another plugin allowed nothing for it is told of no device.
    let other = PluginId::new("other").unwrap();
    assert_eq!(
        peers_listing(&aliases, &other, &asks_send(), std::slice::from_ref(&peer)),
        serde_json::json!([])
    );
    // Allowed only to send to it, not to see it: not listed, and its settings are not read either.
    peer.grants.insert(
        other.clone(),
        Grants::new([permission(HOST, PEERS_SEND_CAPABILITY)]).unwrap(),
    );
    assert_eq!(
        peers_listing(&aliases, &other, &asks_send(), std::slice::from_ref(&peer)),
        serde_json::json!([])
    );
    let spec = plugin_importing("other", HOST, PEERS_SERVICE, 1, &["list", "send"]);
    assert_eq!(
        configuration(&spec, None, &aliases, std::slice::from_ref(&peer))["peers"],
        serde_json::json!([])
    );
}

#[test]
fn the_listing_names_a_device_and_says_how_it_stands_and_nothing_else() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let listing = peers_listing(&aliases, &clip, &asks_send(), &[recognisable_peer(true)]);
    let text = listing.to_string();
    for secret in [
        "KEY-9f3a77c1d2e4",
        "9f3a77c1d2e4",
        "1700000123",
        "1700999456",
        "peers.send",
        "paired_at",
        "last_seen",
        "grants",
    ] {
        assert!(!text.contains(secret), "the listing leaks {secret}: {text}");
    }
    let entries = listing.as_array().expect("a list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "Alex's laptop");
    assert_eq!(entries[0]["online"], true);
    assert_eq!(entries[0]["presence"], "online");
    // Exactly five things: how to name it, what to call it, whether it is connected, which of
    // the three words is true of it, and whether this plugin is allowed all it asks for it.
    assert_eq!(entries[0].as_object().unwrap().len(), 5, "{text}");
}

#[test]
fn a_device_that_sleeps_is_connected_and_said_to_sleep_and_one_that_is_gone_is_offline() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let asleep = Peer {
        sleeping: true,
        ..recognisable_peer(true)
    };
    // Not connected, whatever it last said of sleeping: there is nothing asleep to wake.
    let gone = Peer {
        sleeping: true,
        ..recognisable_peer(false)
    };
    let listing = peers_listing(&aliases, &clip, &asks_send(), &[asleep, gone]);
    assert_eq!(
        (&listing[0]["online"], &listing[0]["presence"]),
        (&serde_json::json!(true), &serde_json::json!("sleeping"))
    );
    assert_eq!(
        (&listing[1]["online"], &listing[1]["presence"]),
        (&serde_json::json!(false), &serde_json::json!("offline"))
    );
}

#[test]
fn a_device_is_allowed_only_when_everything_the_plugin_asks_for_it_was_granted_for_it() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let send = permission(HOST, PEERS_SEND_CAPABILITY);
    let other = permission("clip", "clip.use");
    // The peer was granted `peers.send` and `peers.watch` for this plugin and nothing else.
    let peer = recognisable_peer(true);
    let listed = |plugin: &PluginId, asks: &[&Permission]| {
        let asks: BTreeSet<Permission> = asks.iter().map(|asked| (*asked).clone()).collect();
        peers_listing(&aliases, plugin, &asks, std::slice::from_ref(&peer))[0]["allowed"].clone()
    };
    assert_eq!(listed(&clip, &[&send]), true);
    assert_eq!(
        listed(&clip, &[&send, &other]),
        false,
        "one of two is not all"
    );
    assert_eq!(listed(&clip, &[&other]), false);
    // What one plugin was granted says nothing for another, which is not even told of the device.
    assert_eq!(
        listed(&PluginId::new("other").unwrap(), &[&send]),
        serde_json::Value::Null
    );
}

#[test]
fn the_alias_in_the_listing_is_the_one_send_will_resolve() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let peer = recognisable_peer(true);
    let listing = peers_listing(&aliases, &clip, &asks_send(), std::slice::from_ref(&peer));
    let given = listing[0]["device"].as_str().expect("an alias").to_owned();
    assert_eq!(
        aliases.resolve(&clip, &given, [&peer.id]),
        Some(&peer.id),
        "what list hands out must be what send accepts"
    );
}

#[test]
fn an_offline_device_is_listed_as_offline_not_hidden() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let listing = peers_listing(&aliases, &clip, &asks_send(), &[recognisable_peer(false)]);
    assert_eq!(listing[0]["online"], false);
}

#[test]
fn no_paired_devices_is_an_empty_list_not_an_error() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    assert_eq!(
        peers_listing(&aliases, &clip, &BTreeSet::new(), &[]),
        serde_json::json!([])
    );
}

#[test]
fn two_plugins_are_given_different_names_for_the_same_device() {
    let aliases = crate::aliases::Aliases::new();
    let mut peer = recognisable_peer(true);
    peer.grants.insert(
        PluginId::new("other").unwrap(),
        Grants::new([permission(HOST, PEERS_SEND_CAPABILITY)]).unwrap(),
    );
    let first = peers_listing(
        &aliases,
        &PluginId::new("clip").unwrap(),
        &asks_send(),
        std::slice::from_ref(&peer),
    );
    let second = peers_listing(
        &aliases,
        &PluginId::new("other").unwrap(),
        &asks_send(),
        std::slice::from_ref(&peer),
    );
    assert_ne!(first[0]["device"], second[0]["device"]);
}

#[test]
fn a_plugin_that_answered_ok_has_done_it() {
    use peersus_core::model::ActionState;
    let ok = peersus_api::message::Outcome::ok(serde_json::json!({"anything": 1}));
    assert_eq!(action_state(&ok), ActionState::Done);
}

#[test]
fn every_code_the_hub_gives_becomes_the_reason_it_stands_for() {
    use peersus_api::message::Outcome;
    use peersus_core::model::{ActionFailure::*, ActionState::Failed};
    let cases = [
        ("denied", NotAllowed),
        ("refused", NotAllowed),
        ("unavailable", Unavailable),
        ("stopped", Stopped),
        ("timeout", TimedOut),
        ("invalid-deadline", TimedOut),
        ("busy", Busy),
        ("unknown-method", Other),
        ("version", Other),
        ("too-large", Other),
    ];
    for (code, reason) in cases {
        assert_eq!(
            action_state(&Outcome::error(code, "x")),
            Failed { reason },
            "{code}"
        );
    }
}

#[test]
fn what_a_plugin_writes_in_its_own_words_never_becomes_part_of_the_state() {
    use peersus_api::message::Outcome;
    use peersus_core::model::{ActionFailure, ActionState};
    // Codes and messages are the plugin's own text. None of it is a code the application
    // knows, so all of it reads the same and none of it is carried.
    for code in [
        "disk-full",
        "/home/alex/secret.txt",
        "",
        "DENIED",
        "denied ",
        "timeout\n",
    ] {
        let state = action_state(&Outcome::error(code, "the password is hunter2"));
        assert_eq!(
            state,
            ActionState::Failed {
                reason: ActionFailure::Other
            },
            "{code:?}"
        );
        assert!(!format!("{state:?}").contains("hunter2"));
    }
}

#[test]
fn the_message_of_an_error_makes_no_difference_to_its_state() {
    use peersus_api::message::Outcome;
    assert_eq!(
        action_state(&Outcome::error("denied", "first wording")),
        action_state(&Outcome::error("denied", "completely different words")),
    );
}

#[test]
fn a_failed_call_is_told_in_the_applications_own_words_by_the_reason_and_never_in_the_plugins() {
    use peersus_api::message::Outcome;
    let told =
        |code: &str| action_failure_sentence(&Outcome::error(code, "the password is hunter2"));
    assert_eq!(
        told("denied"),
        Some("A plugin was not allowed to complete the action")
    );
    assert_eq!(told("refused"), told("denied"));
    assert_eq!(
        told("timeout"),
        Some("A plugin did not complete the action in time")
    );
    assert_eq!(told("invalid-deadline"), told("timeout"));
    assert!(told("unavailable").unwrap().contains("not available"));
    // A plugin that ended with the call outstanding is said as that, not as something missing (the test extension's "Leave").
    assert_eq!(
        told("stopped"),
        Some("A plugin stopped before it finished the action")
    );
    assert!(told("busy").unwrap().contains("busy"));
    // A code nobody knows, in any spelling or shape, is the plain sentence: nothing it holds is in what is shown.
    for code in [
        "disk-full",
        "/home/alex/secret.txt",
        "",
        "DENIED",
        "timeout\n",
    ] {
        let sentence = told(code).unwrap();
        assert_eq!(
            sentence, "A plugin could not complete the action",
            "{code:?}"
        );
        assert!(!sentence.contains("hunter2") && !sentence.contains("secret"));
    }
}

#[test]
fn a_call_that_was_cancelled_or_went_well_has_no_failure_to_tell() {
    use peersus_api::message::Outcome;
    assert_eq!(
        action_failure_sentence(&Outcome::error("cancelled", "x")),
        None
    );
    assert_eq!(
        action_failure_sentence(&Outcome::ok(serde_json::json!({}))),
        None
    );
}

/// A device `key` that `plugin` was granted `send` for, online or not.
fn sendable(key: &str, plugin: &str, online: bool) -> Peer {
    let mut peer = peer_granted(key, &[(plugin, &[(HOST, PEERS_SEND_CAPABILITY)])]);
    peer.online = online;
    peer
}

/// A device that granted `plugin` exactly `permission`, online.
fn granted_exactly(key: &str, plugin: &str, owner: &str, name: &str) -> Peer {
    peer_granted(key, &[(plugin, &[(owner, name)])])
}

#[test]
fn sending_needs_the_applications_permission_and_not_a_same_named_one_of_the_plugin() {
    let aliases = crate::aliases::Aliases::new();
    // `send` is guarded by the APPLICATION's `peers.send`. A device that granted this
    // plugin the application's permission may be sent to.
    let app = granted_exactly("a", "clip", HOST, PEERS_SEND_CAPABILITY);
    let alias = aliases.alias(&clip(), &app.id);
    assert!(
        authorize_send(&aliases, &clip(), &alias, std::slice::from_ref(&app)).is_ok(),
        "a grant of the application's own permission was refused"
    );
    // A permission of the PLUGIN with the same name is another permission, with whatever
    // meaning the plugin gave it: it must not let anything be sent.
    let own = granted_exactly("b", "clip", "clip", PEERS_SEND_CAPABILITY);
    let alias = aliases.alias(&clip(), &own.id);
    assert!(
        authorize_send(&aliases, &clip(), &alias, std::slice::from_ref(&own)).is_err(),
        "a same-named permission of the plugin itself opened the application's send"
    );
}

fn clip() -> PluginId {
    PluginId::new("clip").unwrap()
}

#[test]
fn a_send_goes_to_the_device_that_is_granted_online_and_named_by_its_alias() {
    let aliases = crate::aliases::Aliases::new();
    let peers = [sendable("a", "clip", true), sendable("b", "clip", true)];
    let alias = aliases.alias(&clip(), &peers[1].id);
    assert_eq!(
        authorize_send(&aliases, &clip(), &alias, &peers).map(|p| p.id.clone()),
        Ok(peers[1].id.clone())
    );
}

#[test]
fn a_send_is_refused_unless_this_device_granted_this_plugin() {
    let aliases = crate::aliases::Aliases::new();
    let ungranted = peer_granted("a", &[]);
    let other_capability = peer_granted("b", &[("clip", &[("clip", "clip.other")])]);
    let other_plugin = peer_granted("c", &[("other", &[(HOST, PEERS_SEND_CAPABILITY)])]);
    for peer in [ungranted, other_capability, other_plugin] {
        let alias = aliases.alias(&clip(), &peer.id);
        assert!(
            authorize_send(&aliases, &clip(), &alias, std::slice::from_ref(&peer)).is_err(),
            "device {} must not be sendable to",
            peer.id.0
        );
    }
}

#[test]
fn a_grant_on_one_device_does_not_open_another() {
    let aliases = crate::aliases::Aliases::new();
    let peers = [sendable("a", "clip", true), peer_granted("b", &[])];
    let alias_of_b = aliases.alias(&clip(), &peers[1].id);
    assert!(authorize_send(&aliases, &clip(), &alias_of_b, &peers).is_err());
}

#[test]
fn a_send_to_a_device_that_is_not_online_is_refused() {
    let aliases = crate::aliases::Aliases::new();
    let peers = [sendable("a", "clip", false)];
    let alias = aliases.alias(&clip(), &peers[0].id);
    assert!(authorize_send(&aliases, &clip(), &alias, &peers).is_err());
}

#[test]
fn an_alias_that_means_nothing_is_refused() {
    let aliases = crate::aliases::Aliases::new();
    let peers = [sendable("a", "clip", true)];
    for guess in ["", "a", "d1", "0000"] {
        assert!(authorize_send(&aliases, &clip(), guess, &peers).is_err());
    }
}

#[test]
fn another_plugin_cannot_use_an_alias_it_was_not_given() {
    let aliases = crate::aliases::Aliases::new();
    let peer = sendable("a", "other", true);
    // The alias `clip` was given for this device; `other` is granted the device.
    let clips_alias = aliases.alias(&clip(), &peer.id);
    assert!(
        authorize_send(
            &aliases,
            &PluginId::new("other").unwrap(),
            &clips_alias,
            std::slice::from_ref(&peer)
        )
        .is_err()
    );
}

#[test]
fn every_refusal_reads_the_same_so_a_plugin_cannot_probe_which_devices_exist() {
    let aliases = crate::aliases::Aliases::new();
    let granted_offline = sendable("a", "clip", false);
    let ungranted_online = peer_granted("b", &[]);
    let peers = [granted_offline.clone(), ungranted_online.clone()];
    let refusals: BTreeSet<_> = [
        authorize_send(
            &aliases,
            &clip(),
            &aliases.alias(&clip(), &granted_offline.id),
            &peers,
        ),
        authorize_send(
            &aliases,
            &clip(),
            &aliases.alias(&clip(), &ungranted_online.id),
            &peers,
        ),
        authorize_send(&aliases, &clip(), "never-an-alias", &peers),
    ]
    .into_iter()
    .map(|result| result.expect_err("all three are refused"))
    .collect();
    assert_eq!(
        refusals.len(),
        1,
        "different refusals would tell a plugin what exists: {refusals:?}"
    );
}

#[test]
fn a_well_formed_send_names_a_device_a_service_and_a_version() {
    let request = parse_send(&serde_json::json!({
        "device": "abc123",
        "service": "sink.service",
        "version": 3,
    }))
    .expect("a complete request");
    assert_eq!(request.device, "abc123");
    assert_eq!(request.service, "sink.service");
    assert_eq!(request.version, 3);
}

#[test]
fn a_send_that_is_not_exactly_those_three_things_is_refused() {
    let refused = [
        serde_json::Value::Null,
        serde_json::json!("abc"),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!({"service": "s", "version": 1}),
        serde_json::json!({"device": "d", "version": 1}),
        serde_json::json!({"device": "d", "service": "s"}),
        serde_json::json!({"device": 1, "service": "s", "version": 1}),
        serde_json::json!({"device": "d", "service": 1, "version": 1}),
        serde_json::json!({"device": "d", "service": "s", "version": "1"}),
        serde_json::json!({"device": "d", "service": "s", "version": -1}),
        serde_json::json!({"device": "d", "service": "s", "version": 1.5}),
        // A fourth field is not silently ignored: the plugin is asking for something this
        // application does not offer.
        serde_json::json!({"device": "d", "service": "s", "version": 1, "limit": 9}),
    ];
    for args in refused {
        assert_eq!(
            parse_send(&args).err(),
            Some(SEND_REFUSED),
            "{args} must be refused"
        );
    }
}

#[test]
fn a_send_for_a_service_name_the_wire_would_reject_is_refused_before_anything_is_reserved() {
    for service in ["", "has space", "slash/ed", "ünï", &"a".repeat(129)] {
        let args = serde_json::json!({"device": "d", "service": service, "version": 1});
        assert_eq!(parse_send(&args).err(), Some(SEND_REFUSED), "{service:?}");
    }
    let zero = serde_json::json!({"device": "d", "service": "s", "version": 0});
    assert_eq!(parse_send(&zero).err(), Some(SEND_REFUSED));
    let longest = "a".repeat(128);
    let ok = serde_json::json!({"device": "d", "service": longest, "version": 1});
    assert!(
        parse_send(&ok).is_ok(),
        "128 characters is the limit, not 127"
    );
}

#[test]
fn every_malformed_send_reads_the_same_as_an_unknown_device() {
    // One wording for everything a plugin can get wrong, so that no refusal tells it
    // more than "no".
    let aliases = crate::aliases::Aliases::new();
    let peers = [peer_granted("a", &[])];
    let unknown_device = authorize_send(&aliases, &clip(), "nobody", &peers).expect_err("refused");
    let malformed = parse_send(&serde_json::Value::Null).expect_err("refused");
    assert_eq!(unknown_device, malformed);
}

#[tokio::test(start_paused = true)]
async fn a_step_that_never_finishes_ends_at_its_limit_and_not_before() {
    let started = tokio::time::Instant::now();
    // The outer ceiling is only a safety net for the test itself: without it, a `within`
    // that ignored its limit would make this test hang instead of fail.
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        within(Duration::from_secs(10), std::future::pending::<u8>()),
    )
    .await
    .expect("within waited past its limit instead of giving up at it");
    assert_eq!(outcome, None);
    assert_eq!(started.elapsed(), Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn a_step_that_finishes_in_time_is_not_cut_short() {
    let outcome = within(Duration::from_secs(10), async {
        tokio::time::sleep(Duration::from_secs(9)).await;
        7u8
    })
    .await;
    assert_eq!(outcome, Some(7));
}

#[tokio::test(start_paused = true)]
async fn a_step_that_is_already_done_costs_no_waiting() {
    let started = tokio::time::Instant::now();
    assert_eq!(
        within(Duration::from_secs(10), async { 1u8 }).await,
        Some(1)
    );
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[test]
fn what_a_plugin_is_told_about_a_device_is_the_alias_it_was_given_never_the_key() {
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let device = PeerId("KEY-9f3a77c1d2e4".into());
    let told = peer_argument(&aliases, &clip, &device);
    let text = told.to_string();
    assert!(!text.contains("KEY-9f3a77c1d2e4"), "the key leaked: {text}");
    assert!(
        !text.contains("9f3a77c1d2e4"),
        "part of the key leaked: {text}"
    );
    assert_eq!(told["peer"], aliases.alias(&clip, &device));
    assert_eq!(told.as_object().unwrap().len(), 1, "{text}");
}

#[test]
fn the_device_a_plugin_is_told_of_is_one_it_can_send_to() {
    // The one name has to work for everything: what an action or a receive hands over is
    // what `send` takes back, and what `list` gave.
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let seen = |key: &str| {
        peer_granted(
            key,
            &[(
                "clip",
                &[
                    (HOST, PEERS_SEND_CAPABILITY),
                    (HOST, PEERS_WATCH_CAPABILITY),
                ],
            )],
        )
    };
    let peers = [seen("a"), seen("b")];
    let told = peer_argument(&aliases, &clip, &peers[1].id);
    let alias = told["peer"].as_str().unwrap();
    assert_eq!(
        authorize_send(&aliases, &clip, alias, &peers).map(|p| p.id.clone()),
        Ok(peers[1].id.clone())
    );
    let listed = peers_listing(&aliases, &clip, &asks_send(), &peers);
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["device"] == told["peer"]),
        "list and the argument must name a device the same way"
    );
}

#[test]
fn two_plugins_are_told_different_names_for_the_same_device() {
    let aliases = crate::aliases::Aliases::new();
    let device = PeerId("shared".into());
    let first = peer_argument(&aliases, &PluginId::new("clip").unwrap(), &device);
    let second = peer_argument(&aliases, &PluginId::new("other").unwrap(), &device);
    assert_ne!(first["peer"], second["peer"]);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn outbound_completion_confirms_the_bytes_pumped_to_the_real_peer() {
    use tokio::io::AsyncWriteExt;

    let policy = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..peersus_core::config::NetworkSettings::default()
    };
    let application = peersus_net::transport::bind(iroh::SecretKey::generate(), &policy)
        .await
        .unwrap();
    let device = peersus_net::transport::bind(iroh::SecretKey::generate(), &policy)
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::join!(
        application.connect(device.addr(), peersus_net::transport::ALPN),
        async { device.accept().await.unwrap().await }
    );
    let connection = outgoing.unwrap();
    let peer = incoming.unwrap();
    let _keep_application_connection_alive = connection.clone();
    let (mut plugin, application_end) = tokio::net::UnixStream::pair().unwrap();
    let (deliver, receiver) = oneshot::channel();
    deliver.send(application_end).unwrap();
    let payload = b"verified outbound clipboard bytes";
    let request = SendRequest {
        device: "unused".into(),
        service: "sink.service".into(),
        version: 1,
    };

    tokio::time::timeout(Duration::from_secs(15), async {
        let (outcome, received, confirmed) = tokio::join!(
            carry_out(connection, request, receiver, Duration::from_secs(5)),
            async {
                let (_send, mut receive) = peer.accept_bi().await.unwrap();
                let preamble: peersus_net::streams::Request =
                    peersus_net::streams::receive(&mut receive).await.unwrap();
                assert!(matches!(
                    preamble,
                    peersus_net::streams::Request::Plugin { service, version: 1 }
                        if service == "sink.service"
                ));
                receive.read_to_end(1024).await.unwrap()
            },
            async {
                plugin.write_all(payload).await.unwrap();
                plugin.shutdown().await.unwrap();
                peersus_plugin::frame::read::<u64>(&mut plugin, 1024)
                    .await
                    .unwrap()
            }
        );
        assert_eq!(received, payload);
        assert_eq!(confirmed, payload.len() as u64);
        assert_eq!(outcome, crate::receive::Delivery::Done(confirmed));
    })
    .await
    .expect("outbound completion left the plugin or peer waiting");
}

/// A device that stays connected but never lets another stream be opened to it: the very
/// case the limit on opening exists for, on a real connection, going through the real
/// `carry_out` rather than the helper alone.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_gives_no_stream_credit_ends_the_send_at_the_limit_and_frees_the_plugin() {
    use iroh::{
        SecretKey,
        endpoint::{Builder, QuicTransportConfig, VarInt, presets},
    };
    use tokio::io::AsyncReadExt;
    let policy = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..peersus_core::config::NetworkSettings::default()
    };
    let application = peersus_net::transport::bind(SecretKey::generate(), &policy)
        .await
        .unwrap();
    // The device allows the application no stream of its own to open.
    let device = Builder::new(presets::Minimal)
        .secret_key(SecretKey::generate())
        .alpns(vec![peersus_net::transport::ALPN.to_vec()])
        .transport_config(
            QuicTransportConfig::builder()
                .max_concurrent_bidi_streams(VarInt::from_u32(0))
                .build(),
        )
        .clear_ip_transports()
        .bind_addr(std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::join!(
        application.connect(device.addr(), peersus_net::transport::ALPN),
        async { device.accept().await.unwrap().await }
    );
    let connection = outgoing.unwrap();
    let _device_side = incoming.unwrap();

    // The plugin has already come: its end of the data connection is handed over.
    let (plugin_end, application_end) = tokio::net::UnixStream::pair().unwrap();
    let (deliver, receiver) = oneshot::channel();
    deliver.send(application_end).unwrap();
    let request = SendRequest {
        device: "unused".into(),
        service: "sink.service".into(),
        version: 1,
    };

    let started = std::time::Instant::now();
    // The outer ceiling is only a safety net for the test: without the limit under test
    // this would wait for ever.
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        carry_out(connection, request, receiver, Duration::from_millis(600)),
    )
    .await
    .expect("opening a stream waited for ever instead of giving up at its limit");
    assert_eq!(
        outcome,
        crate::receive::Delivery::Failed("A device could not be reached to send to it")
    );
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(500),
        "gave up after {waited:?}, before the limit"
    );
    assert!(
        waited < Duration::from_secs(10),
        "took {waited:?}, far past the limit"
    );

    // And the plugin is freed: its end of the connection is closed, so it is told.
    let mut plugin_end = plugin_end;
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), plugin_end.read(&mut byte))
        .await
        .expect("the plugin's socket was left open");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
}

#[test]
fn configuration_returns_only_declared_caller_values_and_scoped_aliases() {
    use peersus_api::contributions::{Kind, Scope, Setting};
    use peersus_core::plugins::{Value, Values};
    let mut spec = plugin_importing("clip", HOST, CONFIG_SERVICE, 1, &["get"]);
    // It also sends to devices, so a device it was granted that for is one it may read its settings of.
    spec.manifest.imports.extend(
        plugin_wanting_peers("clip", &["list", "send"])
            .manifest
            .imports,
    );
    spec.manifest.contributions.settings = vec![
        Setting {
            id: "automatic".into(),
            label: "Automatic".into(),
            help: None,
            scope: Scope::Global,
            kind: Kind::Toggle { default: false },
        },
        Setting {
            id: "sync".into(),
            label: "Sync".into(),
            help: None,
            scope: Scope::Device,
            kind: Kind::Toggle { default: false },
        },
    ];
    let saved: Values =
        serde_json::from_value(serde_json::json!({"automatic":"invalid", "undeclared":"private"}))
            .unwrap();
    let mut peer = peer_granted(
        "actual-network-key",
        &[("clip", &[(HOST, PEERS_WATCH_CAPABILITY)])],
    );
    // A device it was granted nothing for is not in what it reads at all.
    let stranger = peer_granted("stranger-network-key", &[]);
    let mut device = Values::default();
    device
        .set(
            &spec.manifest.contributions.settings,
            Scope::Device,
            "sync",
            Value::Toggle(true),
        )
        .unwrap();
    peer.plugin_values.insert(spec.id.clone(), device);
    let foreign = PluginId::new("foreign").unwrap();
    peer.plugin_values.insert(
        foreign.clone(),
        serde_json::from_value(serde_json::json!({"private":"do not expose"})).unwrap(),
    );
    let aliases = crate::aliases::Aliases::new();
    let answer = configuration(&spec, Some(&saved), &aliases, &[peer.clone(), stranger]);
    assert_eq!(
        answer,
        serde_json::json!({
            "plugin": spec.id.as_str(),
            "settings": {"automatic": false},
            "peers": [{"device": aliases.alias(&spec.id, &peer.id), "online": peer.online, "settings": {"sync":true},
                "permissions": serde_json::to_value(peer.grants[&spec.id].iter().collect::<Vec<_>>()).unwrap()}]
        })
    );
    assert!(!answer.to_string().contains("stranger-network-key"));
    assert_ne!(
        answer["peers"][0]["device"],
        aliases.alias(&foreign, &peer.id)
    );
    assert!(!answer.to_string().contains("actual-network-key"));
    assert!(!answer.to_string().contains("private"));
}

#[test]
fn configuration_import_does_not_grant_send_or_expose_other_consumers() {
    let mut registry = Registry::default();
    let spec = plugin_importing("clip", HOST, CONFIG_SERVICE, 1, &["get"]);
    registry.register(spec.clone()).unwrap();
    registry.register(plugin_with_guarded_action()).unwrap();
    let exports = host_exports(&registry);
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].consumers, ["clip".to_owned()].into());
    assert_eq!(
        exports[0]
            .methods
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["get"]
    );
    assert!(grantable(&spec).is_empty());
    // Configuration is information, never authorization: the per-device send guard
    // is still rechecked by the same path explicit share calls use.
    let sending = clip_spec();
    let peer = peer_granted("device", &[]);
    let aliases = crate::aliases::Aliases::new();
    assert!(
        authorize_send(
            &aliases,
            &sending.id,
            &aliases.alias(&sending.id, &peer.id),
            &[peer]
        )
        .is_err()
    );
}

#[test]
fn a_revoked_service_guard_blocks_a_new_send_even_with_stale_configuration() {
    let spec = plugin_that_receives(Some("sink.write"));
    let allowed = peer_granted(
        "device",
        &[(
            "sink",
            &[(HOST, PEERS_SEND_CAPABILITY), ("sink", "sink.write")],
        )],
    );
    assert!(authorize_service_send(&spec, &allowed, &spec.manifest.exports[0].service, 1).is_ok());
    let revoked = peer_granted("device", &[("sink", &[(HOST, PEERS_SEND_CAPABILITY)])]);
    let aliases = crate::aliases::Aliases::new();
    let alias = aliases.alias(&spec.id, &revoked.id);
    assert!(authorize_send(&aliases, &spec.id, &alias, std::slice::from_ref(&revoked)).is_ok());
    assert!(authorize_service_send(&spec, &revoked, &spec.manifest.exports[0].service, 1).is_err());
    assert!(authorize_service_send(&spec, &revoked, "an.unrelated.service", 1).is_ok());
}
#[test]
fn platform_facilities_require_explicit_grants_for_each_new_call() {
    use crate::platform_services;
    use peersus_api::message::Outcome;
    for &(service, method, capability, _) in platform_services::METHODS {
        let spec = plugin_importing("upload", HOST, service, 1, &[method]);
        let grant = permission(HOST, capability);
        assert!(grantable_without_a_device(&spec).contains(&grant));
        assert!(!grantable_for_a_device(&spec).contains(&grant));
        let mut registry = Registry::default();
        registry.register(spec.clone()).unwrap();
        registry
            .register(plugin_wanting_peers("unrelated", &["list"]))
            .unwrap();
        let manifest = host_manifest(&registry);
        let export = manifest
            .exports
            .iter()
            .find(|export| export.service == service)
            .unwrap();
        assert_eq!(export.consumers, ["upload".to_owned()].into());
        let platform_timeouts = platform_services::method_timeouts(&manifest);
        // The call asks for exactly the ceiling this method has, taken from the map the hub is given, so that what is
        // tested is who may call and not how long a method may take, which differs by what the method does.
        let ceiling = u32::try_from(
            platform_timeouts
                .get(&(service.to_owned(), 1, method.to_owned()))
                .expect("every platform method has a ceiling")
                .as_millis(),
        )
        .expect("a ceiling fits a call's deadline");
        let hub = Hub::default();
        let generation = hub.register_host(HOST, manifest, BTreeSet::new()).unwrap();
        let host = hub.connect(HOST, generation).unwrap();
        host.set_host_method_timeouts(platform_timeouts).unwrap();
        let generation = hub
            .register("upload", spec.manifest, BTreeSet::new())
            .unwrap();
        let guest = hub.connect("upload", generation).unwrap();
        let call = |id| Operation::Call {
            id,
            provider: HOST.to_owned(),
            service: service.to_owned(),
            version: 1,
            method: method.to_owned(),
            args: serde_json::json!({}),
            timeout_ms: ceiling,
        };
        assert_eq!(
            guest.submit(call(1)),
            Err(peersus_api::error::Error::Denied)
        );
        assert!(host.try_recv().unwrap().is_none());
        hub.set_permissions("upload", [grant].into()).unwrap();
        guest.submit(call(2)).unwrap();
        let Some(Delivery::Call { ticket, caller, .. }) = host.try_recv().unwrap() else {
            panic!("granted call did not reach the host");
        };
        assert_eq!(caller, "upload");
        hub.set_permissions("upload", BTreeSet::new()).unwrap();
        assert_eq!(
            guest.submit(call(3)),
            Err(peersus_api::error::Error::Denied)
        );
        // The broker preserves accepted-call commitments. Sensitive platform work
        // is cancelled by the owner's PlatformCalls, not by changing that contract.
        assert!(host.try_recv().unwrap().is_none());
        host.submit(Operation::Reply {
            ticket,
            result: Outcome::ok(serde_json::json!({})),
        })
        .unwrap();
        assert!(matches!(
            guest.try_recv().unwrap(),
            Some(Delivery::Result { id: 2, .. })
        ));
    }
}

/// What a service says about how long its methods may take is what the hub enforces: one that never waits for a person is
/// held to seconds, and one that does keeps minutes. Permission is granted so that nothing but time can refuse a call.
#[test]
fn a_platform_method_is_held_to_the_ceiling_its_service_declares() {
    use crate::platform_services;
    for &(service, method, capability, _) in platform_services::METHODS {
        let spec = plugin_importing("upload", HOST, service, 1, &[method]);
        let mut registry = Registry::default();
        registry.register(spec.clone()).unwrap();
        let manifest = host_manifest(&registry);
        let ceilings = platform_services::method_timeouts(&manifest);
        let ceiling = ceilings[&(service.to_owned(), 1, method.to_owned())];
        let hub = Hub::default();
        let generation = hub.register_host(HOST, manifest, BTreeSet::new()).unwrap();
        let host = hub.connect(HOST, generation).unwrap();
        host.set_host_method_timeouts(ceilings).unwrap();
        let generation = hub
            .register(
                "upload",
                spec.manifest,
                [permission(HOST, capability)].into(),
            )
            .unwrap();
        let guest = hub.connect("upload", generation).unwrap();
        let call = |id, timeout_ms| Operation::Call {
            id,
            provider: HOST.to_owned(),
            service: service.to_owned(),
            version: 1,
            method: method.to_owned(),
            args: serde_json::json!({}),
            timeout_ms,
        };
        let at = u32::try_from(ceiling.as_millis()).expect("a ceiling fits a call's deadline");
        assert!(
            guest.submit(call(1, at)).is_ok(),
            "{service}.{method} refused a call at its own ceiling"
        );
        // The hub never lowers its general limit for a method; a declared ceiling only raises it. So the first deadline
        // nothing permits is just past the larger of the two.
        let limit = ceiling.max(peersus_plugin::hub::MAX_TIMEOUT);
        let past = u32::try_from(limit.as_millis() + 1).expect("a deadline past the limit fits");
        assert!(
            guest.submit(call(2, past)).is_err(),
            "{service}.{method} accepted a call past every limit that applies to it"
        );
    }
    // The two kinds of service really differ, which is the point of declaring them: a clipboard hangs up in seconds, and
    // the file chooser, which waits for a person, does not.
    let seconds = |service: &str| -> std::time::Duration {
        // A plugin that imports no method of a service is offered none of it, so ask for one the table really has.
        let method = platform_services::METHODS
            .iter()
            .find(|(offered, _, _, _)| *offered == service)
            .map(|(_, method, _, _)| *method)
            .expect("the service has a method");
        let spec = plugin_importing("upload", HOST, service, 1, &[method]);
        let mut registry = Registry::default();
        registry.register(spec).unwrap();
        let manifest = host_manifest(&registry);
        platform_services::method_timeouts(&manifest)
            .into_iter()
            .find(|((owner, _, _), _)| owner == service)
            .map(|(_, ceiling)| ceiling)
            .expect("the service has a method with a ceiling")
    };
    assert!(seconds(platform_services::CLIPBOARD_SERVICE) <= std::time::Duration::from_secs(30));
    assert!(seconds(platform_services::FILES_SERVICE) >= std::time::Duration::from_secs(300));
}
