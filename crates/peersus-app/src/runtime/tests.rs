//! The owner's own rules, tried without a window: what is offered, granted, refused and carried.
use super::*;
use peersus_api::{
    contributions::{Action, Source},
    manifest::{Export, Manifest, Member},
    message::{Delivery, Operation},
};

#[cfg(target_os = "linux")]
mod control_visit;
mod query_publication;

async fn loopback_runtime(root: &std::path::Path, registry: Registry) -> (Runtime, Client) {
    loopback_runtime_on(root, registry, loopback_endpoint().await)
}

async fn loopback_endpoint() -> iroh::Endpoint {
    let network_settings = peersus_core::config::NetworkSettings {
        bind: Some("127.0.0.1".parse().unwrap()),
        allow_relay: false,
        ..Default::default()
    };
    peersus_net::transport::bind(iroh::SecretKey::generate(), &network_settings)
        .await
        .unwrap()
}

fn loopback_runtime_on(
    root: &std::path::Path,
    registry: Registry,
    endpoint: iroh::Endpoint,
) -> (Runtime, Client) {
    let network = NetworkHost::new(
        endpoint,
        "Test".into(),
        peersus_net::peers::Peers::open(root.join("peers.json")).unwrap(),
        peersus_net::history::History::open(root.join("history.json")).unwrap(),
    );
    let settings =
        peersus_net::settings::SettingsStore::open(root.join("preferences.json")).unwrap();
    let grants = peersus_net::grants::StandaloneGrants::open(root.join("grants.json")).unwrap();
    let sockets = root.join("sockets");
    crate::private_fs::private_folders()
        .create(&sockets)
        .unwrap();
    Runtime::new(network, grants, settings, registry, sockets).unwrap()
}

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
        ..Manifest::new(peersus_api::manifest::Execution::Process)
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn input_pressure_setting_rejects_invalid_writes_without_losing_the_saved_amount() {
    use peersus_api::contributions::{EdgePressure, Input, Kind, Scope, Setting};
    use serde_json::json;

    let mut spec = plugin_with_guarded_action();
    let plugin = spec.id.clone();
    let contributions = &mut spec.manifest.contributions;
    contributions.settings.push(Setting {
        id: "effort".into(),
        platforms: vec![peersus_api::platform::OperatingSystem::Linux],
        label: "Effort at the edge".into(),
        help: None,
        scope: Scope::Global,
        kind: Kind::Number {
            min: 0,
            max: 400,
            default: 80,
        },
        public_read: None,
    });
    contributions.input = Some(Input {
        source: Source {
            service: "svc".into(),
            method: "go".into(),
        },
        capability: "cap".into(),
        edge_pressure: EdgePressure::Setting {
            id: "effort".into(),
        },
    });
    let mut registry = Registry::default();
    registry.register(spec).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let (mut runtime, _client) = loopback_runtime(directory.path(), registry).await;
    runtime.disabled.insert(plugin.clone());
    runtime
        .set_plugin_setting(plugin.clone(), None, "effort", Value::Number(0))
        .await
        .unwrap();

    for invalid in [
        Value::Number(-1),
        Value::Number(401),
        Value::Text("80".into()),
        Value::Toggle(true),
    ] {
        assert_eq!(
            runtime
                .set_plugin_setting(plugin.clone(), None, "effort", invalid)
                .await,
            Err("Invalid value for this setting")
        );
    }
    assert!(serde_json::from_value::<Value>(json!(80.5)).is_err());
    let baseline = runtime.settings.current().clone();
    for invalid in [json!(-1), json!(401), json!("80"), json!(true)] {
        // Use the actual whole-preferences wire shape, with a current baseline and an
        // unrelated valid change. Refusal must leave both values untouched.
        let mut candidate = serde_json::to_value(&baseline).unwrap();
        candidate["plugins"][plugin.as_str()]["effort"] = invalid;
        candidate["background"] = json!(!baseline.background);
        let command = serde_json::from_value::<Command>(json!({
            "command": "update-settings",
            "settings": candidate,
            "expected": baseline,
        }))
        .unwrap();
        assert!(matches!(
            runtime.execute(command).await,
            Err("Invalid value for this setting")
        ));
        assert_eq!(runtime.settings.current(), &baseline);
    }
    assert_eq!(
        runtime.settings.current().plugins[&plugin].get("effort"),
        Some(&Value::Number(0))
    );
    let reopened =
        peersus_net::settings::SettingsStore::open(directory.path().join("preferences.json"))
            .unwrap();
    assert_eq!(
        reopened.current().plugins[&plugin].get("effort"),
        Some(&Value::Number(0))
    );
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

#[cfg(all(unix, not(target_os = "android"), not(target_os = "ios")))]
#[tokio::test]
async fn public_read_settings_require_the_exact_disclosed_scope_and_survive_an_owner_restart() {
    use peersus_api::contributions::{Kind, PublicRead, Setting};
    use serde_json::json;
    let mut spec = plugin_with_guarded_action();
    let resource = PublicRead {
        origin: "https://example.test".into(),
        root: "/maps".into(),
    };
    spec.manifest.agreements = Some(serde_json::from_value(json!({"pages":[
        {"kind":"licence","title":"MIT","explains":"You may use it.","changes":"No added rights.","spdx":"MIT","links":[{"label":"MIT","url":"https://opensource.org/license/mit"}]},
        {"kind":"notice","title":"Notice","explains":"Optional public data.","changes":"A setting appears."}
    ]})).unwrap());
    spec.manifest.imports.push(peersus_api::manifest::Import {
        provider: HOST.into(),
        service: crate::platform_services::HTTP_SERVICE.into(),
        version: 1,
        methods: ["restore-read".to_owned()].into(),
        events: Default::default(),
    });
    spec.manifest.imports.push(peersus_api::manifest::Import {
        provider: HOST.into(),
        service: CONFIG_SERVICE.into(),
        version: 1,
        methods: ["get".to_owned()].into(),
        events: Default::default(),
    });
    spec.manifest.contributions.settings.push(Setting {
        id: "public-data".into(),
        platforms: Vec::new(),
        label: "Public data".into(),
        help: None,
        scope: Scope::Global,
        kind: Kind::Toggle { default: false },
        public_read: Some(resource.clone()),
    });
    let plugin = spec.id.clone();
    let root = tempfile::tempdir().unwrap();
    let digest = peersus_core::agreements::digest(spec.manifest.agreements.as_ref().unwrap());
    crate::plugin_agreements::accept(root.path(), plugin.as_str(), &digest, 0).unwrap();
    let start = || {
        let root = root.path();
        let spec = spec.clone();
        async move {
            let mut registry = Registry::default();
            registry.register(spec).unwrap();
            let (runtime, client) = loopback_runtime(root, registry).await;
            let mut runtime = runtime
                .with_packages(crate::plugin_manager::Manager::new(root))
                .with_platform_services(crate::desktop_services::DesktopServices::new(
                    root.join("scratch"),
                    root.join("public-approvals"),
                ));
            runtime.refresh_public_read_settings();
            (runtime, client)
        }
    };
    let toggle = |resource, enabled| Command::SetPublicReadSetting {
        plugin: plugin.clone(),
        setting: "public-data".into(),
        resource,
        enabled,
    };
    let state = |runtime: &mut Runtime| {
        let (answer, _) =
            runtime.answer_plugin_call(plugin.as_str(), CONFIG_SERVICE, "get", &json!({}));
        let peersus_api::message::Outcome::Ok { value } = answer else {
            panic!("Configuration refused");
        };
        value["settings"]["public-data"].as_bool().unwrap()
    };
    let (mut runtime, _) = start().await;
    assert!(!state(&mut runtime));
    assert!(
        runtime
            .execute(Command::SetPluginSetting {
                plugin: plugin.clone(),
                peer: None,
                setting: "public-data".into(),
                value: Value::Toggle(true),
            })
            .await
            .is_err()
    );
    let other = PublicRead {
        root: "/other".into(),
        ..resource.clone()
    };
    assert!(runtime.execute(toggle(other, true)).await.is_err());
    assert!(!state(&mut runtime));
    runtime
        .execute(toggle(resource.clone(), true))
        .await
        .unwrap();
    assert!(state(&mut runtime));
    assert!(runtime.public_read_may_restore(&plugin, Some(&resource.origin), Some(&resource.root)));
    assert!(!runtime.public_read_may_restore(&plugin, Some(&resource.origin), Some("/other")));
    drop(runtime);
    let (mut runtime, _) = start().await;
    assert!(state(&mut runtime));
    runtime.execute(toggle(resource, false)).await.unwrap();
    assert!(!state(&mut runtime));
    drop(runtime);
    let (mut runtime, _) = start().await;
    assert!(!state(&mut runtime));
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
    use peersus_api::contributions::{Kind, Page, Placement, Scope, Setting, Surface};
    use serde_json::json;
    let mut spec = plugin_with_guarded_action();
    spec.manifest.contributions.settings.push(Setting {
        public_read: None,
        platforms: Vec::new(),
        id: "editor-main".into(),
        label: "Show editor".into(),
        help: None,
        scope: Scope::Global,
        kind: Kind::Toggle { default: true },
    });
    spec.manifest.contributions.pages.push(Page {
        id: "editor".into(),
        label: "Editor".into(),
        // Declared for the main navigation: it is still drawn in the extension's settings, only the person places it there.
        placement: Placement::Primary,
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
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    let set = |runtime: &mut Runtime, args| {
        runtime
            .answer_plugin_call(plugin.as_str(), crate::page_offers::SERVICE, "set", &args)
            .0
    };
    let primary = || json!({"pages":[{"id":"editor-main","page":"editor","placement":"primary"}]});
    // A manifest default is not the person's consent. Exercise the same settings store and
    // runtime setter that a window uses, not a predicate that pretends a setting was saved.
    assert!(!matches!(
        set(&mut runtime, primary()),
        peersus_api::message::Outcome::Ok { .. }
    ));
    for enabled in [true, false] {
        runtime
            .set_plugin_setting(
                plugin.clone(),
                None,
                "editor-main",
                peersus_core::plugins::Value::Toggle(enabled),
            )
            .await
            .unwrap();
        if !enabled {
            // Removing consent must not depend on the guest sending a withdrawal.
            assert!(runtime.page_source(&plugin, "editor-main").is_err());
        }
        assert_eq!(
            matches!(
                set(&mut runtime, primary()),
                peersus_api::message::Outcome::Ok { .. }
            ),
            enabled
        );
        assert_eq!(runtime.page_source(&plugin, "editor-main").is_ok(), enabled);
        assert!(runtime.page_source(&plugin, "editor").is_ok());
    }
    assert!(matches!(
        set(
            &mut runtime,
            json!({"pages":[{"id":"editor-main","page":"editor","placement":"status"}]})
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
    assert_eq!(
        pages[0].placement,
        Placement::Settings,
        "a page is never in the navigation because its manifest says so"
    );
    assert_eq!(pages[1].placement, Placement::Status);
    assert!(matches!(
        set(&mut runtime, json!({"pages":[]})),
        peersus_api::message::Outcome::Ok { .. }
    ));
    assert!(runtime.page_source(&plugin, "editor-main").is_err());
    assert!(runtime.page_source(&plugin, "editor").is_ok());
    set(
        &mut runtime,
        json!({"pages":[{"id":"editor-main","page":"editor","placement":"status"}]}),
    );
    runtime.stop_plugin(&plugin).await;
    assert!(runtime.page_source(&plugin, "editor-main").is_err());
    assert_eq!(
        runtime.plugin_infos()[0].manifest.contributions.pages.len(),
        1
    );

    // The owner has withdrawn the offer, but the old view can still emit an open,
    // resize or clock event until the window consumes its next snapshot.
    let owner = tokio::spawn(runtime.run());
    let event = |page: &str, event| Command::PageEvent {
        plugin: plugin.clone(),
        page: page.into(),
        event,
    };
    use peersus_api::elements::Event;
    for background in [
        Event::Open,
        Event::Size {
            id: "canvas".into(),
            x: 0,
            y: 0,
            width: 320_000,
            height: 200_000,
        },
        Event::Tick {
            id: "clock".into(),
            at: 1_000,
        },
    ] {
        assert!(
            matches!(
                client.command(event("editor-main", background)).await,
                Err("Unknown page")
            ),
            "the withdrawn page must not receive stale input"
        );
        assert!(
            !client
                .snapshot()
                .errors
                .iter()
                .any(|error| error.ends_with("Unknown page")),
            "an automatic event must not give the person an error for the withdrawn page"
        );
    }
    assert!(
        matches!(
            client
                .command(event("unregistered", Event::Press { id: "go".into() }))
                .await,
            Err("Unknown page")
        ),
        "the explicit request must still fail"
    );
    assert!(
        client
            .snapshot()
            .errors
            .iter()
            .any(|error| error.ends_with("Unknown page"))
    );
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
}

fn observed_page_fixture() -> PluginSpec {
    let mut spec = plugin_with_guarded_action();
    // The test may put its own guest on the hub, no external program.
    spec.program = None;
    spec.manifest.contributions.actions.clear();
    spec.manifest.exports[0]
        .methods
        .get_mut("go")
        .unwrap()
        .capability = None;
    spec.manifest.contributions.pages.push(
        serde_json::from_value(serde_json::json!({
            "id":"editor", "label":"Editor", "placement":"settings", "surface":"elements",
            "source":{"service":"svc","method":"go"}
        }))
        .unwrap(),
    );
    spec
}

#[tokio::test]
async fn stale_page_samples_do_not_reach_the_guest_or_report_a_window_error() {
    use peersus_api::elements::Event;
    let spec = observed_page_fixture();
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    // The in-memory guest owns this fixture; do not start an external program at boot.
    runtime.disabled.insert(plugin.clone());
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    let key = crate::page_views::PageKey {
        plugin: plugin.clone(),
        page: "editor".into(),
    };
    runtime.page_views.sent(&key, 1);
    runtime.page_views.answered(1, Some(&serde_json::json!({
        "k":"column","children":[{"k":"box","id":"row","observe":["visibility"]},{"k":"devices","id":"pick"}]
    }))).unwrap();
    runtime.page_views.sent(&key, 2);
    runtime.page_views.answered(2, Some(&serde_json::json!({
        "k":"column","children":[{"k":"box","id":"other","observe":["visibility"]},{"k":"devices","id":"current"}]
    }))).unwrap();
    let owner = tokio::spawn(runtime.run());
    let command = |event| Command::PageEvent {
        plugin: plugin.clone(),
        page: key.page.clone(),
        event,
    };
    let submit = |event| client.command(command(event));
    let observation = |id: &str| Event::Visibility {
        id: id.into(),
        visible: true,
        foreground: true,
    };

    assert!(matches!(
        client.observe_page(command(observation("row"))).await,
        Ok(Reply::Applied)
    ));
    assert!(matches!(
        submit(Event::Choose {
            id: "pick".into(),
            device: None
        })
        .await,
        Ok(Reply::Applied)
    ));
    assert!(guest.try_recv().unwrap().is_none());
    assert!(client.snapshot().errors.is_empty());
    assert!(matches!(
        client
            .observe_page(command(observation("invalid/id")))
            .await,
        Err("That input was not understood")
    ));
    assert!(matches!(
        submit(Event::Choose {
            id: "current".into(),
            device: None
        })
        .await,
        Err("That input was not understood")
    ));
    assert!(matches!(
        submit(Event::Choose {
            id: "invalid/id".into(),
            device: None
        })
        .await,
        Err("That input was not understood")
    ));
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
#[tokio::test]
async fn cli_pages_do_not_attach_a_window_or_report_its_visibility() {
    use crate::{cli, ipc};
    use clap::Parser;
    use peersus_api::{elements::Event, message::Outcome};
    use serde_json::json;

    let spec = observed_page_fixture();
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut owner, client) = loopback_runtime(root.path(), registry).await;
    owner.disabled.insert(plugin.clone());
    let generation = owner
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = owner.hub.connect(plugin.as_str(), generation).unwrap();
    let socket = root.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let presence = crate::lifetime::Presence::default();
    let ipc = tokio::spawn(ipc::serve(listener, client.clone(), presence.clone()));
    let running = tokio::spawn(owner.run());
    let (calls, mut events) = mpsc::channel(4);
    let serving = tokio::spawn(async move {
        while let Ok(Delivery::Call { args, ticket, .. }) = guest.receive().await {
            let event: Event = serde_json::from_value(args["event"].clone()).unwrap();
            let description = if event == Event::Open {
                "Opened"
            } else {
                "Changed"
            };
            calls.send(event).await.unwrap();
            guest.submit(Operation::Reply {
                ticket,
                result: Outcome::ok(json!({
                    "k":"box","id":"row","observe":["visibility"],
                    "children":[{"k":"text","text":description},{"k":"button","id":"action","label":"Act"}]
                })),
            }).unwrap();
        }
    });
    let control = ipc::LocalControl::new(socket.clone());
    let name = plugin.as_str();
    assert!(matches!(
        cli::Cli::try_parse_from(["peersus", "page", name, "editor"])
            .unwrap()
            .execute(&control)
            .await,
        Ok(cli::Output::Status(_))
    ));
    assert_eq!(events.try_recv().unwrap(), Event::Open);
    assert_eq!(presence.windows(), 0);
    let press = r#"{"t":"press","id":"action"}"#;
    assert!(matches!(
        cli::Cli::try_parse_from(["peersus", "page-event", name, "editor", press])
            .unwrap()
            .execute(&control)
            .await,
        Ok(cli::Output::Status(_))
    ));
    assert_eq!(
        events.try_recv().unwrap(),
        Event::Press {
            id: "action".into()
        }
    );
    let observed = json!({"t":"visibility", "id":"row", "visible":true, "foreground":true});
    let raw = json!({"command":"page-event", "plugin":name, "page":"editor", "event":observed});
    for args in [
        vec![
            "peersus".into(),
            "page-event".into(),
            name.into(),
            "editor".into(),
            observed.to_string(),
        ],
        vec!["peersus".into(), "call".into(), raw.to_string()],
    ] {
        let result = cli::Cli::try_parse_from(args)
            .unwrap()
            .execute(&control)
            .await;
        assert!(matches!(&result, Err(reason) if reason == "Visibility is reported by a window"));
    }
    assert!(events.try_recv().is_err());
    assert_eq!(presence.windows(), 0);
    let (window, following) = ipc::attach(socket).await.unwrap();
    assert_eq!(presence.windows(), 1);
    let sample: Event = serde_json::from_value(observed).unwrap();
    assert!(matches!(
        window
            .observe_page(Command::PageEvent {
                plugin,
                page: "editor".into(),
                event: sample.clone(),
            })
            .await,
        Ok(Reply::Applied)
    ));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap(),
        Some(sample)
    );
    assert!(client.snapshot().errors.is_empty());
    drop(window);
    following.await.unwrap();
    client.command(Command::Shutdown {}).await.unwrap();
    running.await.unwrap();
    serving.abort();
    let _ = serving.await;
    ipc.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_choice_queued_by_a_live_chooser_is_dropped_if_it_disappears_before_dispatch() {
    use peersus_api::{elements::Event, message::Outcome};
    use serde_json::json;
    let spec = observed_page_fixture();
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), registry).await;
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    let page = "editor";
    let key = crate::page_views::PageKey {
        plugin: plugin.clone(),
        page: page.into(),
    };
    runtime.page_views.sent(&key, 1);
    runtime
        .page_views
        .answered(
            1,
            Some(&json!({"k":"devices","id":"pick","this_device":true})),
        )
        .unwrap();
    let send = |event| Command::PageEvent {
        plugin: plugin.clone(),
        page: page.into(),
        event,
    };
    assert!(matches!(
        runtime.execute(send(Event::Open)).await,
        Ok(Reply::Applied)
    ));
    let Delivery::Call { ticket, .. } = guest.try_recv().unwrap().unwrap() else {
        panic!("Open did not reach the guest");
    };
    let choice = Event::Choose {
        id: "pick".into(),
        device: None,
    };
    assert!(matches!(
        runtime.execute(send(choice)).await,
        Ok(Reply::Applied)
    ));
    assert!(
        guest.try_recv().unwrap().is_none(),
        "one page call at a time"
    );
    guest
        .submit(Operation::Reply {
            ticket,
            result: Outcome::ok(json!({"k":"text","text":"The old chooser is gone"})),
        })
        .unwrap();
    let result = runtime.host.try_recv().unwrap().unwrap();
    runtime.handle_host_delivery(result).await;
    assert!(
        guest.try_recv().unwrap().is_none(),
        "removed chooser must not receive queued input"
    );
    assert!(runtime.errors.is_empty());
}

#[tokio::test]
async fn a_live_chooser_exposes_only_the_owner_resolved_alias_to_the_guest() {
    use peersus_api::elements::{DeviceEntry, Event, Presence};
    let spec = observed_page_fixture();
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), registry).await;
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    let key = crate::page_views::PageKey {
        plugin: plugin.clone(),
        page: "editor".into(),
    };
    runtime.page_views.sent(&key, 1);
    runtime
        .page_views
        .answered(1, Some(&serde_json::json!({"k":"devices","id":"pick"})))
        .unwrap();
    let peer = PeerId("owner-only-fixture".into());
    runtime.page_views.fill_devices(None, |_, _, _| {
        vec![DeviceEntry {
            device: peer.0.clone(),
            name: "Device".into(),
            presence: Presence::Online,
            statuses: Vec::new(),
            refused: None,
        }]
    });
    let alias = runtime.aliases.alias(&plugin, &peer);
    runtime
        .execute(Command::PageEvent {
            plugin,
            page: key.page,
            event: Event::Choose {
                id: "pick".into(),
                device: Some(peer.0.clone()),
            },
        })
        .await
        .unwrap();
    let Delivery::Call { args, .. } = guest.try_recv().unwrap().unwrap() else {
        panic!("the valid choice did not reach the extension");
    };
    assert_ne!(alias, peer.0);
    assert_eq!(
        args["event"],
        serde_json::json!({"t":"choose","id":"pick","device":alias})
    );
    assert!(guest.try_recv().unwrap().is_none());
}

#[tokio::test]
async fn each_executor_can_publish_a_canvas_and_claim_its_stream() {
    use peersus_api::{
        canvas::{OPEN, SERVICE},
        elements::{FEATURE_CANVAS, PAGE_FEATURES_KEY},
        manifest::{EXECUTION_ABI_VERSION, Execution},
        message::Outcome,
    };
    use serde_json::json;

    let executors = [
        (
            "canvas-wasm",
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            },
        ),
        (
            "canvas-native",
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            },
        ),
        ("canvas-process", Execution::Process),
    ];
    let mut registry = Registry::default();
    for (name, execution) in executors {
        let mut plugin = plugin_importing(name, HOST, SERVICE, 1, &[OPEN]);
        plugin.manifest.execution = execution;
        registry.register(plugin).unwrap();
    }
    let exported = host_manifest(&registry)
        .exports
        .into_iter()
        .find(|export| export.service == SERVICE)
        .unwrap();
    assert_eq!(
        exported.consumers,
        executors
            .into_iter()
            .map(|(name, _)| name.to_owned())
            .collect::<BTreeSet<_>>()
    );
    assert!(
        exported
            .methods
            .get(OPEN)
            .is_some_and(|member| member.capability.is_none())
    );

    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), registry).await;
    for (id, (name, _)) in (1..).zip(executors) {
        let Outcome::Ok { value } = runtime
            .answer_plugin_call(name, CONFIG_SERVICE, "get", &json!({}))
            .0
        else {
            panic!("configuration missing for {name}");
        };
        let features = value[PAGE_FEATURES_KEY].as_array().unwrap();
        assert!(features.contains(&json!(FEATURE_CANVAS)), "{name}");

        let plugin = PluginId::new(name).unwrap();
        let page = crate::page_views::PageKey {
            plugin: plugin.clone(),
            page: "demo".into(),
        };
        runtime.page_views.sent(&page, id);
        runtime
            .handle_host_delivery(Delivery::Result {
                id,
                result: Outcome::ok(json!({"k":"canvas","id":"map"})),
            })
            .await;
        let args = json!({"page":"demo","canvas":"map"});
        assert!(
            matches!(
                runtime.answer_plugin_call(name, SERVICE, OPEN, &args).0,
                Outcome::Error { .. }
            ),
            "an unwatched canvas cannot be claimed"
        );
        let _view = runtime
            .page_frames
            .watch(crate::page_frames::Key {
                plugin,
                page: page.page,
                canvas: "map".into(),
            })
            .unwrap_or_else(|| panic!("{name}'s page did not declare a canvas"));
        assert!(
            matches!(
                runtime.answer_plugin_call(name, SERVICE, OPEN, &args).0,
                Outcome::Ok { value } if value["claim"].as_u64().is_some()
            ),
            "{name} could not open a watched canvas"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_in_process_canvas_shares_the_latest_pixels_and_closes_its_claim_on_unmount() {
    use crate::ipc::{self, FrameKey, FramePoll, FrameReader};
    use peersus_api::canvas::{FrameHeader, FrameLayout, MAX_FRAME_HEADER_BYTES};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = loopback_runtime(root.path(), Registry::default()).await;
    let socket = root.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(ipc::serve(listener, owner.clone(), Default::default()));
    let (client, window) = ipc::attach_with_local_frames(socket, &owner).await.unwrap();
    let key = FrameKey {
        plugin: PluginId::new("fixture").unwrap(),
        page: "page".into(),
        canvas: "map".into(),
    };
    let relay = &runtime.page_frames;
    relay.set_page(&key.plugin, &key.page, vec![key.canvas.clone()]);
    let mut reader = FrameReader::new(&client, key.clone()).expect("in-process canvas source");
    assert!(matches!(reader.next().await.unwrap(), FramePoll::Empty));

    let claim = relay.claim(&key, std::time::Instant::now()).unwrap();
    let (mut worker, owner) = tokio::io::duplex(128);
    relay.accept(&key.plugin, claim, owner);
    let mut audit = relay.watch(key.clone()).unwrap();
    let mut wire = [0; MAX_FRAME_HEADER_BYTES];
    for seq in [1u64, 2] {
        let header = FrameHeader {
            id: &key.canvas,
            layout: FrameLayout::new(1, 2, 8).unwrap(),
            seq,
            settled: seq + 7,
        };
        worker
            .write_all(header.encode(&mut wire).unwrap())
            .await
            .unwrap();
        worker
            .write_all(&[seq as u8, 5, 7, 255, 0, 0, 0, 0, 8, 9, 10, 255, 0, 0, 0, 0])
            .await
            .unwrap();
    }
    let latest = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        audit
            .latest
            .wait_for(|frame| frame.as_ref().is_some_and(|frame| frame.seq == 2)),
    )
    .await
    .unwrap()
    .unwrap()
    .as_ref()
    .unwrap()
    .clone();
    let FramePoll::Frame(frame) = reader.next().await.unwrap() else {
        panic!("expected the latest in-process raster");
    };
    assert_eq!(
        (frame.width, frame.height, frame.seq, frame.settled),
        (1, 2, 2, 9)
    );
    assert_eq!(frame.pixels.as_ref(), &[2, 5, 7, 255, 8, 9, 10, 255]);
    assert!(std::ptr::eq(frame.pixels.as_ref(), latest.pixels.as_ref()));

    drop(audit);
    drop(reader);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), worker.read_u8())
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert!(relay.claim(&key, std::time::Instant::now()).is_none());

    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(1), window)
        .await
        .unwrap()
        .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn an_extension_keeps_layers_for_its_drawings_that_windows_fetch_and_that_go_with_it() {
    use crate::drawing_layers::{FORGET, KEEP, SERVICE};
    use peersus_api::message::Outcome;
    use serde_json::json;
    let drawing = |name: &str| plugin_importing(name, HOST, SERVICE, 1, &[KEEP, FORGET]);
    let mut registry = Registry::default();
    registry.register(drawing("map")).unwrap();
    registry.register(drawing("chart")).unwrap();
    registry
        .register(plugin_importing(
            "quiet",
            HOST,
            PLATFORM_SERVICE,
            1,
            &["inspect"],
        ))
        .unwrap();
    // Offered only to the extensions that asked for it, and with no permission: what it keeps is the caller's own.
    let exports = host_exports(&registry);
    let offered = exports
        .iter()
        .find(|export| export.service == SERVICE)
        .unwrap();
    assert_eq!(
        offered.consumers,
        ["chart".to_owned(), "map".to_owned()].into()
    );
    assert_eq!(
        offered
            .methods
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [FORGET, KEEP]
    );
    assert!(
        offered
            .methods
            .values()
            .all(|member| member.capability.is_none())
    );
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    let (map, chart) = (
        PluginId::new("map").unwrap(),
        PluginId::new("chart").unwrap(),
    );
    let call = |runtime: &mut Runtime, caller: &str, method: &str, args: serde_json::Value| {
        runtime.answer_plugin_call(caller, SERVICE, method, &args).0
    };
    let tile = |id: &str, outline: &str| json!({"id": id, "shapes": [{"path": outline, "stroke": "accent", "line": 20}]});
    let ok = |outcome: &Outcome| matches!(outcome, Outcome::Ok { value } if *value == json!({}));
    let refused =
        |outcome: &Outcome| matches!(outcome, Outcome::Error { code, .. } if code == "refused");
    let kept = || client.snapshot().kept_layers.clone();
    assert!(kept().is_empty());

    // Kept for the caller alone, and told to the windows by a stamp.
    assert!(ok(&call(
        &mut runtime,
        "map",
        KEEP,
        tile("t/1", "M0 0 L9 9")
    )));
    assert!(ok(&call(
        &mut runtime,
        "map",
        KEEP,
        tile("t/2", "M0 0 L8 8")
    )));
    assert!(ok(&call(
        &mut runtime,
        "chart",
        KEEP,
        tile("t/1", "M1 1 L5 5")
    )));
    let told = kept();
    assert_eq!(
        told.iter()
            .map(|set| set.plugin.as_str())
            .collect::<Vec<_>>(),
        ["chart", "map"]
    );
    assert!(told.iter().all(|set| set.stamp != 0));
    // The same layer again changes nothing, and a window is not asked to look.
    assert!(ok(&call(
        &mut runtime,
        "map",
        KEEP,
        tile("t/1", "M0 0 L9 9")
    )));
    assert_eq!(kept(), told);
    // What is not understood, from a caller that is not an extension, or past the limits, is refused and the set stays.
    for bad in [
        tile("T/1", "M0 0"),
        tile("t/1", "url(#x)"),
        json!({"id":"t/1","shapes":[]}),
        json!({"id":"t/9","shapes":[{"path":"M0 0"}],"extra":true}),
        json!(7),
    ] {
        assert!(
            refused(&call(&mut runtime, "map", KEEP, bad.clone())),
            "{bad}"
        );
    }
    assert!(refused(&call(
        &mut runtime,
        "map",
        FORGET,
        json!({"ids": ["A"]})
    )));
    assert!(matches!(
        call(&mut runtime, "nobody", KEEP, tile("t/1", "M0 0")),
        Outcome::Error { code, .. } if code == "unknown-method"
    ));
    assert_eq!(kept(), told);

    // A window reads the index and then what it lacks, once for each change, and only what is asked for.
    let Ok(Reply::LayerIndex(index)) = runtime
        .execute(Command::LayerIndex {
            plugin: map.clone(),
        })
        .await
    else {
        panic!("the index")
    };
    assert_eq!(index.stamp, told[1].stamp);
    assert_eq!(
        index
            .layers
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["t/1", "t/2"]
    );
    let Ok(Reply::Layers(layers)) = runtime
        .execute(Command::FetchLayers {
            plugin: map.clone(),
            ids: vec!["t/2".into(), "gone".into()],
        })
        .await
    else {
        panic!("the layers")
    };
    assert_eq!(layers.len(), 1);
    assert_eq!(
        (layers[0].id.as_str(), layers[0].stamp),
        ("t/2", index.layers[1].1)
    );
    assert_eq!(layers[0].shapes[0].path, "M0 0 L8 8");

    // Letting go: by id, and of what is not kept nothing.
    assert!(ok(&call(
        &mut runtime,
        "map",
        FORGET,
        json!({"ids": ["t/2", "t/7"]})
    )));
    assert_ne!(kept()[1].stamp, told[1].stamp);
    assert_eq!(kept()[0], told[0], "another extension's set is untouched");
    let after = kept();
    assert!(ok(&call(
        &mut runtime,
        "map",
        FORGET,
        json!({"ids": ["t/7"]})
    )));
    assert_eq!(kept(), after);

    // The optional page features are those the contract names, and the settle-back that was is not among them.
    let Outcome::Ok { value } = runtime
        .answer_plugin_call("map", CONFIG_SERVICE, "get", &json!({}))
        .0
    else {
        panic!("the configuration")
    };
    let expected: Vec<_> = peersus_api::elements::PAGE_FEATURES
        .into_iter()
        .chain([peersus_api::elements::FEATURE_CANVAS])
        .collect();
    assert_eq!(
        value[peersus_api::elements::PAGE_FEATURES_KEY],
        json!(expected)
    );
    let features = value["page_features"].as_array().unwrap();
    assert!(features.contains(&json!("layers")) && features.contains(&json!("follow")));
    assert!(!features.contains(&json!("transition")));

    // Gone with the extension: stopped, turned off or removed.
    runtime.stop_plugin(&map).await;
    runtime.publish();
    assert_eq!(
        kept()
            .iter()
            .map(|set| set.plugin.as_str())
            .collect::<Vec<_>>(),
        ["chart"]
    );
    let Ok(Reply::LayerIndex(index)) = runtime
        .execute(Command::LayerIndex {
            plugin: map.clone(),
        })
        .await
    else {
        panic!("the index")
    };
    assert_eq!((index.stamp, index.layers.len()), (0, 0));
    runtime.stop_plugin(&chart).await;
    runtime.publish();
    assert!(kept().is_empty());
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
    assert_eq!(
        receiving.get(&2).map(|entry| entry.owner.plugin.as_str()),
        Some("sink")
    );
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
    let missing = || {
        Some(admission::Refusal::MissingPermission {
            plugin: PluginId::new("sink").unwrap(),
            permission: Permission::new("sink", "sink.write").unwrap(),
        })
    };
    // Paired, but nothing granted: default-deny.
    let none = [peer_granted("p", &[])];
    assert_eq!(
        admit(&registry, &none, &PeerId("p".into()), "sink.service", 1).err(),
        missing()
    );
    // Granted a different capability of the same plugin: still not this one.
    let wrong = [peer_granted("p", &[("sink", &[("sink", "sink.other")])])];
    assert_eq!(
        admit(&registry, &wrong, &PeerId("p".into()), "sink.service", 1).err(),
        missing()
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
        missing()
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
        Some(admission::Refusal::Failed("Device is not paired"))
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
        Some(admission::Refusal::ExtensionAbsent)
    );
    // The same service at another version is not a best-effort match.
    assert_eq!(
        admit(&registry, &peers, &PeerId("p".into()), "sink.service", 2).err(),
        Some(admission::Refusal::ExtensionAbsent)
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
    assert!(
        super::admission::accepted_from(&registry, &BTreeSet::new(), &allowed, &BTreeMap::new())
            .is_empty()
    );
    let taken = super::admission::accepted_from(&registry, &running, &allowed, &BTreeMap::new());
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
    assert!(
        super::admission::accepted_from(&registry, &running, &nothing, &BTreeMap::new()).is_empty()
    );
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
    assert_eq!(
        host.submit(call(1)),
        Err(peersus_api::Error::MissingPermission(permission(
            "alpha", "cap"
        )))
    );
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
    #[cfg(target_os = "android")]
    services.remove(crate::page_frames::SERVICE);
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
                ..Manifest::new(peersus_api::manifest::Execution::Wasm {
                    abi: peersus_api::manifest::EXECUTION_ABI_VERSION,
                })
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
            ..Manifest::new(peersus_api::manifest::Execution::Process)
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
            Err(peersus_api::error::Error::MissingPermission(permission(
                HOST,
                if event == events::TRANSFERS_CHANGED {
                    events::TRANSFERS_WATCH_CAPABILITY
                } else {
                    events::TRANSFERS_NAMES_CAPABILITY
                }
            ))),
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
        Err(peersus_api::error::Error::MissingPermission(permission(
            HOST,
            events::TRANSFERS_NAMES_CAPABILITY,
        ))),
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
    let infos = [info("clip", Running), info("sink", Failed)];
    let listed = plugins_listing(infos.iter().map(|p| (&p.id, p.status)));
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
    assert_eq!(plugins_listing([]), serde_json::json!([]));
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
    // The broker knows the exact missing guard but still reveals no device.
    for (id, method, guard) in [
        (1, "list", PEERS_WATCH_CAPABILITY),
        (2, "send", PEERS_SEND_CAPABILITY),
    ] {
        assert_eq!(
            clip.submit(call_peers(id, method)),
            Err(peersus_api::Error::MissingPermission(permission(
                HOST, guard
            ))),
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
        configuration(&spec, None, &aliases, std::slice::from_ref(&peer), None)["peers"],
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
    assert_eq!(entries[0]["allowed"], true);
    assert_eq!(entries[0]["missing_permissions"], serde_json::json!([]));
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
fn permission_readiness_reports_only_missing_declarations_for_visible_devices() {
    use serde_json::json;
    let aliases = crate::aliases::Aliases::new();
    let clip = PluginId::new("clip").unwrap();
    let send = permission(HOST, PEERS_SEND_CAPABILITY);
    let extra = permission("clip", "clip.use");
    let asked = [send.clone(), extra.clone()].into();
    let mut visible = recognisable_peer(true);
    let hidden = peer_granted("hidden", &[("clip", &[(HOST, PEERS_SEND_CAPABILITY)])]);
    let peers = [visible.clone(), hidden];
    let listed = peers_listing(&aliases, &clip, &asked, &peers);
    let rows = listed.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["missing_permissions"], json!([extra]));
    assert_eq!(rows[0]["allowed"], false);
    visible.grants.insert(
        clip.clone(),
        Grants::new([send, extra, permission(HOST, PEERS_WATCH_CAPABILITY)]).unwrap(),
    );
    let listed = peers_listing(&aliases, &clip, &asked, &[visible]);
    assert_eq!(listed[0]["missing_permissions"], json!([]));
    assert_eq!(listed[0]["allowed"], true);
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

#[tokio::test]
async fn an_action_can_finish_without_showing_its_result() {
    use peersus_api::message::Outcome;
    use peersus_core::model::ACTION_RESULT_NOT_SHOWN;

    let spec = plugin_with_guarded_action();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    let generation = runtime
        .hub
        .register(spec.id.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(spec.id.as_str(), generation).unwrap();
    runtime
        .hub
        .set_permissions(HOST, [permission(spec.id.as_str(), "cap")].into())
        .unwrap();
    runtime
        .execute(Command::InvokeAction {
            plugin: spec.id.clone(),
            action: "go".into(),
            peer: None,
        })
        .await
        .unwrap();
    let Some(Delivery::Call { ticket, .. }) = guest.try_recv().unwrap() else {
        panic!("the action did not reach the guest");
    };
    guest
        .submit(Operation::Reply {
            ticket,
            result: Outcome::error("not-shown", "This is untrusted guest text"),
        })
        .unwrap();
    let result = runtime.host.try_recv().unwrap().unwrap();
    assert!(runtime.handle_host_delivery(result).await);
    runtime.publish();

    let snapshot = client.snapshot();
    let [status] = snapshot.actions.as_slice() else {
        panic!("expected exactly one action status");
    };
    assert_eq!(status.plugin, spec.id);
    assert_eq!(status.action, "go");
    assert_eq!(status.state, ActionState::DoneUnshown);
    assert_eq!(
        snapshot.errors,
        [format!("[{}] {ACTION_RESULT_NOT_SHOWN}", spec.id.as_str())]
    );
}

#[tokio::test]
async fn a_missing_action_permission_is_blocked_and_a_real_failure_still_fails() {
    use peersus_api::message::Outcome;
    use peersus_core::model::NotificationSource;

    let spec = plugin_with_guarded_action();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry_with_alpha()).await;
    let required = permission(spec.id.as_str(), "cap");
    let generation = runtime
        .hub
        .register(spec.id.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(spec.id.as_str(), generation).unwrap();
    let command = || Command::InvokeAction {
        plugin: spec.id.clone(),
        action: "go".into(),
        peer: None,
    };
    for _ in 0..2 {
        assert!(matches!(
            runtime.execute(command()).await,
            Ok(Reply::MissingPermission { permission }) if permission == required
        ));
        assert!(guest.try_recv().unwrap().is_none());
    }
    runtime.publish();
    let blocked = client.snapshot();
    assert_eq!(blocked.actions[0].state, ActionState::Blocked);
    assert!(blocked.errors.is_empty());
    assert_eq!(blocked.errors_said, 0);
    assert!(matches!(
        blocked.notifications.as_slice(),
        [card] if matches!(&card.source, NotificationSource::MissingPermission(cause)
            if cause.plugin == spec.id && cause.permission == required && cause.peer.is_none())
    ));

    runtime
        .hub
        .set_permissions(HOST, [required.clone()].into())
        .unwrap();
    assert!(matches!(
        runtime.execute(command()).await,
        Ok(Reply::Applied)
    ));
    let Some(Delivery::Call { ticket, .. }) = guest.try_recv().unwrap() else {
        panic!("granted action did not reach the extension");
    };
    guest
        .submit(Operation::Reply {
            ticket,
            result: Outcome::error("unavailable", "untrusted fixture text"),
        })
        .unwrap();
    assert!(
        runtime
            .handle_host_delivery(runtime.host.try_recv().unwrap().unwrap())
            .await
    );
    runtime.publish();
    let failed = client.snapshot();
    assert_eq!(
        failed.actions[0].state,
        ActionState::Failed {
            reason: ActionFailure::Unavailable
        }
    );
    assert_eq!(failed.errors_said, 1);
    assert_eq!(
        failed.errors,
        [format!(
            "[{}] {}",
            spec.id.as_str(),
            ActionFailure::Unavailable.notice()
        )]
    );
}

#[tokio::test]
async fn missing_permission_readiness_changes_without_polling_or_repeating_a_refused_call() {
    use peersus_api::{manifest::Import, message::Outcome};
    use peersus_core::model::NotificationSource;
    use serde_json::json;

    let mut spec = plugin_importing("fixture", HOST, PLATFORM_SERVICE, 1, &["inspect"]);
    spec.manifest.imports.extend([
        Import {
            provider: HOST.into(),
            service: CONFIG_SERVICE.into(),
            version: 1,
            methods: ["get".into()].into(),
            events: [CONFIG_CHANGED.into()].into(),
        },
        plugin_wanting_peers("fixture", &["list"])
            .manifest
            .imports
            .remove(0),
    ]);
    let plugin = spec.id.clone();
    let capability = permission(HOST, PLATFORM_READ_CAPABILITY);
    let watch = permission(HOST, PEERS_WATCH_CAPABILITY);
    let root = tempfile::tempdir().unwrap();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    // Exercise the real owner loop, without launching any executable.
    runtime.disabled.insert(plugin.clone());
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    guest
        .submit(Operation::Subscribe {
            provider: HOST.into(),
            service: CONFIG_SERVICE.into(),
            version: 1,
            event: CONFIG_CHANGED.into(),
        })
        .unwrap();
    let owner = tokio::spawn(runtime.run());
    let call = |id, service: &str, method: &str| Operation::Call {
        id,
        provider: HOST.into(),
        service: service.into(),
        version: 1,
        method: method.into(),
        args: json!({}),
        timeout_ms: 1_000,
    };
    let receive = || async {
        tokio::time::timeout(Duration::from_secs(3), guest.receive())
            .await
            .unwrap()
            .unwrap()
    };
    guest.submit(call(1, CONFIG_SERVICE, "get")).unwrap();
    let Delivery::Result {
        id: 1,
        result: Outcome::Ok { value },
    } = receive().await
    else {
        panic!("expected caller configuration");
    };
    assert_eq!(value["missing_permissions"], json!([watch, capability]));
    assert_eq!(value["peers"], json!([]));

    let refused = guest
        .submit(call(2, PLATFORM_SERVICE, "inspect"))
        .unwrap_err();
    guest.reject(2, &refused).unwrap();
    assert_eq!(
        receive().await,
        Delivery::Result {
            id: 2,
            result: Outcome::MissingPermission {
                permission: capability.clone()
            },
        }
    );
    let mut snapshots = client.subscribe();
    let state = tokio::time::timeout(
        Duration::from_secs(3),
        snapshots.wait_for(|state| !state.notifications.is_empty()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(state.errors.is_empty());
    assert_eq!(state.errors_said, 0);
    assert!(matches!(state.notifications.as_slice(), [card] if
        matches!(&card.source, NotificationSource::MissingPermission(cause) if cause.permission == capability)));
    drop(state);

    assert!(matches!(
        client
            .command(Command::Allow {
                plugin: plugin.clone(),
                peer: None,
                permission: capability,
                on: true,
            })
            .await,
        Ok(Reply::Applied)
    ));
    assert_eq!(
        receive().await,
        Delivery::Event {
            provider: HOST.into(),
            service: CONFIG_SERVICE.into(),
            version: 1,
            event: CONFIG_CHANGED.into(),
            data: json!({}),
        }
    );
    assert!(client.snapshot().notifications.is_empty());
    guest.submit(call(3, CONFIG_SERVICE, "get")).unwrap();
    let Delivery::Result {
        id: 3,
        result: Outcome::Ok { value },
    } = receive().await
    else {
        panic!("expected updated caller configuration");
    };
    assert_eq!(value["missing_permissions"], json!([watch]));
    client.command(Command::Shutdown {}).await.unwrap();
    owner.await.unwrap();
}

#[test]
fn action_error_codes_become_their_specific_reasons() {
    use peersus_api::message::Outcome;
    use peersus_core::model::{ActionFailure::*, ActionState::Failed};
    let cases = [
        ("denied", NotAllowed),
        ("unavailable", Unavailable),
        ("stopped", Stopped),
        ("timeout", TimedOut),
        ("invalid-deadline", TimedOut),
        ("busy", Busy),
        ("unknown-method", Other),
        ("version", Other),
        ("rate-limited", RateLimited),
        ("too-large", TooLarge),
        ("refused", Refused),
        ("credential-rejected", CredentialRejected),
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
        "not-shown ",
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
    for (code, expected) in [
        ("refused", "The request was refused"),
        (
            "rate-limited",
            "The destination's limit was reached. Try again later.",
        ),
        (
            "too-large",
            "What was sent is larger than the destination accepts.",
        ),
        (
            "credential-rejected",
            "The saved credential was not accepted. Save a valid one.",
        ),
    ] {
        assert_eq!(told(code), Some(expected));
    }
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
    // Non-action results do not use the completion warning as success.
    assert_eq!(told("not-shown"), told("disk-full"));
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
    let attempts = [
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
    ];
    for attempt in attempts {
        assert_eq!(
            attempt.expect_err("all three are refused"),
            SendRefusal::Refused(SEND_REFUSED),
            "a different refusal would reveal a device that was not offered"
        );
    }
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
    assert_eq!(unknown_device, SendRefusal::Refused(malformed));
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
    let (_revoke, revoked) = watch::channel(false);

    tokio::time::timeout(Duration::from_secs(15), async {
        let (outcome, received, confirmed) = tokio::join!(
            carry_out(
                connection,
                request,
                receiver,
                Duration::from_secs(5),
                async || {},
                SendGates {
                    revoked,
                    refused: None,
                    visit: None
                },
            ),
            async {
                let (send, mut receive) = peer.accept_bi().await.unwrap();
                let preamble: peersus_net::streams::Request =
                    peersus_net::streams::receive(&mut receive).await.unwrap();
                assert!(matches!(
                    preamble,
                    peersus_net::streams::Request::Plugin { service, version: 1, visit: None }
                        if service == "sink.service"
                ));
                let bytes = receive.read_to_end(1024).await.unwrap();
                crate::receive::write_terminal(
                    send,
                    crate::receive::Terminal::received(bytes.len() as u64),
                )
                .await;
                bytes
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
    let (_revoke, revoked) = watch::channel(false);
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        carry_out(
            connection,
            request,
            receiver,
            Duration::from_millis(600),
            async || {},
            SendGates {
                revoked,
                refused: None,
                visit: None,
            },
        ),
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
            public_read: None,
            platforms: Vec::new(),
            id: "automatic".into(),
            label: "Automatic".into(),
            help: None,
            scope: Scope::Global,
            kind: Kind::Toggle { default: false },
        },
        Setting {
            public_read: None,
            platforms: Vec::new(),
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
    let answer = configuration(
        &spec,
        Some(&saved),
        &aliases,
        &[peer.clone(), stranger],
        None,
    );
    assert_eq!(
        answer,
        serde_json::json!({
            "plugin": spec.id.as_str(),
            "settings": {"automatic": false},
            "missing_permissions": [],
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
            Err(peersus_api::Error::MissingPermission(grant.clone()))
        );
        assert!(host.try_recv().unwrap().is_none());
        hub.set_permissions("upload", [grant.clone()].into())
            .unwrap();
        guest.submit(call(2)).unwrap();
        let Some(Delivery::Call { ticket, caller, .. }) = host.try_recv().unwrap() else {
            panic!("granted call did not reach the host");
        };
        assert_eq!(caller, "upload");
        hub.set_permissions("upload", BTreeSet::new()).unwrap();
        assert_eq!(
            guest.submit(call(3)),
            Err(peersus_api::Error::MissingPermission(grant))
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tokio::test]
async fn granting_selected_file_use_clears_an_earlier_denial_and_allows_retry() {
    use crate::platform_services::{FILES_CAPABILITY, FILES_SERVICE};
    use peersus_api::{Error, contributions::ActionInput, manifest::Import, message::Outcome};
    use serde_json::json;

    let mut spec = plugin_with_guarded_action();
    spec.manifest.exports[0]
        .methods
        .get_mut("go")
        .unwrap()
        .capability = None;
    let action = &mut spec.manifest.contributions.actions[0];
    action.capability = None;
    action.input = ActionInput::SelectedFile;
    spec.manifest.imports.push(Import {
        provider: HOST.into(),
        service: FILES_SERVICE.into(),
        version: 1,
        methods: ["selected".into()].into(),
        events: BTreeSet::new(),
    });
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("chosen.txt");
    std::fs::write(&file, b"test").unwrap();
    let (runtime, client) = loopback_runtime(root.path(), registry).await;
    let mut runtime =
        runtime.with_platform_services(crate::desktop_services::DesktopServices::new(
            root.path().into(),
            root.path().join("public-approvals"),
        ));
    let plugin = &spec.id;
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    let state = || {
        client
            .snapshot()
            .actions
            .iter()
            .find(|entry| entry.plugin == *plugin && entry.action == "go" && entry.peer.is_none())
            .map(|entry| entry.state)
    };
    let denied = ActionState::Blocked;
    for (id, change, expected) in [
        (1, None, denied),
        (2, Some(false), denied),
        (3, Some(true), ActionState::Done),
    ] {
        if let Some(on) = change {
            runtime
                .execute(Command::Allow {
                    plugin: plugin.clone(),
                    peer: None,
                    permission: permission(HOST, FILES_CAPABILITY),
                    on,
                })
                .await
                .unwrap();
            runtime.publish();
            assert_eq!(state(), if on { None } else { Some(denied) });
        }

        runtime
            .execute(Command::InvokeSelectedPathAction {
                plugin: plugin.clone(),
                action: "go".into(),
                path: file.clone(),
            })
            .await
            .unwrap();
        runtime.publish();
        assert_eq!(state(), Some(ActionState::Running));
        let Some(Delivery::Call { ticket, args, .. }) = guest.try_recv().unwrap() else {
            panic!("the press did not reach the guest");
        };
        let submitted = guest.submit(Operation::Call {
            id,
            provider: HOST.into(),
            service: FILES_SERVICE.into(),
            version: 1,
            method: "selected".into(),
            args: json!({ "context": args["context"] }),
            timeout_ms: 5_000,
        });
        if expected == denied {
            let missing = Error::MissingPermission(permission(HOST, FILES_CAPABILITY));
            assert_eq!(submitted.as_ref(), Err(&missing));
            guest.reject(id, &missing).unwrap();
        } else {
            submitted.unwrap();
            let call = runtime.host.try_recv().unwrap().unwrap();
            assert!(!runtime.handle_host_delivery(call).await);
            let completed = runtime.platform_calls.next().await.unwrap();
            let result = completed.result.unwrap();
            assert!(matches!(&result, Outcome::Ok { value } if value["bytes"] == json!(4)));
            runtime
                .host
                .submit(Operation::Reply {
                    ticket: completed.ticket,
                    result,
                })
                .unwrap();
        }
        let Some(Delivery::Result {
            id: answered,
            result,
        }) = guest.try_recv().unwrap()
        else {
            panic!("the guest did not receive a result for its platform call");
        };
        assert_eq!(answered, id);
        guest.submit(Operation::Reply { ticket, result }).unwrap();
        let result = runtime.host.try_recv().unwrap().unwrap();
        assert!(runtime.handle_host_delivery(result).await);
        runtime.publish();
        assert_eq!(state(), Some(expected));
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
#[tokio::test]
async fn a_successful_cli_file_action_replaces_the_cancelled_result_in_window_ipc() {
    use crate::{cli, ipc};
    use clap::Parser;
    use peersus_api::{contributions::ActionInput, message::Outcome};

    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fixture.txt");
    std::fs::write(&file, [0_u8; 134]).unwrap();
    let mut spec = plugin_with_guarded_action();
    spec.program = None;
    spec.manifest.execution = peersus_api::manifest::Execution::Wasm { abi: 1 };
    spec.manifest.exports[0]
        .methods
        .get_mut("go")
        .unwrap()
        .capability = None;
    let action = &mut spec.manifest.contributions.actions[0];
    action.input = ActionInput::SelectedFile;
    action.capability = None;
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let (runtime, owner) = loopback_runtime(root.path(), registry).await;
    let mut runtime =
        runtime.with_platform_services(crate::desktop_services::DesktopServices::new(
            root.path().into(),
            root.path().join("public-approvals"),
        ));
    let (meter_updates, receiver) =
        watch::channel(crate::platform_services::ActionProgress::default());
    runtime.action_progress_changes = Some(receiver);
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    let socket = root.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn({
        let owner = owner.clone();
        async move {
            ipc::serve(listener, owner, Default::default())
                .await
                .unwrap()
        }
    });
    tasks.spawn(async move {
        let _ = runtime.run().await;
    });
    let (window, following) = ipc::attach(socket.clone()).await.unwrap();
    let mut window_states = window.subscribe();
    let state = |snapshot: &Snapshot| {
        snapshot
            .actions
            .iter()
            .find(|entry| entry.plugin == plugin && entry.action == "go" && entry.peer.is_none())
            .map(|entry| entry.state)
    };
    let observe = |states: &mut watch::Receiver<Arc<Snapshot>>, expected| {
        let mut states = states.clone();
        async move {
            tokio::time::timeout(
                Duration::from_secs(3),
                states.wait_for(|snapshot| state(snapshot) == Some(expected)),
            )
            .await
            .expect("the window IPC never observed the action state")
            .unwrap();
        }
    };
    window
        .command(Command::InvokeSelectedPathAction {
            plugin: plugin.clone(),
            action: "go".into(),
            path: file.clone(),
        })
        .await
        .unwrap();
    let Delivery::Call {
        ticket: cancelled, ..
    } = guest.receive().await.unwrap()
    else {
        panic!("missing first action");
    };
    observe(&mut window_states, ActionState::Running).await;
    window
        .command(Command::CancelAction {
            plugin: plugin.clone(),
            action: "go".into(),
            peer: None,
        })
        .await
        .unwrap();
    assert_eq!(
        guest.receive().await.unwrap(),
        Delivery::Cancelled { ticket: cancelled }
    );
    observe(&mut window_states, ActionState::Cancelled).await;
    assert_eq!(state(&owner.snapshot()), Some(ActionState::Cancelled));

    let console = cli::Cli::try_parse_from([
        "peersus".into(),
        "run-file".into(),
        plugin.as_str().into(),
        "go".into(),
        file.into_os_string(),
    ])
    .unwrap();
    assert!(matches!(
        console
            .execute(&ipc::LocalControl::new(socket.clone()))
            .await
            .unwrap(),
        cli::Output::Reply(Reply::Applied)
    ));
    let Delivery::Call { ticket, args, .. } = guest.receive().await.unwrap() else {
        panic!("missing command-line action");
    };
    assert_ne!(ticket, cancelled);
    observe(&mut window_states, ActionState::Running).await;
    let context = args["context"].as_u64().unwrap();
    let meter = Arc::new(crate::platform_services::file_http::FileProgress::new(
        context,
        plugin.clone(),
        134,
    ));
    assert!(meter.consume(0, 67));
    meter_updates.send_modify(|running| {
        running.insert(context, meter);
    });
    tokio::time::timeout(
        Duration::from_secs(3),
        window_states.wait_for(|snapshot| {
            snapshot.actions.first().is_some_and(|entry| {
                entry.state == ActionState::Running && entry.progress == Some((67, 134))
            })
        }),
    )
    .await
    .expect("window IPC never observed the progress tuple")
    .unwrap();
    guest
        .submit(Operation::Reply {
            ticket,
            result: Outcome::ok(serde_json::Value::Null),
        })
        .unwrap();
    observe(&mut window_states, ActionState::Done).await;
    assert_eq!(state(&owner.snapshot()), Some(ActionState::Done));
    assert_eq!(
        state(&ipc::snapshot(&socket).await.unwrap()),
        Some(ActionState::Done)
    );
    assert_eq!(window.snapshot().actions.len(), 1);
    assert_eq!(window.snapshot().actions[0].progress, None);
    drop(window_states);
    drop(window);
    following.await.unwrap();
    owner.command(Command::Shutdown {}).await.unwrap();
    tasks.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires explicit PEERSUS_UPLOAD_WASM and PEERSUS_WASM_HOST built artifacts"]
async fn compiled_upload_wasm_cancel_action_releases_the_live_file_without_revocation() {
    use crate::platform_services::{
        FILES_CAPABILITY, HTTP_CAPABILITY, PlatformServices,
        file_http::{PausedUpload, upload_artifact},
    };
    use peersus_core::model::PluginStatus;
    use peersus_plugin::module::ModuleLauncher;
    use tokio::time::timeout;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("selected.bin");
    let total = 6 * 1024 * 1024;
    std::fs::write(&source, vec![0xa5; total]).unwrap();
    let mut upload = PausedUpload::catbox(root.path().to_owned()).await;

    let plugin = PluginId::new("upload").unwrap();
    let manifest_bytes = std::fs::read(upload_artifact("PEERSUS_UPLOAD_MANIFEST")).unwrap();
    let package = peersus_api::package::split(&manifest_bytes).unwrap();
    let mut registry = Registry::default();
    registry
        .register(PluginSpec {
            id: plugin.clone(),
            manifest: serde_json::from_slice(&package.manifest).unwrap(),
            program: None,
            arguments: Vec::new(),
            restart_policy: None,
            languages: Default::default(),
        })
        .unwrap();
    let mut grants = StandaloneGrants::open(root.path().join("grants.json")).unwrap();
    grants
        .set(
            plugin.clone(),
            Grants::new([FILES_CAPABILITY, HTTP_CAPABILITY].map(|name| permission(HOST, name)))
                .unwrap(),
        )
        .unwrap();
    let (runtime, owner) = loopback_runtime(root.path(), registry).await;
    let launcher = ModuleLauncher::new(
        upload_artifact("PEERSUS_WASM_HOST"),
        upload_artifact("PEERSUS_UPLOAD_WASM"),
    )
    .unwrap();
    let runtime = runtime
        .with_platform_services(upload.platform.clone())
        .with_launcher(plugin.clone(), Arc::new(launcher));
    let mut running = tokio::spawn(runtime.run());
    let mut driver = tokio::spawn({
        let owner = owner.clone();
        async move {
            let action = "upload_anonymous";
            let mut snapshots = owner.subscribe();
            snapshots
                .wait_for(|s| {
                    s.plugins
                        .iter()
                        .any(|p| p.id == plugin && p.status == PluginStatus::Running)
                })
                .await
                .unwrap();
            owner
                .command(Command::InvokeSelectedPathAction {
                    plugin: plugin.clone(),
                    action: action.into(),
                    path: source,
                })
                .await
                .unwrap();
            upload.wait_for_headers().await;
            snapshots
                .wait_for(|s| {
                    s.actions.iter().any(|row| {
                        row.plugin == plugin
                            && row.action == action
                            && row.state == ActionState::Running
                            && row.progress.is_some_and(|(sent, all)| {
                                all == total as u64 && 0 < sent && sent < all
                            })
                    })
                })
                .await
                .unwrap();
            let (generation, files, authorizations) = upload.resources(&plugin);
            assert_eq!((files, authorizations), (1, 1));
            owner
                .command(Command::CancelAction {
                    plugin: plugin.clone(),
                    action: action.into(),
                    peer: None,
                })
                .await
                .unwrap();
            let mut progress = upload.platform.action_progress().unwrap();
            progress.wait_for(|files| files.is_empty()).await.unwrap();
            let (current_generation, files, authorizations) = upload.resources(&plugin);
            assert_eq!(
                current_generation, generation,
                "cleanup must not rely on revocation"
            );
            assert_eq!((files, authorizations), (0, 0));
            assert!(owner.snapshot().actions.iter().any(|row| {
                row.plugin == plugin && row.action == action && row.state == ActionState::Cancelled
            }));
            upload.assert_aborted().await;
        }
    });
    let checked = timeout(Duration::from_secs(45), &mut driver).await;
    if checked.is_err() {
        driver.abort();
        let _ = driver.await;
    }
    // Join the real owner/worker even when a driver assertion failed.
    let shutdown_wait = Duration::from_secs(5);
    timeout(shutdown_wait, owner.command(Command::Shutdown {}))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        timeout(shutdown_wait, &mut running).await.unwrap().unwrap(),
        Exit::Stopped
    ));
    checked
        .expect("the live cancelled upload did not release its file")
        .unwrap();
}

#[tokio::test]
async fn file_progress_is_coalesced_and_terminal_rows_expire_in_an_idle_owner() {
    use crate::platform_services::file_http::FileProgress;
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, owner) = loopback_runtime(root.path(), Registry::default()).await;
    tokio::time::pause();
    let key = |action: &str| Key {
        plugin: PluginId::new("fixture").unwrap(),
        action: action.to_owned(),
        device: None,
    };
    let first = key("first");
    let second = key("second");
    runtime.board.file_running(first.clone(), 8);
    runtime.board.file_running(second.clone(), 9);
    let finished = key("finished");
    runtime.board.running(finished.clone());
    runtime.board.finished(&finished, ActionState::Done);
    let expiry = runtime.board.next_expiry().unwrap();
    let one = Arc::new(FileProgress::new(8, first.plugin.clone(), 100));
    let two = Arc::new(FileProgress::new(9, second.plugin.clone(), 100));
    let (change, changes) = watch::channel([(8, one.clone()), (9, two.clone())].into());
    runtime.action_progress_changes = Some(changes);
    let feed = |meter: &FileProgress, offset, length| {
        if meter.consume(offset, length) {
            change.send_modify(|_| {});
        }
    };
    feed(&one, 0, 5);
    feed(&two, 0, 10);
    let mut states = owner.subscribe();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(runtime.run());
    let bytes = |s: &Snapshot, action: &str| {
        s.actions
            .iter()
            .find(|row| row.action == action)
            .and_then(|row| row.progress)
    };
    let read = |s: &Snapshot| (bytes(s, "first"), bytes(s, "second"));
    let initial = (Some((5, 100)), Some((10, 100)));
    tokio::time::timeout(
        Duration::from_secs(1),
        states.wait_for(|s| read(s) == initial),
    )
    .await
    .unwrap()
    .unwrap();
    let sampled_at = tokio::time::Instant::now();
    for (delta, offset, length) in [(80, 5, 10), (80, 15, 20), (89, 35, 25)] {
        tokio::time::advance(Duration::from_millis(delta)).await;
        feed(&one, offset, length);
        if offset == 15 {
            feed(&two, 10, 70);
        }
        tokio::task::yield_now().await;
        assert_eq!(read(&states.borrow()), initial);
    }
    tokio::time::advance(Duration::from_millis(1)).await;
    let coalesced = (Some((60, 100)), Some((80, 100)));
    tokio::time::timeout(
        Duration::from_millis(10),
        states.wait_for(|s| read(s) == coalesced),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(sampled_at.elapsed() >= Duration::from_millis(250));
    let before_expiry = expiry - tokio::time::Instant::now() - Duration::from_millis(1);
    tokio::time::advance(before_expiry).await;
    assert!(
        states
            .borrow()
            .actions
            .iter()
            .any(|row| row.action == "finished" && row.state == ActionState::Done)
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::time::timeout(
        Duration::from_millis(10),
        states.wait_for(|s| s.actions.iter().all(|row| row.action != "finished")),
    )
    .await
    .expect("idle owner did not publish expiry")
    .unwrap();
    assert_eq!(
        read(&states.borrow()),
        coalesced,
        "expiry discarded unrelated running actions"
    );
    owner.command(Command::Shutdown {}).await.unwrap();
    assert!(matches!(
        tasks.join_next().await.unwrap().unwrap(),
        Exit::Stopped
    ));
}

#[tokio::test]
async fn action_offers_follow_broker_generations_even_between_health_samples() {
    use crate::page_offers::ACTIONS_SERVICE;
    use peersus_api::message::Outcome;
    use peersus_plugin::hub::Channel;
    use serde_json::json;

    let mut spec = plugin_with_guarded_action();
    spec.manifest.imports.push(peersus_api::manifest::Import {
        provider: HOST.into(),
        service: ACTIONS_SERVICE.into(),
        version: 1,
        methods: ["withhold".to_owned()].into(),
        events: Default::default(),
    });
    spec.manifest
        .contributions
        .initially_withheld_actions
        .insert("go".into());
    let mut steady = spec.clone();
    steady.id = PluginId::new("steady").unwrap();
    steady.manifest.exports[0].service = "svc.steady".into();
    steady.manifest.contributions.actions[0].target.service = "svc.steady".into();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    registry.register(steady.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, owner) = loopback_runtime(root.path(), registry).await;
    assert!(
        owner
            .snapshot()
            .plugins
            .iter()
            .all(|info| info.manifest.contributions.actions.is_empty())
    );
    let register = |hub: &Hub, plugin: &PluginSpec| {
        let id = plugin.id.as_str();
        let generation = hub
            .register(id, plugin.manifest.clone(), BTreeSet::new())
            .unwrap();
        (generation, hub.connect(id, generation).unwrap())
    };
    let submit = |channel: &Channel, id| {
        channel
            .submit(Operation::Call {
                id,
                provider: HOST.into(),
                service: ACTIONS_SERVICE.into(),
                version: 1,
                method: "withhold".into(),
                args: json!({"actions":[]}),
                timeout_ms: 5_000,
            })
            .unwrap();
    };
    let ticket = |delivery: &Delivery| match delivery {
        Delivery::Call { ticket, .. } => *ticket,
        other => panic!("expected an action offer, got {other:?}"),
    };
    let is_offered = |runtime: &Runtime, plugin: &PluginId| {
        runtime
            .plugin_infos()
            .iter()
            .find(|info| &info.id == plugin)
            .unwrap()
            .manifest
            .contributions
            .actions
            .iter()
            .any(|action| action.id == "go")
    };

    let (first, old) = register(&runtime.hub, &spec);
    let (steady_generation, other) = register(&runtime.hub, &steady);
    assert_eq!(
        runtime.hub.registration_generation(spec.id.as_str()),
        Some(first)
    );
    assert!(!is_offered(&runtime, &spec.id));
    submit(&old, 1);
    let offer = runtime.host.receive().await.unwrap();
    assert_eq!(
        runtime.host.pending_caller_generation(ticket(&offer)),
        Some(first)
    );
    assert_eq!(old.pending_caller_generation(ticket(&offer)), None);
    assert!(runtime.handle_host_delivery(offer).await);
    assert_eq!(
        old.receive().await.unwrap(),
        Delivery::Result {
            id: 1,
            result: Outcome::ok(json!({}))
        }
    );
    assert!(is_offered(&runtime, &spec.id));

    submit(&old, 2);
    let pending_old = runtime.host.receive().await.unwrap();
    let old_ticket = ticket(&pending_old);
    submit(&other, 1);
    let pending_steady = runtime.host.receive().await.unwrap();
    let steady_ticket = ticket(&pending_steady);
    assert_eq!(
        runtime.host.pending_caller_generation(old_ticket),
        Some(first)
    );
    assert_eq!(
        runtime.host.pending_caller_generation(steady_ticket),
        Some(steady_generation)
    );

    // Replace the broker registration before the owner has sampled another health event.
    // The old lift is already inapplicable, and the undelivered answer cannot reapply it.
    let (replacement, new) = register(&runtime.hub, &spec);
    assert_ne!(first, replacement);
    assert_eq!(
        runtime.hub.registration_generation(spec.id.as_str()),
        Some(replacement)
    );
    assert_eq!(
        runtime.hub.registration_generation(steady.id.as_str()),
        Some(steady_generation)
    );
    assert_eq!(runtime.host.pending_caller_generation(old_ticket), None);
    assert_eq!(
        runtime.host.pending_caller_generation(steady_ticket),
        Some(steady_generation)
    );
    assert!(!is_offered(&runtime, &spec.id));
    assert_eq!(
        runtime.invoke(spec.id.clone(), "go", None, None).err(),
        Some("This action is not available now")
    );
    assert!(!runtime.handle_host_delivery(pending_old).await);
    assert!(!is_offered(&runtime, &spec.id));
    assert_eq!(
        runtime.host.receive().await.unwrap(),
        Delivery::Cancelled { ticket: old_ticket }
    );
    assert!(runtime.handle_host_delivery(pending_steady).await);
    assert!(is_offered(&runtime, &steady.id));

    submit(&new, 1);
    let fresh = runtime.host.receive().await.unwrap();
    assert_eq!(
        runtime.host.pending_caller_generation(ticket(&fresh)),
        Some(replacement)
    );
    assert!(runtime.handle_host_delivery(fresh).await);
    assert!(is_offered(&runtime, &spec.id));
    assert!(is_offered(&runtime, &steady.id));
}

#[tokio::test]
async fn a_withheld_selected_file_action_keeps_its_cancel_row_until_it_ends() {
    use crate::page_offers::ACTIONS_SERVICE;
    use peersus_api::{contributions::ActionInput, manifest::Import, message::Outcome};
    use serde_json::json;

    let mut spec = plugin_with_guarded_action();
    spec.manifest.exports[0]
        .methods
        .get_mut("go")
        .unwrap()
        .capability = None;
    let action = &mut spec.manifest.contributions.actions[0];
    action.input = ActionInput::SelectedFile;
    action.capability = None;
    spec.manifest.imports.push(Import {
        provider: HOST.into(),
        service: ACTIONS_SERVICE.into(),
        version: 1,
        methods: ["withhold".to_owned()].into(),
        events: Default::default(),
    });
    let plugin = spec.id.clone();
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, owner) = loopback_runtime(root.path(), registry).await;
    let generation = runtime
        .hub
        .register(plugin.as_str(), spec.manifest, BTreeSet::new())
        .unwrap();
    let guest = runtime.hub.connect(plugin.as_str(), generation).unwrap();
    runtime.invoke(plugin.clone(), "go", None, Some(1)).unwrap();
    let Some(Delivery::Call { ticket, .. }) = guest.try_recv().unwrap() else {
        panic!("the selected action was not dispatched");
    };
    guest
        .submit(Operation::Call {
            id: 1,
            provider: HOST.into(),
            service: ACTIONS_SERVICE.into(),
            version: 1,
            method: "withhold".into(),
            args: json!({"actions":["go"]}),
            timeout_ms: 5000,
        })
        .unwrap();
    let offer = runtime.host.try_recv().unwrap().unwrap();
    assert!(runtime.handle_host_delivery(offer).await);
    assert_eq!(
        guest.try_recv().unwrap(),
        Some(Delivery::Result {
            id: 1,
            result: Outcome::ok(json!({}))
        })
    );

    let shown =
        |snapshot: &Snapshot| {
            snapshot.plugins.iter().any(|info| {
                info.id == plugin
                    && info.manifest.contributions.actions.iter().any(|action| {
                        action.id == "go" && action.input == ActionInput::SelectedFile
                    })
            })
        };
    runtime.publish();
    assert_eq!(owner.snapshot().actions[0].state, ActionState::Running);
    assert!(
        shown(&owner.snapshot()),
        "Send lost the running action's cancel row"
    );
    assert_eq!(
        runtime.invoke(plugin.clone(), "go", None, Some(2)).err(),
        Some("This action is not available now")
    );
    runtime
        .execute(Command::CancelAction {
            plugin: plugin.clone(),
            action: "go".into(),
            peer: None,
        })
        .await
        .unwrap();
    assert_eq!(
        guest.try_recv().unwrap(),
        Some(Delivery::Cancelled { ticket })
    );
    runtime.publish();
    assert_eq!(owner.snapshot().actions[0].state, ActionState::Cancelled);
    assert!(
        !shown(&owner.snapshot()),
        "a finished, withheld action is not offered"
    );
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

/// A package folder `id` in `parent` at `version`, importing the application's plugin service with `methods`.
fn update_package(
    parent: &std::path::Path,
    id: &str,
    version: &str,
    methods: &[&str],
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let directory = parent
        .join(format!("{id}-{version}-{}", methods.join("-")))
        .join(id);
    std::fs::create_dir_all(&directory).unwrap();
    let manifest = serde_json::json!({
        "manifest_version": 1, "execution": {"kind": "process"},
        "capabilities": {},
        "package": {"name": "Upd", "version": version, "author": "Someone", "link": "https://example.org"},
        "imports": [{"provider": HOST, "service": PLUGINS_SERVICE, "version": 1, "methods": methods, "events": []}],
        "exports": [],
        "agreements": {"pages": [
            {"kind": "licence", "title": "MIT", "explains": "You may use it.",
             "changes": "Nothing changes on your devices.", "spdx": "MIT",
             "links": [{"label": "Text", "url": "https://opensource.org/license/mit"}]},
            {"kind": "notice", "title": "What it does", "explains": "It does a thing.", "changes": "Nothing."}
        ]}
    });
    std::fs::write(
        directory.join(crate::discovery::MANIFEST_FILE),
        manifest.to_string(),
    )
    .unwrap();
    let program = directory.join(crate::discovery::PROGRAM_FILE);
    std::fs::write(&program, b"x").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    directory
}

/// Stage `source` as the window or the store would (`from_store` stands for the store's verification, tested in
/// `tests/store.rs`), and confirm it with its own agreement.
async fn install_as(runtime: &mut Runtime, source: &std::path::Path, from_store: bool) {
    runtime.stage_install(source).unwrap();
    let plugin = PluginId::new(source.file_name().unwrap().to_str().unwrap()).unwrap();
    let staged = runtime.staged.get_mut(&plugin).unwrap();
    staged.from_store = from_store;
    let agreed = agreement_digest(&staged.manifest).unwrap();
    runtime
        .confirm_install(plugin, Some(agreed), true)
        .await
        .unwrap();
    runtime.refresh_installed();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_update_keeps_only_what_is_still_asked_for_and_every_other_replacement_starts_with_nothing()
 {
    let root = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let (runtime, _client) = loopback_runtime(root.path(), Registry::default()).await;
    let mut runtime = runtime.with_packages(crate::plugin_manager::Manager::new(root.path()));
    let plugin = PluginId::new("upd").unwrap();
    let (watch, restart) = (
        permission(HOST, PLUGINS_WATCH_CAPABILITY),
        permission(HOST, PLUGINS_RESTART_CAPABILITY),
    );
    let allow_both = |runtime: &mut Runtime| {
        runtime
            .standalone
            .set(
                plugin.clone(),
                Grants::new([watch.clone(), restart.clone()]).unwrap(),
            )
            .unwrap();
    };
    let held = |runtime: &Runtime| -> BTreeSet<Permission> {
        runtime
            .standalone
            .get(&plugin)
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default()
    };

    // From the store, over nothing: new, nothing allowed, and the folder says it came from the store.
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "1.0.0", &["list", "restart"]),
        true,
    )
    .await;
    assert!(held(&runtime).is_empty());
    assert!(crate::plugin_folder::came_from_store(
        &root.path().join("plugins/upd")
    ));
    allow_both(&mut runtime);

    // A newer version from the store that no longer asks to restart: watching is kept, restarting forgotten.
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "1.1.0", &["list"]),
        true,
    )
    .await;
    assert_eq!(held(&runtime), [watch.clone()].into());
    // Asking for restarting again is asking the person again: it was forgotten, not hidden.
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "1.2.0", &["list", "restart"]),
        true,
    )
    .await;
    assert_eq!(held(&runtime), [watch.clone()].into());

    // Not newer, even from the store: a replacement, nothing kept.
    allow_both(&mut runtime);
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "1.2.0", &["restart", "list"]),
        true,
    )
    .await;
    assert!(held(&runtime).is_empty());

    // A newer version from outside the store: a replacement, and the folder no longer says "from the store".
    allow_both(&mut runtime);
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "2.0.0", &["list", "restart"]),
        false,
    )
    .await;
    assert!(held(&runtime).is_empty());
    assert!(!crate::plugin_folder::came_from_store(
        &root.path().join("plugins/upd")
    ));

    // The store over a package put there from outside: the same name is not the same author. Nothing kept.
    allow_both(&mut runtime);
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "3.0.0", &["list", "restart"]),
        true,
    )
    .await;
    assert!(held(&runtime).is_empty());
    // Now installed from the store, the next store update keeps again.
    allow_both(&mut runtime);
    install_as(
        &mut runtime,
        &update_package(parent.path(), "upd", "3.1.0", &["list", "restart"]),
        true,
    )
    .await;
    assert_eq!(held(&runtime), [watch.clone(), restart.clone()].into());
    // And the hub holds exactly that for the running version.
    assert_eq!(
        plugin_grants(
            runtime
                .plugins
                .get(runtime.plugins.find(&plugin).unwrap())
                .unwrap(),
            &runtime.network.peers(),
            runtime.standalone.all()
        ),
        [watch, restart.clone()].into()
    );

    // Same name, more access: `plugins.watch` now also permits a subscription
    // the prior owner never reviewed. The independent restart grant still matches.
    let expanded = update_package(parent.path(), "upd", "3.2.0", &["list", "restart"]);
    let path = expanded.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["imports"][0]["events"] = serde_json::json!([PLUGINS_STATE_CHANGED]);
    std::fs::write(&path, manifest.to_string()).unwrap();
    install_as(&mut runtime, &expanded, true).await;
    assert_eq!(held(&runtime), [restart].into());
}

#[test]
fn a_grant_is_kept_only_while_its_whole_declaration_is_unchanged() {
    use super::permissions::{grantable, unchanged_permissions};
    use peersus_api::{
        contributions::{EdgePressure, Input, Source},
        manifest::{Execution, Manifest},
    };
    let old = plugin_with_guarded_action();
    let cap = permission("alpha", "cap");
    let all = grantable(&old);
    assert!(all.contains(&cap));
    let kept = |replacement: &Manifest| unchanged_permissions(&old, replacement);
    assert_eq!(
        kept(&old.manifest),
        all,
        "an identical declaration keeps everything"
    );
    assert!(kept(&Manifest::new(Execution::Process)).is_empty());

    type Change = fn(&mut Manifest);
    let different: [(&str, Change); 6] = [
        ("the permission's description", |m| {
            m.capabilities
                .insert("cap".into(), "Lets the plugin do more.".into());
        }),
        ("the guarded method", |m| {
            m.exports[0].methods.get_mut("go").unwrap().description = "Does something else.".into();
        }),
        ("who may consume the export", |m| {
            m.exports[0].consumers.insert("other".into());
        }),
        ("an event it also guards", |m| {
            m.exports[0]
                .events
                .insert("done".into(), member(Some("cap")));
        }),
        ("the guarded action", |m| {
            m.contributions.actions[0].destructive = true;
        }),
        ("the unguarded actions a device entry also allows", |m| {
            let mut extra = m.contributions.actions[0].clone();
            extra.id = "extra".into();
            extra.capability = None;
            extra.needs_device = true;
            m.contributions.actions.push(extra);
        }),
    ];
    for (what, change) in different {
        let mut replacement = old.manifest.clone();
        change(&mut replacement);
        assert!(
            kept(&replacement).is_empty(),
            "a changed {what} is another declaration"
        );
    }

    // Something unrelated may be added: it neither drops the grant nor is covered by it.
    let mut added = old.manifest.clone();
    added
        .capabilities
        .insert("unrelated".into(), "Allow the unrelated action.".into());
    added.exports[0]
        .methods
        .insert("other".into(), member(Some("unrelated")));
    let still = kept(&added);
    assert_eq!(still, all);
    assert!(!still.contains(&permission("alpha", "unrelated")));

    let carrier = |method: &str| {
        let mut spec = plugin_with_guarded_action();
        spec.manifest.contributions.input = Some(Input {
            source: Source {
                service: "svc".into(),
                method: method.into(),
            },
            capability: "cap".into(),
            edge_pressure: EdgePressure::Off {},
        });
        spec
    };
    let (before, after) = (carrier("go"), carrier("other"));
    assert!(grantable(&before).contains(&cap));
    assert_eq!(
        unchanged_permissions(&before, &before.manifest),
        grantable(&before)
    );
    assert!(
        unchanged_permissions(&before, &after.manifest).is_empty(),
        "a changed input transport is another declaration"
    );

    // What the application guards on an import is compared member by member.
    let importer = plugin_importing("sink", HOST, LANGUAGE_SERVICE, 1, &["get"]);
    let read = permission(HOST, LANGUAGE_READ_CAPABILITY);
    assert_eq!(
        unchanged_permissions(&importer, &importer.manifest),
        [read.clone()].into()
    );
    let mut widened = importer.manifest.clone();
    widened.imports[0].events.insert(LANGUAGE_CHANGED.into());
    assert!(
        unchanged_permissions(&importer, &widened).is_empty(),
        "a member the person never reviewed"
    );
    // The implicit actions of a device entry matter only to what is granted for a device.
    let mut device_action = old.manifest.contributions.actions[0].clone();
    device_action.capability = None;
    device_action.needs_device = true;
    let mut with_action = importer.manifest.clone();
    with_action.contributions.actions.push(device_action);
    assert_eq!(
        unchanged_permissions(&importer, &with_action),
        [read].into()
    );
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
#[tokio::test(flavor = "multi_thread")]
async fn paired_owners_keep_refusals_neutral_and_interrupt_live_copies_on_either_end() {
    use crate::{
        desktop_input::{DesktopInput, Device, Raw},
        platform_services::{
            Call, INPUT_CAPABILITY, INPUT_SERVICE, InputLease, PlatformServices, Response,
        },
    };
    use peersus_api::{
        contributions::{EdgePressure, Input},
        manifest::Import,
        message::Outcome,
    };
    use peersus_plugin::{
        data::DataHello,
        frame,
        link::Hello,
        supervisor::{Bootstrap, Launcher, Lifecycle},
        transport,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone, Default)]
    struct Keys(Arc<std::sync::Mutex<Vec<Raw>>>, Arc<tokio::sync::Notify>);
    impl Keys {
        async fn wait_for(&self, expected: &[Raw]) {
            within(Duration::from_secs(2), async {
                while self.0.lock().unwrap().as_slice() != expected {
                    self.1.notified().await;
                }
            })
            .await
            .expect("the controlled owner did not release the held input");
        }
    }
    impl Device for Keys {
        fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(events);
            self.1.notify_one();
            Ok(())
        }
    }
    struct Replay(DesktopInput);
    impl PlatformServices for Replay {
        fn dispatch(&self, call: Call) -> Response {
            self.0.dispatch(call)
        }
        fn dispatch_input(&self, call: Call, lease: Option<InputLease>) -> Response {
            self.0.dispatch_leased(call, lease)
        }
        fn release_peer_input(&self, caller: &PluginId, owner: &str) {
            self.0.release_peer(caller, owner);
        }
        fn revoke(&self, caller: &PluginId) {
            self.0.revoke(caller);
        }
    }

    struct FixtureGuest {
        control: transport::Stream,
        bootstrap: Bootstrap,
    }
    impl FixtureGuest {
        async fn submit(&mut self, operation: Operation) {
            frame::write(&mut self.control, &operation, frame::CONTROL_LIMIT)
                .await
                .unwrap();
        }
        async fn data(&self, ticket: u64, claim: Option<u64>) -> transport::Stream {
            let mut stream = transport::connect(&self.bootstrap.data_socket)
                .await
                .unwrap();
            frame::write(
                &mut stream,
                &DataHello {
                    token: self.bootstrap.token.clone(),
                    ticket,
                    claim,
                },
                1024,
            )
            .await
            .unwrap();
            stream
        }
    }

    struct FixtureLauncher(mpsc::Sender<FixtureGuest>);
    struct FixtureProcess(tokio::task::JoinHandle<()>);
    impl Launcher for FixtureLauncher {
        fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
            let linked = self.0.clone();
            Ok(Box::new(FixtureProcess(tokio::spawn(async move {
                let mut control = transport::connect(&bootstrap.socket).await.unwrap();
                frame::write(
                    &mut control,
                    &Hello {
                        token: bootstrap.token.clone(),
                    },
                    1024,
                )
                .await
                .unwrap();
                if linked
                    .send(FixtureGuest { control, bootstrap })
                    .await
                    .is_ok()
                {
                    std::future::pending::<()>().await;
                }
            }))))
        }
    }
    impl Lifecycle for FixtureProcess {
        fn wait(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {
                let _ = (&mut self.0).await;
            })
        }
        fn stop(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.0.abort();
            self.wait()
        }
    }
    impl Drop for FixtureProcess {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn tell(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let mut state = client.subscribe();
        within(Duration::from_secs(8), state.wait_for(|s| predicate(s)))
            .await
            .unwrap()
            .unwrap()
            .clone()
    }
    async fn message(guest: &mut FixtureGuest) -> Delivery {
        within(
            Duration::from_secs(8),
            frame::read(&mut guest.control, frame::CONTROL_LIMIT),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn ask(
        guest: &mut FixtureGuest,
        id: u64,
        service: &str,
        method: &str,
        args: Value,
    ) -> Outcome {
        guest
            .submit(Operation::Call {
                id,
                provider: HOST.to_owned(),
                service: service.to_owned(),
                version: 1,
                method: method.to_owned(),
                args,
                timeout_ms: 3_000,
            })
            .await;
        loop {
            if let Delivery::Result {
                id: received,
                result,
            } = message(guest).await
            {
                assert_eq!(id, received);
                return result;
            }
        }
    }
    async fn grants(
        client: &Client,
        plugin: &PluginId,
        peer: Option<&PeerId>,
        permissions: &[Permission],
    ) {
        client
            .command(Command::SetGrants {
                plugin: plugin.clone(),
                peer: peer.cloned(),
                grants: Grants::new(permissions.iter().cloned()).unwrap(),
            })
            .await
            .unwrap();
    }

    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut calling = plugin_wanting_peers("source", &["list", "send"]);
    calling.manifest.imports.push(Import {
        provider: HOST.into(),
        service: CONFIG_SERVICE.into(),
        version: 1,
        methods: BTreeSet::new(),
        events: BTreeSet::from([CONFIG_CHANGED.to_owned()]),
    });
    let use_capture = permission("source", "source.capture");
    calling
        .manifest
        .capabilities
        .insert(use_capture.name.clone(), "Capture test input.".into());
    let source = Source {
        service: "sink.service".into(),
        method: "forward".into(),
    };
    calling.manifest.contributions.input = Some(Input {
        source: source.clone(),
        capability: use_capture.name.clone(),
        edge_pressure: EdgePressure::Off {},
    });
    calling.manifest.exports.push(Export {
        service: source.service,
        version: 1,
        description: "Forward a captured batch.".into(),
        consumers: BTreeSet::from([HOST.into()]),
        methods: [
            (source.method, member(Some(&use_capture.name))),
            (
                crate::plugin_host::RECEIVE_METHOD.into(),
                member(Some(&use_capture.name)),
            ),
        ]
        .into(),
        events: BTreeMap::new(),
    });
    let mut taking = plugin_that_receives(Some("sink.write"));
    taking.manifest.contributions.input = Some(Input {
        source: Source {
            service: "sink.service".into(),
            method: "forward".into(),
        },
        capability: "sink.write".into(),
        edge_pressure: EdgePressure::Off {},
    });
    taking.manifest.exports[0]
        .methods
        .insert("forward".into(), member(Some("sink.write")));
    taking.manifest.imports.push(Import {
        provider: HOST.into(),
        service: INPUT_SERVICE.into(),
        version: 1,
        methods: BTreeSet::from(["inject".into(), "inspect".into()]),
        events: BTreeSet::new(),
    });
    let mut a_registry = Registry::default();
    a_registry.register(calling.clone()).unwrap();
    let a_endpoint = loopback_endpoint().await;
    let (a, a_client) = loopback_runtime_on(a_dir.path(), a_registry, a_endpoint.clone());
    let mut b_registry = Registry::default();
    b_registry.register(taking.clone()).unwrap();
    let (b, b_client) = loopback_runtime(b_dir.path(), b_registry).await;
    let keys = Keys::default();
    let recorder = keys.clone();
    let input = DesktopInput::new(Box::new(move || Ok(Box::new(recorder.clone()))));
    let b = b.with_platform_services(Arc::new(Replay(input)));
    let (a_id, b_id) = (calling.id.clone(), taking.id.clone());
    let (a_launch, mut a_linked) = mpsc::channel(1);
    let (b_launch, mut b_linked) = mpsc::channel(1);
    let a = a.with_launcher(a_id.clone(), Arc::new(FixtureLauncher(a_launch)));
    let b = b.with_launcher(b_id.clone(), Arc::new(FixtureLauncher(b_launch)));
    let (a_task, b_task) = (tokio::spawn(a.run()), tokio::spawn(b.run()));
    let mut sender = within(Duration::from_secs(4), a_linked.recv())
        .await
        .unwrap()
        .unwrap();
    let mut sink = within(Duration::from_secs(4), b_linked.recv())
        .await
        .unwrap()
        .unwrap();
    let Reply::Invitation(invitation) = b_client
        .command(Command::CreateInvitation {})
        .await
        .unwrap()
    else {
        panic!("pairing invitation unavailable")
    };
    a_client
        .command(Command::JoinInvitation { invitation })
        .await
        .unwrap();
    let (a_prompt, b_prompt) = tokio::join!(
        tell(&a_client, |s| !s.pairing.is_empty()),
        tell(&b_client, |s| !s.pairing.is_empty())
    );
    let number = a_prompt.pairing[0].options[0];
    for (client, state) in [(&a_client, a_prompt), (&b_client, b_prompt)] {
        client
            .command(Command::ChoosePairing {
                request: state.pairing[0].id.clone(),
                number,
            })
            .await
            .unwrap();
    }
    let (a_seen, b_seen) = tokio::join!(
        tell(&a_client, |s| s.peers.iter().any(|p| p.online)),
        tell(&b_client, |s| s.peers.iter().any(|p| p.online))
    );
    let (b_peer, a_peer) = (&a_seen.peers[0].id, &b_seen.peers[0].id);
    let listen = permission(HOST, PEERS_WATCH_CAPABILITY);
    let send = permission(HOST, PEERS_SEND_CAPABILITY);
    let write = permission("sink", "sink.write");
    let replay = permission(HOST, INPUT_CAPABILITY);
    let sending_grants = [listen.clone(), send.clone(), use_capture.clone()];
    grants(&a_client, &a_id, Some(b_peer), &sending_grants).await;
    grants(&b_client, &b_id, None, std::slice::from_ref(&replay)).await;

    let mut serial = 0;
    let alias = within(Duration::from_secs(8), async {
        loop {
            serial += 1;
            let Outcome::Ok { value } =
                ask(&mut sender, serial, PEERS_SERVICE, "list", json!({})).await
            else {
                panic!("granted list refused")
            };
            if value[0]["accepts"].is_array() {
                break value[0]["device"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let send_to = |service: &str| json!({"device":alias, "service":service, "version":1});
    for service in ["missing.service", "sink.service"] {
        serial += 1;
        let Outcome::Ok { value } =
            ask(&mut sender, serial, PEERS_SERVICE, "send", send_to(service)).await
        else {
            panic!("a first attempt should reach the paired receiver")
        };
        let mut extra = Vec::new();
        if service == "missing.service" {
            for _ in 1..crate::parked::MAX_PARKED {
                serial += 1;
                let Outcome::Ok { value } =
                    ask(&mut sender, serial, PEERS_SERVICE, "send", send_to(service)).await
                else {
                    panic!("claim slots are occupied before the first refusal");
                };
                extra.push(value["claim"].as_u64().unwrap());
            }
        }
        let mut stream = sender.data(0, value["claim"].as_u64()).await;
        let _ = stream.write_all(b"synthetic opening").await;
        let _ = stream.shutdown().await;
        let ack = within(
            Duration::from_secs(8),
            peersus_plugin::frame::read::<Outcome>(&mut stream, 1024),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(ack, Outcome::NotAccepted {});
        serial += 1;
        assert_eq!(
            ask(&mut sender, serial, PEERS_SERVICE, "send", send_to(service)).await,
            ack,
            "a second attempt has neither a claim nor a stream to open"
        );
        if !extra.is_empty() {
            // One slot was collected. The other original claims still have a reply
            // reserved, even after another withdrawal with those replies pending.
            serial += 1;
            let Outcome::Ok { value } = ask(
                &mut sender,
                serial,
                PEERS_SERVICE,
                "send",
                send_to("unused.service"),
            )
            .await
            else {
                panic!("the collected claim freed one slot")
            };
            extra.push(value["claim"].as_u64().unwrap());
            grants(
                &a_client,
                &a_id,
                Some(b_peer),
                &[listen.clone(), use_capture.clone()],
            )
            .await;
            grants(&a_client, &a_id, Some(b_peer), &sending_grants).await;
            serial += 1;
            assert_eq!(
                ask(
                    &mut sender,
                    serial,
                    PEERS_SERVICE,
                    "send",
                    send_to("unused.service")
                )
                .await,
                Outcome::error("refused", "Too many extension transfers are waiting"),
                "a full claim pool must return a visible local error, not a remote NO"
            );
            for late in extra {
                let mut retired = sender.data(0, Some(late)).await;
                assert_eq!(
                    within(
                        Duration::from_secs(2),
                        frame::read::<Outcome>(&mut retired, 1024)
                    )
                    .await
                    .unwrap()
                    .unwrap(),
                    ack,
                );
            }
        }
    }
    let refused = tell(&b_client, |state| state.notifications.len() == 2).await;
    assert!(refused.notifications.iter().any(|notice| matches!(
        notice.source,
        peersus_core::model::NotificationSource::UnavailableRecipient
    )));
    assert!(refused.notifications.iter().any(|notice| matches!(
        &notice.source, peersus_core::model::NotificationSource::MissingPermission(cause)
            if cause.permission == write)));
    assert!(a_client.snapshot().notifications.is_empty());
    assert!(a_client.snapshot().errors.is_empty() && b_client.snapshot().errors.is_empty());

    sender
        .submit(Operation::Subscribe {
            provider: HOST.into(),
            service: CONFIG_SERVICE.into(),
            version: 1,
            event: CONFIG_CHANGED.into(),
        })
        .await;
    let mut stopped = a_client.surface_failures();
    for (index, taking_back) in [
        "receiver",
        "sender",
        "parked",
        "standalone",
        "completed",
        "sender-between-batches",
        "connection-loss",
    ]
    .into_iter()
    .enumerate()
    {
        let visit = u64::try_from(index).unwrap() + 1;
        grants(&b_client, &b_id, Some(a_peer), std::slice::from_ref(&write)).await;
        grants(&b_client, &b_id, None, std::slice::from_ref(&replay)).await;
        within(Duration::from_secs(5), async {
            loop {
                serial += 1;
                let Outcome::Ok { value } =
                    ask(&mut sender, serial, PEERS_SERVICE, "list", json!({})).await
                else {
                    panic!("granted list refused");
                };
                if value[0]["accepts"] == json!(["sink.service"]) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the receiver's own admission became visible");
        if index == 0 {
            serial += 1;
            assert_eq!(
                ask(
                    &mut sender,
                    serial,
                    PEERS_SERVICE,
                    "send",
                    send_to("missing.service")
                )
                .await,
                Outcome::NotAccepted {},
                "a different usable route must not clear the recorded refusal"
            );
        }
        grants(&a_client, &a_id, Some(b_peer), &sending_grants).await;
        let motion = || Command::ForwardInput {
            plugin: a_id.clone(),
            peer: b_peer.clone(),
            end: false,
            events: vec![json!({"t":"position","x":0.5,"y":0.5})],
        };
        let capture = CaptureInput {
            serial: visit,
            resting: false,
        };
        a_client
            .request(motion(), Some(capture), false)
            .await
            .unwrap();
        let captured = loop {
            if let Delivery::Call { ticket, args, .. } = message(&mut sender).await {
                assert_eq!(args["begin"], true);
                break ticket;
            }
        };
        a_client
            .request(motion(), Some(capture), false)
            .await
            .unwrap();
        serial += 1;
        let Outcome::Ok { value } = ask(
            &mut sender,
            serial,
            PEERS_SERVICE,
            "send",
            send_to("sink.service"),
        )
        .await
        else {
            panic!("a new user action after a grant can open a fresh stream")
        };
        let mut sending = sender.data(0, value["claim"].as_u64()).await;
        let Delivery::Call { ticket, args, .. } = message(&mut sink).await else {
            panic!("the peer's receive was not called")
        };
        if taking_back == "parked" {
            // Never claim the receive. The call must cancel before the claim deadline.
            grants(
                &a_client,
                &a_id,
                Some(b_peer),
                &[listen.clone(), send.clone()],
            )
            .await;
            assert!(matches!(within(Duration::from_secs(2),
                frame::read::<Delivery>(&mut sink.control, frame::CONTROL_LIMIT)).await,
                Some(Ok(Delivery::Cancelled { ticket: got })) if ticket == got));
            assert_eq!(
                frame::read::<Outcome>(&mut sending, 1024).await.unwrap(),
                Outcome::NotAccepted {}
            );
            let stop = within(Duration::from_secs(2), stopped.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stop.visit, Some(visit));
            loop {
                if let Delivery::Call { ticket, args, .. } = message(&mut sender).await {
                    assert_eq!(args["stop"]["status"], "missing-permission");
                    sender
                        .submit(Operation::Reply {
                            ticket,
                            result: Outcome::ok(json!({})),
                        })
                        .await;
                    break;
                }
            }
            continue;
        }
        let mut receiving = sink.data(ticket, None).await;
        sending.write_all(b"prefix").await.unwrap();
        let mut prefix = [0; 6];
        within(Duration::from_secs(4), receiving.read_exact(&mut prefix))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&prefix, b"prefix");
        let press =
            json!({"owner":args["peer"], "events":[{"t":"key_code","code":30,"down":true}]});
        assert!(matches!(
            ask(&mut sink, visit, INPUT_SERVICE, "inject", press).await,
            Outcome::Ok { .. }
        ));
        assert_eq!(*keys.0.lock().unwrap(), [(1, 30, 1)]);
        if taking_back == "connection-loss" {
            sender
                .submit(Operation::Reply {
                    ticket: captured,
                    result: Outcome::ok(json!({})),
                })
                .await;
            sending.shutdown().await.unwrap();
            assert_eq!(
                within(Duration::from_secs(4), receiving.read(&mut prefix))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            assert_eq!(
                peersus_plugin::frame::read::<u64>(&mut sending, 1024)
                    .await
                    .unwrap(),
                6
            );
            sink.submit(Operation::Reply {
                ticket,
                result: Outcome::ok(json!({})),
            })
            .await;
            continue;
        }
        if matches!(taking_back, "completed" | "sender-between-batches") {
            sending.shutdown().await.unwrap();
            assert_eq!(
                within(Duration::from_secs(4), receiving.read(&mut prefix))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            assert_eq!(frame::read::<u64>(&mut sending, 1024).await.unwrap(), 6);
        }
        if taking_back == "sender-between-batches" {
            sink.submit(Operation::Reply {
                ticket,
                result: Outcome::ok(json!({})),
            })
            .await;
            assert!(matches!(
                ask(&mut sink, 101, INPUT_SERVICE, "inspect", json!({})).await,
                Outcome::Ok { .. }
            ));
            assert_eq!(
                *keys.0.lock().unwrap(),
                [(1, 30, 1)],
                "the clean batch left the key held"
            );
        }
        match taking_back {
            "receiver" | "completed" => grants(&b_client, &b_id, Some(a_peer), &[]).await,
            "standalone" => grants(&b_client, &b_id, None, &[]).await,
            "sender-between-batches" => {
                grants(
                    &a_client,
                    &a_id,
                    Some(b_peer),
                    &[listen.clone(), use_capture.clone()],
                )
                .await
            }
            _ => {
                grants(
                    &a_client,
                    &a_id,
                    Some(b_peer),
                    &[listen.clone(), send.clone()],
                )
                .await
            }
        }
        let stop = within(Duration::from_secs(2), stopped.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(stop.blocked);
        assert_eq!(stop.visit, Some(visit));
        assert_eq!(stop.key.device.as_ref(), Some(b_peer));
        let mut cancelled = false;
        loop {
            match message(&mut sender).await {
                Delivery::Cancelled { ticket } if ticket == captured => cancelled = true,
                Delivery::Call { ticket, args, .. } => {
                    assert_eq!(args["end"], true);
                    assert_eq!(args["events"], json!([]));
                    assert_eq!(
                        args["stop"]["status"],
                        if matches!(taking_back, "sender" | "sender-between-batches") {
                            "missing-permission"
                        } else {
                            "not-accepted"
                        }
                    );
                    sender
                        .submit(Operation::Reply {
                            ticket,
                            result: Outcome::ok(json!({})),
                        })
                        .await;
                    break;
                }
                _ => {}
            }
        }
        assert!(cancelled);
        a_client
            .request(motion(), Some(capture), false)
            .await
            .unwrap();
        while let Some(Ok(delivery)) = within(
            Duration::from_millis(30),
            frame::read::<Delivery>(&mut sender.control, frame::CONTROL_LIMIT),
        )
        .await
        {
            assert!(
                !matches!(delivery, Delivery::Call { .. }),
                "stale input must be discarded"
            );
        }
        if !matches!(taking_back, "completed" | "sender-between-batches") {
            assert_eq!(
                within(
                    Duration::from_secs(2),
                    peersus_plugin::frame::read::<Outcome>(&mut sending, 1024)
                )
                .await
                .unwrap()
                .unwrap(),
                Outcome::NotAccepted {}
            );
            assert_eq!(
                within(Duration::from_secs(2), receiving.read(&mut prefix))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
        if taking_back != "sender-between-batches" {
            assert!(
                matches!(message(&mut sink).await, Delivery::Cancelled { ticket: got } if got == ticket)
            );
        }
        if matches!(taking_back, "completed" | "sender-between-batches") {
            assert_eq!(
                ask(
                    &mut sink,
                    100,
                    INPUT_SERVICE,
                    "inject",
                    json!({
                        "owner":args["peer"], "events":[{"t":"key_code","code":31,"down":true}],
                    })
                )
                .await,
                Outcome::NotAccepted {}
            );
        }
        keys.wait_for(&[(1, 30, 1), (1, 30, 0)]).await;
        keys.0.lock().unwrap().clear();
    }
    let mut held_no = None;
    let mut refused_new = false;
    for index in 0..=MOST_REMOTE_REFUSALS_PER_EXTENSION {
        let service = format!("additional.{index}");
        serial += 1;
        match ask(
            &mut sender,
            serial,
            PEERS_SERVICE,
            "send",
            send_to(&service),
        )
        .await
        {
            Outcome::Ok { value } => {
                let mut data = sender.data(0, value["claim"].as_u64()).await;
                let _ = data.shutdown().await;
                assert_eq!(
                    within(
                        Duration::from_secs(2),
                        frame::read::<Outcome>(&mut data, 1024)
                    )
                    .await
                    .unwrap()
                    .unwrap(),
                    Outcome::NotAccepted {}
                );
                held_no.get_or_insert(service);
            }
            Outcome::Error { code, message } => {
                assert_eq!(code, "refused");
                assert_eq!(
                    message,
                    "Too many extension destinations are already guarded"
                );
                refused_new = true;
                break;
            }
            other => panic!("expected a new destination or the capacity refusal, got {other:?}"),
        }
    }
    assert!(
        refused_new,
        "refuse new destinations rather than evicting current decisions"
    );
    serial += 1;
    assert_eq!(
        ask(
            &mut sender,
            serial,
            PEERS_SERVICE,
            "send",
            send_to(&held_no.unwrap())
        )
        .await,
        Outcome::NotAccepted {}
    );
    assert!(a_client.snapshot().notifications.is_empty());
    // No grant change, more input, guest release or new stream: dropping the
    // sending endpoint itself must make the controlled owner release the hold.
    assert_eq!(*keys.0.lock().unwrap(), [(1, 30, 1)]);
    a_endpoint.close().await;
    keys.wait_for(&[(1, 30, 1), (1, 30, 0)]).await;
    tell(&a_client, |state| {
        state
            .errors
            .iter()
            .any(|error| error.ends_with("The other device did not accept the input visit"))
    })
    .await;
    assert!(
        b_client.snapshot().errors.is_empty(),
        "the remote NO does not disclose why"
    );
    for (client, task) in [(a_client, a_task), (b_client, b_task)] {
        client.command(Command::Shutdown {}).await.unwrap();
        task.await.unwrap();
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
#[tokio::test]
async fn a_live_visit_rotates_without_an_ack_and_fences_an_old_release() {
    use crate::{
        desktop_input::{DesktopInput, Device, Raw},
        platform_services::{Call, INPUT_SERVICE, InputLease, PlatformServices, Response},
    };
    use peersus_api::message::Outcome;
    use serde_json::json;

    struct Keys(Arc<std::sync::Mutex<Vec<Raw>>>);
    impl Device for Keys {
        fn emit(&mut self, events: &[Raw]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(events);
            Ok(())
        }
    }
    struct Replay(DesktopInput);
    impl PlatformServices for Replay {
        fn dispatch(&self, call: Call) -> Response {
            self.0.dispatch(call)
        }
        fn release_peer_input(&self, caller: &PluginId, alias: &str) {
            self.0.release_peer(caller, alias);
        }
        fn revoke(&self, caller: &PluginId) {
            self.0.revoke(caller);
        }
    }
    let keys = Arc::new(std::sync::Mutex::new(Vec::new()));
    let raw = keys.clone();
    let input = DesktopInput::new(Box::new(move || Ok(Box::new(Keys(raw.clone())))));
    let root = tempfile::tempdir().unwrap();
    let (runtime, _) = loopback_runtime(root.path(), Registry::default()).await;
    let mut runtime = runtime.with_platform_services(Arc::new(Replay(input.clone())));
    let owner = crate::parked::Owner {
        plugin: PluginId::new("sink").unwrap(),
        peer: PeerId("fixture".into()),
    };
    let alias = runtime.aliases.alias(&owner.plugin, &owner.peer);
    let parent = InputLease::new();
    let generation = Some(VisitKey {
        connection: 5,
        stream: 9,
    });
    let call = |method: &str, args| Call {
        caller: owner.plugin.clone(),
        service: INPUT_SERVICE.into(),
        method: method.into(),
        args,
    };
    let copy = |call| admission::ReplayController {
        call,
        visit: generation,
        guard: Some(permission("sink", "sink.write")),
        lease: parent.for_copy(),
    };
    let old = runtime.keep_replay(owner.clone(), copy(1)).unwrap();
    assert!(matches!(
        input
            .dispatch_leased(
                call(
                    "inject",
                    json!({
                        "owner": alias, "events": [{"t":"key_code","code":30,"down":true}],
                    })
                ),
                Some(old.clone())
            )
            .await,
        Outcome::Ok { .. }
    ));
    let queued = input.dispatch_leased(call("release", json!({"owner":alias})), Some(old));
    let next = runtime.keep_replay(owner.clone(), copy(2)).unwrap();
    // A stale copy claimed late never takes the controller back from the newer one.
    assert!(runtime.keep_replay(owner.clone(), copy(1)).is_none());
    assert_eq!(queued.await, Outcome::NotAccepted {});
    assert_eq!(*keys.lock().unwrap(), [(1, 30, 1)]);
    parent.revoke();
    assert_eq!(
        input
            .dispatch_leased(call("release", json!({"owner":alias})), Some(next))
            .await,
        Outcome::NotAccepted {}
    );
    // Owner cleanup does not use the expired guest lease.
    runtime.release_replays(|candidate, replay| candidate == &owner && replay.visit == generation);
    assert_eq!(*keys.lock().unwrap(), [(1, 30, 1), (1, 30, 0)]);

    // A handoff keeps what is held only while the same visit continues under the same guard;
    // data that has no visit at all never keeps anything for the next copy.
    let sink_write = permission("sink", "sink.write");
    for (what, earlier, visit, guard) in [
        (
            "another visit",
            generation,
            Some(VisitKey {
                connection: 5,
                stream: 11,
            }),
            Some(sink_write.clone()),
        ),
        (
            "another guard",
            generation,
            generation,
            Some(permission("sink", "sink.other")),
        ),
        ("no visit", None, None, Some(sink_write.clone())),
    ] {
        keys.lock().unwrap().clear();
        let gate = InputLease::new();
        let first = runtime
            .keep_replay(
                owner.clone(),
                admission::ReplayController {
                    call: 10,
                    visit: earlier,
                    guard: Some(sink_write.clone()),
                    lease: gate.for_copy(),
                },
            )
            .unwrap();
        assert!(
            matches!(
                input
                    .dispatch_leased(
                        call(
                            "inject",
                            json!({
                                "owner": alias,
                                "events": [{"t":"key_code","code":30,"down":true}],
                            })
                        ),
                        Some(first)
                    )
                    .await,
                Outcome::Ok { .. }
            ),
            "{what}"
        );
        assert_eq!(*keys.lock().unwrap(), [(1, 30, 1)], "{what}");
        runtime
            .keep_replay(
                owner.clone(),
                admission::ReplayController {
                    call: 11,
                    visit,
                    guard,
                    lease: gate.for_copy(),
                },
            )
            .unwrap();
        assert_eq!(
            *keys.lock().unwrap(),
            [(1, 30, 1), (1, 30, 0)],
            "{what} lets go of what the earlier copy held"
        );
        runtime.release_replays(|candidate, _| candidate == &owner);
    }
}

#[tokio::test]
async fn a_platform_call_is_answered_only_by_a_copy_claimed_before_it_was_admitted() {
    use crate::platform_services::InputLease;
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), Registry::default()).await;
    let owner = crate::parked::Owner {
        plugin: PluginId::new("sink").unwrap(),
        peer: PeerId("fixture".into()),
    };
    let (old, new) = (InputLease::new(), InputLease::new());
    let copy = |lease: &InputLease, claimed| admission::Receiving {
        owner: owner.clone(),
        guard: None,
        replays_input: true,
        visit: None,
        replay: Some((claimed, lease.clone())),
        data_commit: None,
        stop: None,
        parked_stop: None,
    };
    runtime.receiving.insert(1, copy(&old, 10));
    runtime.receiving.insert(2, copy(&new, 20));
    let answers = |runtime: &Runtime, ticket| {
        runtime
            .replay_source(&owner.plugin, &owner.peer, ticket)
            .map(|(call, _)| call)
    };
    assert_eq!(
        answers(&runtime, 30),
        Some(1),
        "the oldest live copy answers"
    );
    old.revoke();
    assert_eq!(answers(&runtime, 30), Some(2));
    // Work queued by the visit that ended was admitted before the newer copy's data was
    // claimed: it is refused, never applied under the newer gate.
    assert_eq!(answers(&runtime, 15), None);
    assert_eq!(answers(&runtime, 20), None);
    assert_eq!(answers(&runtime, 21), Some(2));
}

#[tokio::test]
async fn a_withdrawn_claim_and_its_unwritten_neutral_reply_each_hold_a_place() {
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), Registry::default()).await;
    let owner = crate::parked::Owner {
        plugin: PluginId::new("sink").unwrap(),
        peer: PeerId("fixture".into()),
    };
    assert!(runtime.has_claim_room());
    assert!(
        runtime
            .outbound
            .park_fresh(
                owner,
                Outbound::Declined,
                std::time::Instant::now(),
                crate::parked::MAX_WAIT,
                || 7
            )
            .is_ok()
    );
    for _ in 1..crate::parked::MAX_PARKED {
        runtime.claim_replies.spawn(std::future::pending::<()>());
    }
    assert!(
        !runtime.has_claim_room(),
        "the withdrawn claim and the replies still being written fill every place"
    );
    runtime.claim_replies.abort_all();
    while runtime.claim_replies.join_next().await.is_some() {}
    assert!(
        runtime.has_claim_room(),
        "a place comes back when its reply ends"
    );
}

#[tokio::test]
async fn a_held_visit_with_a_mismatching_route_returns_a_local_error() {
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), Registry::default()).await;
    let (a, b) = tokio::join!(loopback_endpoint(), loopback_endpoint());
    let (a_to_b, b_to_a) = tokio::join!(a.connect(b.addr(), peersus_net::transport::ALPN), async {
        b.accept().await.unwrap().await
    });
    let connection = a_to_b.unwrap();
    let _remote = b_to_a.unwrap();
    let mut route = RemoteDestination {
        owner: crate::parked::Owner {
            plugin: PluginId::new("carrier").unwrap(),
            peer: PeerId("fixture".into()),
        },
        service: "sink.service".into(),
        version: 1,
    };
    assert!(
        runtime
            .hold_outbound_visit(&connection, &route, None)
            .is_ok()
    );
    route.version = 2;
    assert!(matches!(
        runtime.hold_outbound_visit(&connection, &route, None),
        Err(SendRefusal::Refused(
            "The input visit no longer matches its destination"
        ))
    ));
}

#[tokio::test]
async fn a_carrier_cannot_release_or_inject_without_a_live_controller_and_the_adapter_is_never_asked()
 {
    use crate::platform_services::{
        Call, INPUT_CAPABILITY, INPUT_SERVICE, PlatformServices, Response,
    };
    use peersus_api::{
        contributions::{EdgePressure, Input},
        manifest::Import,
        message::Outcome,
    };
    #[derive(Default)]
    struct Asked(std::sync::Mutex<Vec<String>>);
    impl PlatformServices for Asked {
        fn dispatch(&self, call: Call) -> Response {
            self.0.lock().unwrap().push(call.method);
            Box::pin(async { Outcome::ok(serde_json::Value::Null) })
        }
        fn revoke(&self, _: &PluginId) {}
    }
    let mut carrier = plugin_that_receives(Some("sink.write"));
    carrier.manifest.contributions.input = Some(Input {
        source: Source {
            service: "sink.service".into(),
            method: "forward".into(),
        },
        capability: "sink.write".into(),
        edge_pressure: EdgePressure::Off {},
    });
    carrier.manifest.exports[0]
        .methods
        .insert("forward".into(), member(Some("sink.write")));
    carrier.manifest.imports.push(Import {
        provider: HOST.into(),
        service: INPUT_SERVICE.into(),
        version: 1,
        methods: BTreeSet::from(["inject".into(), "release".into()]),
        events: BTreeSet::new(),
    });
    let mut registry = Registry::default();
    registry.register(carrier.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (runtime, _) = loopback_runtime(root.path(), registry).await;
    let adapter = Arc::new(Asked::default());
    let mut runtime = runtime.with_platform_services(adapter.clone());
    let input = permission(HOST, INPUT_CAPABILITY);
    runtime
        .standalone
        .set(carrier.id.clone(), Grants::new([input.clone()]).unwrap())
        .unwrap();
    let generation = runtime
        .hub
        .register(carrier.id.as_str(), carrier.manifest, [input].into())
        .unwrap();
    let guest = runtime
        .hub
        .connect(carrier.id.as_str(), generation)
        .unwrap();
    for (id, method) in [(1, "release"), (2, "inject")] {
        guest
            .submit(Operation::Call {
                id,
                provider: HOST.to_owned(),
                service: INPUT_SERVICE.to_owned(),
                version: 1,
                method: method.to_owned(),
                args: serde_json::json!({}),
                timeout_ms: 5_000,
            })
            .unwrap();
        let call = runtime.host.try_recv().unwrap().unwrap();
        runtime.handle_host_delivery(call).await;
        assert!(
            matches!(
                guest.try_recv().unwrap(),
                Some(Delivery::Result { id: got, result: Outcome::NotAccepted {} }) if got == id
            ),
            "{method} is answered without a live controller"
        );
    }
    assert!(adapter.0.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_matching_receipt_does_not_succeed_without_forward_transport_confirmation() {
    use crate::receive::Delivery;
    let copied = Delivery::Done(6);
    assert!(matches!(
        confirm_delivery(copied, 6, std::future::pending()).await,
        Delivery::Failed(_)
    ));
    assert!(matches!(
        confirm_delivery(Delivery::Done(6), 6, async { false }).await,
        Delivery::Failed(_)
    ));
    assert_eq!(
        confirm_delivery(Delivery::Done(6), 6, async { true }).await,
        Delivery::Done(6)
    );
}

#[tokio::test]
async fn a_guest_receive_result_cannot_mint_an_owner_permission_warning() {
    use peersus_api::message::Outcome;
    let original = plugin_importing("sink", HOST, LANGUAGE_SERVICE, 1, &["get"]);
    let mut registry = Registry::default();
    registry.register(original.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(root.path(), registry).await;
    let asked = permission(HOST, LANGUAGE_READ_CAPABILITY);
    assert!(grantable_without_a_device(&original).contains(&asked));
    let entry = admission::Receiving {
        owner: crate::parked::Owner {
            plugin: original.id,
            peer: PeerId("fixture".into()),
        },
        guard: None,
        replays_input: false,
        visit: None,
        replay: None,
        data_commit: None,
        stop: None,
        parked_stop: None,
    };
    runtime.finish_receive(1, entry, &Outcome::MissingPermission { permission: asked });
    runtime.publish();
    assert!(
        client.snapshot().notifications.is_empty(),
        "there was no owner-observed refused call"
    );
}

#[tokio::test]
async fn readiness_changes_do_not_wake_an_unrelated_subscriber() {
    let subscriber = |name: &str| {
        let mut spec = plugin_importing(name, HOST, LANGUAGE_SERVICE, 1, &["get"]);
        spec.manifest.imports.push(peersus_api::manifest::Import {
            provider: HOST.into(),
            service: CONFIG_SERVICE.into(),
            version: 1,
            methods: ["get".into()].into(),
            events: [CONFIG_CHANGED.into()].into(),
        });
        spec
    };
    let a = subscriber("requester");
    let b = subscriber("other");
    let mut registry = Registry::default();
    for spec in [&a, &b] {
        registry.register(spec.clone()).unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let (mut runtime, _) = loopback_runtime(root.path(), registry).await;
    let attach = |spec: &PluginSpec| {
        let id = spec.id.as_str();
        let generation = runtime
            .hub
            .register(id, spec.manifest.clone(), BTreeSet::new())
            .unwrap();
        let channel = runtime.hub.connect(id, generation).unwrap();
        channel
            .submit(Operation::Subscribe {
                provider: HOST.into(),
                service: CONFIG_SERVICE.into(),
                version: 1,
                event: CONFIG_CHANGED.into(),
            })
            .unwrap();
        channel
    };
    let (a_guest, b_guest) = (attach(&a), attach(&b));
    runtime
        .execute(Command::Allow {
            plugin: a.id,
            peer: None,
            permission: permission(HOST, LANGUAGE_READ_CAPABILITY),
            on: true,
        })
        .await
        .unwrap();
    runtime.publish();
    assert!(
        matches!(a_guest.try_recv().unwrap(), Some(Delivery::Event { data, .. }) if data == serde_json::json!({}))
    );
    assert!(
        b_guest.try_recv().unwrap().is_none(),
        "another grant cannot signal its timing"
    );
}

#[tokio::test]
async fn remote_stop_releases_after_the_control_reply_but_not_a_replacement_controller() {
    use crate::platform_services::{Call, InputLease, PlatformServices, Response};
    use peersus_api::message::Outcome;
    #[derive(Default)]
    struct Recording(std::sync::Mutex<Vec<String>>);
    impl PlatformServices for Recording {
        fn dispatch(&self, _: Call) -> Response {
            Box::pin(async { Outcome::ok(serde_json::Value::Null) })
        }
        fn release_peer_input(&self, _: &PluginId, alias: &str) {
            self.0.lock().unwrap().push(alias.to_owned());
        }
        fn revoke(&self, _: &PluginId) {}
    }
    let root = tempfile::tempdir().unwrap();
    let (runtime, _) = loopback_runtime(root.path(), receiving_registry()).await;
    let input = Arc::new(Recording::default());
    let mut runtime = runtime.with_platform_services(input.clone());
    let owner = crate::parked::Owner {
        peer: PeerId("fixture".into()),
        plugin: PluginId::new("sink").unwrap(),
    };
    let make = || admission::ReplayController {
        call: 1,
        visit: Some(VisitKey {
            connection: 1,
            stream: 1,
        }),
        guard: Some(permission("sink", "sink.write")),
        lease: InputLease::new(),
    };
    let mut old = make();
    runtime.replay_controllers.insert(owner.clone(), make());
    runtime.receiving.insert(
        old.call,
        admission::Receiving {
            owner: owner.clone(),
            guard: old.guard.take(),
            replays_input: true,
            visit: old.visit,
            replay: Some((1, old.lease)),
            data_commit: None,
            stop: None,
            parked_stop: None,
        },
    );
    runtime
        .handle_host_delivery(Delivery::Result {
            id: 1,
            result: Outcome::ok(serde_json::Value::Null),
        })
        .await;
    assert!(
        input.0.lock().unwrap().is_empty(),
        "a clean reply is not a revoke"
    );
    runtime.finish_revoked_receive(1);
    let alias = runtime.aliases.alias(&owner.plugin, &owner.peer);
    assert_eq!(*input.0.lock().unwrap(), std::slice::from_ref(&alias));
    let mut next = make();
    next.call = 2;
    let current = next.lease.clone();
    runtime.replay_controllers.insert(owner, next);
    runtime.finish_revoked_receive(1);
    assert_eq!(*input.0.lock().unwrap(), [alias]);
    assert_eq!(current.apply(|| true), Some(true));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn development_confirmation_uses_only_registered_predecessor_and_reviewed_archive() {
    use peersus_core::model::SourceDir;
    use serde_json::{Value, json};

    async fn pair(a: &Client, b: &Client) -> (PeerId, PeerId) {
        async fn await_state(client: &Client, ready: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
            let mut state = client.subscribe();
            within(Duration::from_secs(6), state.wait_for(|s| ready(s)))
                .await
                .unwrap()
                .unwrap()
                .clone()
        }
        let Reply::Invitation(invitation) = b.command(Command::CreateInvitation {}).await.unwrap()
        else {
            panic!("expected an invitation")
        };
        a.command(Command::JoinInvitation { invitation })
            .await
            .unwrap();
        let (pa, pb) = tokio::join!(
            await_state(a, |s| !s.pairing.is_empty()),
            await_state(b, |s| !s.pairing.is_empty())
        );
        let number = pa.pairing[0].options[0];
        for (client, state) in [(a, pa), (b, pb)] {
            client
                .command(Command::ChoosePairing {
                    request: state.pairing[0].id.clone(),
                    number,
                })
                .await
                .unwrap();
        }
        let (pa, pb) = tokio::join!(
            await_state(a, |s| s.peers.iter().any(|peer| peer.online)),
            await_state(b, |s| s.peers.iter().any(|peer| peer.online)),
        );
        (pa.peers[0].id.clone(), pb.peers[0].id.clone())
    }

    fn package(parent: &std::path::Path, version: &str, declaration: &str) -> PathBuf {
        let returned = update_package(parent, "grant-test", version, &["list"]);
        let manifest = returned.join("plugin.json");
        let mut value: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        value["execution"] = json!({"kind":"wasm","abi":1});
        value["program"] = json!("payload.wasm");
        value["capabilities"] = json!({"thing.receive": declaration});
        value["exports"] = json!([{
            "service":"thing.service","version":1,"description":"Takes fixture data.","consumers":[HOST],
            "methods":{"receive":{"capability":"thing.receive","description":"Collects fixture data."}},"events":{},
        }]);
        std::fs::write(manifest, value.to_string()).unwrap();
        std::fs::remove_file(returned.join(crate::discovery::PROGRAM_FILE)).unwrap();
        std::fs::write(returned.join("payload.wasm"), b"\x00asm\x01\x00\x00\x00").unwrap();
        returned
    }

    for mode in [
        "approved",
        "refused",
        "declaration-changed",
        "disk-edited",
        "replaced-outside-owner",
        "forgotten",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let peer_dir = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let (mut a, a_client) = loopback_runtime(dir.path(), Registry::default()).await;
        let (b, b_client) = loopback_runtime(peer_dir.path(), Registry::default()).await;
        a = a.with_packages(crate::plugin_manager::Manager::new(dir.path()));
        let first = package(parent.path(), "1.0.0", "Previous meaning.");
        install_as(&mut a, &first, false).await;
        let plugin = PluginId::new("grant-test").unwrap();
        a.disable_plugin(plugin.clone()).await.unwrap();
        let installed = dir.path().join("plugins/grant-test");
        let manifest = installed.join("plugin.json");
        if matches!(mode, "refused" | "disk-edited") {
            let mut value: Value =
                serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
            if mode == "refused" {
                value["older_presentation"] = json!(true);
            } else {
                // The current bytes may parse and the executable still match its
                // approval, but this is not the declaration the owner registered.
                value["capabilities"]["thing.receive"] = json!("Different meaning.");
            }
            std::fs::write(&manifest, value.to_string()).unwrap();
        }
        if mode == "replaced-outside-owner" {
            std::fs::write(
                installed.join(crate::plugin_folder::NEW_INSTALL_MARKER),
                b"",
            )
            .unwrap();
        }
        let (a_task, b_task) = (tokio::spawn(a.run()), tokio::spawn(b.run()));
        let (on_a, on_b) = pair(&a_client, &b_client).await;
        let standalone = permission(HOST, PLUGINS_WATCH_CAPABILITY);
        let receive = permission(plugin.as_str(), "thing.receive");
        for (peer, grant) in [
            (None, standalone.clone()),
            (Some(on_a.clone()), receive.clone()),
        ] {
            a_client
                .command(Command::SetGrants {
                    plugin: plugin.clone(),
                    peer,
                    grants: Grants::new([grant]).unwrap(),
                })
                .await
                .unwrap();
        }
        let new = package(
            parent.path(),
            "2.0.0",
            if matches!(mode, "declaration-changed" | "disk-edited") {
                "Different meaning."
            } else {
                "Previous meaning."
            },
        );
        let reviewed = crate::discovery::read_package(&new).unwrap();
        let agreement = agreement_digest(&reviewed).unwrap();
        if mode == "approved" {
            assert!(matches!(
                a_client
                    .command(Command::StageDevelopmentInstall {
                        source: SourceDir::new(new.clone()).unwrap(),
                        replacement_only: true,
                    })
                    .await,
                Err("A development replacement needs a package archive")
            ));
        }
        let archive = parent.path().join("fixture.tar");
        {
            let mut tar = tar::Builder::new(std::fs::File::create(&archive).unwrap());
            tar.append_dir_all("grant-test", &new).unwrap();
            tar.finish().unwrap();
        }
        a_client
            .command(Command::StageDevelopmentInstall {
                source: SourceDir::new(archive.clone()).unwrap(),
                replacement_only: true,
            })
            .await
            .unwrap();
        // A rebuild of the uploaded source cannot change the reviewed private copy.
        std::fs::write(&archive, b"replaced upload").unwrap();
        if mode == "forgotten" {
            a_client
                .command(Command::ForgetPeer { peer: on_a.clone() })
                .await
                .unwrap();
            b_client
                .command(Command::ForgetPeer { peer: on_b.clone() })
                .await
                .unwrap();
            assert_eq!(pair(&a_client, &b_client).await, (on_a.clone(), on_b));
        }
        let Reply::DevelopmentInstalled {
            predecessor_sound,
            disabled,
            kept_own,
            device_sets,
            dropped,
        } = a_client
            .command(Command::ConfirmInstall {
                plugin: plugin.clone(),
                agreement: Some(agreement),
                risk_acknowledged: true,
            })
            .await
            .unwrap()
        else {
            panic!("expected confirmation report")
        };
        let valid = !matches!(mode, "refused" | "disk-edited" | "replaced-outside-owner");
        assert_eq!(predecessor_sound, valid, "{mode}");
        assert!(disabled, "{mode}");
        let expected_device = valid && !matches!(mode, "forgotten" | "declaration-changed");
        assert_eq!(kept_own, usize::from(valid), "{mode}");
        assert_eq!(device_sets, usize::from(expected_device), "{mode}");
        assert_eq!(
            dropped.contains(&receive),
            mode != "forgotten" && !expected_device,
            "{mode}"
        );
        let state = a_client.snapshot();
        assert_eq!(
            state
                .grants
                .get(&plugin)
                .is_some_and(|grants| grants.allows(&standalone)),
            valid
        );
        let remembered = state.peers.iter().find(|peer| peer.id == on_a).unwrap();
        assert_eq!(
            remembered
                .grants
                .get(&plugin)
                .is_some_and(|grants| grants.allows(&receive)),
            expected_device
        );
        assert_eq!(
            crate::discovery::read_package(&installed).unwrap(),
            reviewed
        );
        for (client, task) in [(a_client, a_task), (b_client, b_task)] {
            client.command(Command::Shutdown {}).await.unwrap();
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn withdrawing_a_declared_action_guard_does_not_cancel_the_other_action() {
    let mut spec = plugin_with_guarded_action();
    spec.manifest.contributions.actions[0].needs_device = true;
    let mut other = spec.manifest.contributions.actions[0].clone();
    other.id = "other".into();
    other.target.method = "other".into();
    other.capability = Some("unrelated".into());
    spec.manifest
        .capabilities
        .insert("unrelated".into(), "Allow the unrelated action.".into());
    spec.manifest.exports[0]
        .methods
        .insert("other".into(), member(Some("unrelated")));
    spec.manifest.contributions.actions.push(other);
    let mut registry = Registry::default();
    registry.register(spec.clone()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let (mut runtime, client) = loopback_runtime(directory.path(), registry).await;
    let device = PeerId("fixture".into());
    for (id, name) in [(1, "go"), (2, "other")] {
        let key = Key {
            plugin: spec.id.clone(),
            action: name.into(),
            device: Some(device.clone()),
        };
        runtime.in_flight.start(key.clone(), id).unwrap();
        runtime.board.running(key);
    }
    runtime.withdraw_grant_work(
        &spec.id,
        Some(&device),
        &permission(spec.id.as_str(), "cap"),
    );
    runtime.publish();
    let snapshot = client.snapshot();
    assert!(
        snapshot
            .actions
            .iter()
            .any(|entry| entry.action == "go" && entry.state == ActionState::Cancelled)
    );
    assert!(
        snapshot
            .actions
            .iter()
            .any(|entry| entry.action == "other" && entry.state == ActionState::Running)
    );
    assert!(!runtime.in_flight.finish(1));
    assert!(runtime.in_flight.finish(2));
}
