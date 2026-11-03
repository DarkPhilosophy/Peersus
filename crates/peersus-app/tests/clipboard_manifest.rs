//! The clipboard plugin's shipped `plugin.json`, put where discovery looks and loaded by the
//! application's own rules.
//!
//! The manifest is a file next to the plugin's code, and nothing at compile time ties the two
//! to what the application will accept. A manifest the application refused would be a plugin
//! that ships and never loads, so this loads the real file the way a user's install would.
#![cfg(unix)]

use peersus_app::{discovery::discover, plugin_host::RECEIVE_METHOD};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

const MANIFEST: &str = include_str!("../../peersus-clipboard/plugin.json");

/// Install `manifest` as plugin `name` under `root`, with a program that is acceptable to
/// discovery, and nothing else different from a real install.
fn install(root: &Path, name: &str, manifest: &str) {
    let directory = root.join(name);
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(directory.join("plugin.json"), manifest).unwrap();
    // The program is the file the manifest names, which is what a real install holds.
    let named = serde_json::from_str::<peersus_plugin_api::manifest::Manifest>(manifest)
        .map(|m| m.program_file().to_owned())
        .unwrap_or_else(|_| "plugin".to_owned());
    let program = directory.join(named);
    fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
}

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    root
}

#[test]
fn the_shipped_manifest_loads_and_is_routed_for_what_a_device_sends() {
    let root = root();
    install(root.path(), "clipboard", MANIFEST);
    let found = discover(root.path());
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    assert_eq!(found.registry.len(), 1);
    // The service another device addresses is the one the plugin answers for, and nothing
    // else routes there.
    let sink = found.registry.route("clipboard.text", 1);
    assert!(
        matches!(sink, peersus_app::plugin_host::Route::Plugin { .. }),
        "a device's text would not reach the plugin"
    );
    assert!(matches!(
        found.registry.route("clipboard.text", 2),
        peersus_app::plugin_host::Route::Unknown
    ));
    assert!(matches!(
        found.registry.route("clipboard.other", 1),
        peersus_app::plugin_host::Route::Unknown
    ));
}

#[test]
fn what_the_manifest_asks_for_is_exactly_what_the_plugin_does() {
    let root = root();
    install(root.path(), "clipboard", MANIFEST);
    let found = discover(root.path());
    let spec = found.registry.iter().next().expect("it did not load");
    let manifest = &spec.manifest;
    // It declares ONE permission of its own, the guard on what it offers. Sending is the
    // application's permission, not the plugin's, so it is not declared here.
    assert_eq!(
        manifest
            .capabilities
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["clipboard.use"]
    );
    // What the user can grant it is derived from the roles it plays: its own guard on what it
    // offers, and the application's guard on the `send` it imports. Nothing else.
    let grantable = peersus_app::runtime::grantable(spec);
    let expected: std::collections::BTreeSet<_> = [
        peersus_plugin_api::permission::Permission::new("clipboard", "clipboard.use").unwrap(),
        peersus_plugin_api::permission::Permission::new("host", "peers.send").unwrap(),
    ]
    .into();
    assert_eq!(grantable, expected);
    // Automatic synchronization reads caller-scoped configuration, while device streams
    // still need the application's per-device send grant. Neither import grants itself access.
    let imports: Vec<_> = manifest
        .imports
        .iter()
        .map(|import| {
            (
                import.provider.as_str(),
                import.service.as_str(),
                import.version,
                import
                    .methods
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        vec![
            ("host", "peersus.peers", 1, vec!["list", "send"]),
            ("host", "peersus.config", 1, vec!["get"]),
        ]
    );
    let automatic = manifest
        .contributions
        .settings
        .iter()
        .find(|setting| setting.id == "automatic")
        .unwrap();
    let sync = manifest
        .contributions
        .settings
        .iter()
        .find(|setting| setting.id == "sync")
        .unwrap();
    assert_eq!(
        automatic.scope,
        peersus_plugin_api::contributions::Scope::Global
    );
    assert_eq!(sync.scope, peersus_plugin_api::contributions::Scope::Device);
    for setting in [automatic, sync] {
        assert_eq!(
            setting.kind,
            peersus_plugin_api::contributions::Kind::Toggle { default: false }
        );
    }
    // It answers `share` and `receive`, both behind the same permission, and only the
    // application may call them.
    assert_eq!(manifest.exports.len(), 1);
    let export = &manifest.exports[0];
    assert_eq!(
        (export.service.as_str(), export.version),
        ("clipboard.text", 1)
    );
    assert_eq!(
        export
            .consumers
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["host"]
    );
    assert_eq!(export.methods.len(), 2);
    assert_eq!(
        export.methods["share"].capability.as_deref(),
        Some("clipboard.use")
    );
    assert_eq!(
        export.methods[RECEIVE_METHOD].capability.as_deref(),
        Some("clipboard.use")
    );
    // What a person reads when deciding: nothing shipped is left unexplained. A description
    // is disclosure, not prevention (a hostile plugin writes a friendly one too), but a
    // permission nobody can read is one nobody can decide about.
    assert!(!export.description.trim().is_empty());
    assert!(
        export
            .methods
            .values()
            .all(|m| !m.description.trim().is_empty())
    );
    assert!(manifest.capabilities.values().all(|d| !d.trim().is_empty()));
    // One thing the user can press, and it needs a device.
    let actions = &manifest.contributions.actions;
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].id, "share");
    assert!(actions[0].needs_device);
    assert!(!actions[0].destructive);
}

#[test]
fn the_same_manifest_without_the_permission_that_guards_receiving_is_refused() {
    // The control that shows the test above is not passing for want of a rule: dropping the
    // permission from `receive` is something the application refuses. There are two ways an
    // author can leave it unguarded, and both must be caught: writing `null`, and simply
    // leaving the line out.
    let guarded = "\"receive\": {\n          \"capability\": \"clipboard.use\",";
    for (how, replacement) in [
        (
            "an explicit null",
            "\"receive\": {\n          \"capability\": null,",
        ),
        ("an omitted capability", "\"receive\": {"),
    ] {
        let unguarded = MANIFEST.replace(guarded, replacement);
        assert_ne!(unguarded, MANIFEST, "{how}: the edit changed nothing");
        // Only `receive` lost its guard: `share` still has it, so the refusal below is about
        // taking things in, not about the plugin having no permission at all.
        assert_eq!(
            unguarded
                .matches("\"capability\": \"clipboard.use\"")
                .count(),
            MANIFEST
                .matches("\"capability\": \"clipboard.use\"")
                .count()
                - 1,
            "{how}: the edit touched the wrong member"
        );
        let root = root();
        install(root.path(), "clipboard", &unguarded);
        let found = discover(root.path());
        assert!(found.registry.is_empty(), "{how}: it was loaded");
        assert_eq!(found.problems.len(), 1, "{how}");
        assert_eq!(
            found.problems[0].reason, "It takes things in without a permission to guard them",
            "{how}: refused for the wrong reason"
        );
    }
}
