//! The application side of the plugin contract: which plugins exist, who may use them,
//! and how a peer's bytes reach one.
//!
//! A plugin is a separate process. This module never runs plugin code and never hands a
//! plugin a network stream; it only knows what each plugin *declared* and decides, from
//! that and from the user's per-device grants, whether a device may use it. It never
//! names a concrete plugin: a plugin is reached by the `(service, version)` its own
//! manifest exports.
//!
//! Two independent questions are answered by two independent layers. This module
//! answers "may this DEVICE use this plugin" ([`permitted`], default-deny). The hub
//! answers "may this PLUGIN call that plugin". A remote device is never a hub
//! participant, so a device can never inherit what the application itself may do.
use peersus_api::{contributions::Action, manifest::Manifest};
use peersus_core::{
    model::{Grantable, Peer, PluginInfo, PluginStatus},
    plugins::PluginId,
};
use peersus_plugin::supervisor::Policy;
use std::path::PathBuf;

/// Convention: the exported method that receives an inbound peer stream.
pub const RECEIVE_METHOD: &str = "receive";

/// Everything needed to know about one plugin, and to start it. Plain data: no code of
/// the plugin is reachable from here.
#[derive(Clone, Debug)]
pub struct PluginSpec {
    pub id: PluginId,
    pub manifest: Manifest,
    /// Desktop executable, or None for a bundled platform-isolated lifecycle.
    pub program: Option<PathBuf>,
    pub arguments: Vec<String>,
    /// How patiently the application restarts this plugin when it fails. `None` uses the
    /// standard policy. A plugin that starts slowly or fails in a way that is expected can
    /// be given more or less patience without changing it for the others.
    pub restart_policy: Option<Policy>,
    /// The package's own translations of the words its manifest shows (`languages/<tag>.po` in its folder), each checked
    /// for being the language it says it is. Empty for a package that brings none.
    pub languages: std::sync::Arc<[peersus_api::language::Catalog]>,
}

/// Where a service is exported, resolved from manifests rather than from code.
#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    Plugin {
        index: usize,
        /// Capability the peer must have been granted, if the export requires one.
        capability: Option<&'a str>,
    },
    Unknown,
}

/// The set of plugins the application knows about, with manifest-driven lookup.
#[derive(Default)]
pub struct Registry {
    plugins: Vec<PluginSpec>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("a plugin with this id is already registered")]
    Duplicate,
    #[error("two plugins export the same service")]
    ServiceConflict,
    #[error("invalid plugin manifest")]
    Manifest,
    #[error("a plugin action is not reachable because its export does not list the application")]
    ActionUnreachable,
    #[error("a plugin that receives from peers must name the capability that guards it")]
    ReceiveWithoutCapability,
    #[error("this plugin does not support the current operating system")]
    UnsupportedPlatform,
}

fn external_execution_supported(
    execution: peersus_api::manifest::Execution,
    android: bool,
    native: bool,
) -> bool {
    use peersus_api::manifest::Execution;
    !android
        || match execution {
            Execution::Process => false,
            Execution::NativeLibrary { .. } => native,
            Execution::Wasm { .. } => true,
        }
}

/// Only external packages are constrained here; bundled Android services retain their
/// existing declaration and explicit platform launcher. Called again before launch.
pub(crate) fn check_external_execution(
    execution: peersus_api::manifest::Execution,
) -> Result<(), &'static str> {
    external_execution_supported(
        execution,
        cfg!(target_os = "android"),
        cfg!(feature = "android-native-extensions"),
    )
    .then_some(())
    .ok_or("This extension format is not available in this distribution")
}

/// What a manifest must meet on its own, whichever other plugins there are: it validates, every action it declares can be
/// reached by the application, and what it takes in from peers is guarded by a capability. This is ONE place, used by
/// [`Registry::register`] and by the state of a plugin's folder, so that what is accepted to run and what is shown as
/// usable cannot disagree. What depends on the other plugins (an id used twice, a service two of them export) is not here.
pub fn check_manifest(manifest: &Manifest) -> Result<(), RegistryError> {
    manifest.validate().map_err(|_| RegistryError::Manifest)?;
    if manifest.platforms.is_some()
        && peersus_api::platform::OperatingSystem::current().is_none_or(|os| !manifest.supports(os))
    {
        return Err(RegistryError::UnsupportedPlatform);
    }
    // A button is only honest if pressing it can work. The hub accepts a call from the
    // application only when the exporter lists it as a consumer, and exporting is the
    // author's choice, so nothing is added on the author's behalf: a plugin that
    // declares an action but keeps its export private is told so here, once, instead
    // of every click failing later with no explanation.
    let reachable = |service: &str| {
        manifest
            .exports
            .iter()
            .any(|e| e.service == service && e.consumers.contains(peersus_plugin::hub::HOST))
    };
    if !manifest
        .contributions
        .actions
        .iter()
        .all(|action| reachable(&action.target.service))
    {
        return Err(RegistryError::ActionUnreachable);
    }
    // What a plugin takes in from peers has to be guarded by a capability the user can
    // grant. Permission is a grant and a grant names a capability, so an unguarded
    // `receive` could never be permitted to anyone: it would look functional and do
    // nothing. Refusing it here says so once, at the start, with a reason.
    if manifest.exports.iter().any(|export| {
        export
            .methods
            .get(RECEIVE_METHOD)
            .is_some_and(|member| member.capability.is_none())
    }) {
        return Err(RegistryError::ReceiveWithoutCapability);
    }
    Ok(())
}

impl Registry {
    /// Register a plugin after validating its manifest. A service may be exported by
    /// exactly one plugin so inbound routing is never ambiguous, and an id may appear
    /// once, which is also what a supervisor needs: two services under one id would
    /// revoke each other forever.
    pub fn register(&mut self, plugin: PluginSpec) -> Result<(), RegistryError> {
        check_manifest(&plugin.manifest)?;
        if self.plugins.iter().any(|p| p.id == plugin.id) {
            return Err(RegistryError::Duplicate);
        }
        let conflict = |a: &Manifest, b: &Manifest| {
            a.exports
                .iter()
                .any(|x| b.exports.iter().any(|y| x.service == y.service))
        };
        if self
            .plugins
            .iter()
            .any(|p| conflict(&p.manifest, &plugin.manifest))
        {
            return Err(RegistryError::ServiceConflict);
        }
        self.plugins.push(plugin);
        Ok(())
    }

    /// Take a plugin out of the registry and give it back, or `None` if it was not there. Its id
    /// and the services it exported are free again, so a different package may be registered
    /// under them.
    pub fn remove(&mut self, id: &PluginId) -> Option<PluginSpec> {
        let index = self.plugins.iter().position(|p| &p.id == id)?;
        Some(self.plugins.remove(index))
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Plugins and manifests for generic frontends, in registration order. What each can be granted is the
    /// owner's rule, given here: the registry knows what plugins declared and nothing of what that allows.
    pub fn info(&self, grantable: impl Fn(&PluginSpec) -> Grantable) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .map(|p| PluginInfo {
                id: p.id.clone(),
                manifest: p.manifest.clone(),
                // The registry only knows what plugins declared. Whether one is running is
                // known to whoever supervises the processes, which fills this in.
                status: PluginStatus::Stopped,
                // What is in the registry was admitted to it, which only a plugin that may run is, so it is ready here. The
                // owner knows which of them the person switched off, and which plugins are not in the registry at all
                // (not approved, changed, broken), and says so when it puts the list together.
                standing: peersus_core::model::Standing::Ready,
                grantable: grantable(p),
                program: None,
                package: None,
                // Whether this person accepted its agreement is the owner's knowledge, filled in where the list is put together.
                agreed: false,
                from_store: false,
            })
            .collect()
    }

    pub fn find(&self, id: &PluginId) -> Option<usize> {
        self.plugins.iter().position(|p| &p.id == id)
    }

    pub fn get(&self, index: usize) -> Option<&PluginSpec> {
        self.plugins.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PluginSpec> {
        self.plugins.iter()
    }

    /// Resolve an inbound `(service, version)` using only the manifests. A version
    /// mismatch is `Unknown`, never a best-effort match.
    pub fn route(&self, service: &str, version: u32) -> Route<'_> {
        for (index, plugin) in self.plugins.iter().enumerate() {
            let Some(export) = plugin
                .manifest
                .exports
                .iter()
                .find(|e| e.service == service && e.version == version)
            else {
                continue;
            };
            let Some(member) = export.methods.get(RECEIVE_METHOD) else {
                continue;
            };
            return Route::Plugin {
                index,
                capability: member.capability.as_deref(),
            };
        }
        Route::Unknown
    }

    /// The action a plugin declared, if any.
    pub fn action(&self, index: usize, action: &str) -> Option<&Action> {
        self.plugins
            .get(index)?
            .manifest
            .contributions
            .actions
            .iter()
            .find(|a| a.id == action)
    }
}

/// Whether `peer` may use `capability` of `plugin`. No entry means no access.
///
/// The capability is one of THE PLUGIN'S OWN guards, so it is looked up as a permission of
/// that plugin: the owner is the plugin asked about and is never taken from the caller, which
/// means there is no way to ask about, or satisfy, someone else's permission of that name.
pub fn permitted(peer: &Peer, plugin: &PluginId, capability: Option<&str>) -> bool {
    match capability {
        None => peer.grants.contains_key(plugin),
        Some(capability) => {
            let Some(permission) =
                peersus_api::permission::Permission::new(plugin.as_str(), capability)
            else {
                return false;
            };
            peer.grants
                .get(plugin)
                .is_some_and(|grants| grants.allows(&permission))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::manifest::{Export, Member};
    use peersus_core::{model::PeerId, plugins::Grants};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn external_execution_respects_distribution_without_changing_the_shared_abi() {
        use peersus_api::manifest::{EXECUTION_ABI_VERSION, Execution};
        let native = Execution::NativeLibrary {
            abi: EXECUTION_ABI_VERSION,
        };
        let wasm = Execution::Wasm {
            abi: EXECUTION_ABI_VERSION,
        };
        for community in [false, true] {
            assert!(!external_execution_supported(
                Execution::Process,
                true,
                community
            ));
            assert_eq!(
                external_execution_supported(native, true, community),
                community
            );
            assert!(external_execution_supported(wasm, true, community));
        }
        for execution in [Execution::Process, native, wasm] {
            assert!(external_execution_supported(execution, false, false));
        }
    }

    /// A member that needs `capability` (or nothing) and says what it does.
    fn member(capability: Option<&str>) -> Member {
        Member {
            capability: capability.map(str::to_owned),
            description: "Does what its name says.".to_owned(),
        }
    }

    fn exporting(id: &str, service: &str, version: u32, capability: Option<&str>) -> PluginSpec {
        PluginSpec {
            id: PluginId::new(id).unwrap(),
            manifest: Manifest {
                capabilities: capability
                    .map(|c| (c.to_owned(), format!("Lets the plugin use {c}.")))
                    .into_iter()
                    .collect(),
                exports: vec![Export {
                    service: service.into(),
                    version,
                    description: "A test service.".into(),
                    consumers: BTreeSet::new(),
                    methods: BTreeMap::from([(RECEIVE_METHOD.to_owned(), member(capability))]),
                    events: BTreeMap::new(),
                }],
                ..Manifest::new(peersus_api::manifest::Execution::Process)
            },
            program: Some(PathBuf::from("/nonexistent/plugin")),
            arguments: Vec::new(),
            restart_policy: None,
            languages: Default::default(),
        }
    }

    #[test]
    fn removing_a_plugin_frees_its_name_and_its_service_and_leaves_the_others() {
        let mut registry = Registry::default();
        registry
            .register(exporting("first", "first.service", 1, Some("first.use")))
            .unwrap();
        registry
            .register(exporting("second", "second.service", 1, Some("second.use")))
            .unwrap();
        let first = PluginId::new("first").unwrap();

        assert!(registry.remove(&first).is_some(), "it was there to remove");
        assert!(registry.find(&first).is_none());
        assert_eq!(registry.len(), 1);
        assert!(registry.find(&PluginId::new("second").unwrap()).is_some());
        // Removing it again finds nothing, and is not an error.
        assert!(registry.remove(&first).is_none());
        // Its name and its service are free: a different package may now be registered under them.
        registry
            .register(exporting("first", "first.service", 1, Some("first.use")))
            .unwrap();
        assert_eq!(registry.len(), 2);
    }
    fn peer_with(grants: &[(&str, &[&str])]) -> Peer {
        Peer {
            id: PeerId("p".into()),
            name: "n".into(),
            paired_at: 0,
            last_seen: None,
            online: true,
            sleeping: false,
            statuses: Vec::new(),
            grants: grants
                .iter()
                .map(|(plugin, caps)| {
                    (
                        PluginId::new(*plugin).unwrap(),
                        // The plugin's own guards: permissions OF that plugin.
                        Grants::new(caps.iter().map(|name| {
                            peersus_api::permission::Permission::new(*plugin, *name).unwrap()
                        }))
                        .unwrap(),
                    )
                })
                .collect(),
            plugin_values: BTreeMap::new(),
        }
    }

    #[test]
    fn routes_by_exported_service_and_exact_version_not_by_plugin_name() {
        let mut registry = Registry::default();
        registry
            .register(exporting("alpha", "svc.one", 1, Some("one.use")))
            .unwrap();
        registry
            .register(exporting("beta", "svc.two", 2, Some("beta.use")))
            .unwrap();
        assert_eq!(
            registry.route("svc.one", 1),
            Route::Plugin {
                index: 0,
                capability: Some("one.use")
            }
        );
        // Every route names the capability that guards it: a `receive` that names none is
        // refused at registration, so a route without one can no longer exist.
        assert_eq!(
            registry.route("svc.two", 2),
            Route::Plugin {
                index: 1,
                capability: Some("beta.use")
            }
        );
        // A different version, an unknown service and a plugin *name* never route.
        assert_eq!(registry.route("svc.one", 2), Route::Unknown);
        assert_eq!(registry.route("nope", 1), Route::Unknown);
        assert_eq!(registry.route("alpha", 1), Route::Unknown);
    }

    #[test]
    fn a_service_cannot_be_exported_twice_and_ids_are_unique() {
        let mut registry = Registry::default();
        registry
            .register(exporting("alpha", "svc", 1, Some("alpha.use")))
            .unwrap();
        assert_eq!(
            registry.register(exporting("beta", "svc", 2, Some("beta.use"))),
            Err(RegistryError::ServiceConflict),
        );
        assert_eq!(
            registry.register(exporting("alpha", "other", 1, Some("alpha.use"))),
            Err(RegistryError::Duplicate),
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn an_invalid_manifest_is_never_registered() {
        let mut bad = exporting("alpha", "svc", 1, Some("alpha.use"));
        bad.manifest.manifest_version = 99;
        let mut registry = Registry::default();
        assert_eq!(registry.register(bad), Err(RegistryError::Manifest));
        assert!(registry.is_empty());
    }

    #[test]
    fn access_is_default_deny_and_per_plugin() {
        let plugin = PluginId::new("alpha").unwrap();
        let none = peer_with(&[]);
        assert!(!permitted(&none, &plugin, Some("alpha.use")));
        assert!(!permitted(&none, &plugin, None));
        let granted = peer_with(&[("alpha", &["alpha.use"])]);
        assert!(permitted(&granted, &plugin, Some("alpha.use")));
        assert!(!permitted(&granted, &plugin, Some("alpha.admin")));
        // A grant for another plugin never carries over.
        assert!(!permitted(
            &granted,
            &PluginId::new("beta").unwrap(),
            Some("alpha.use")
        ));
        // An export without a capability still requires the plugin to be granted at all.
        assert!(permitted(&granted, &plugin, None));
        assert!(!permitted(&granted, &PluginId::new("beta").unwrap(), None));
    }

    #[test]
    fn a_permission_of_someone_else_with_the_same_name_does_not_satisfy_a_plugins_guard() {
        // `alpha`'s guard is `alpha → alpha.use`. A device that granted alpha the permission
        // `host → alpha.use`, or `beta → alpha.use`, granted something else that merely shares
        // a name, and it must not open alpha's guard.
        use peersus_api::permission::Permission;
        let plugin = PluginId::new("alpha").unwrap();
        for owner in ["host", "beta"] {
            let mut peer = peer_with(&[]);
            peer.grants.insert(
                plugin.clone(),
                Grants::new([Permission::new(owner, "alpha.use").unwrap()]).unwrap(),
            );
            assert!(
                !permitted(&peer, &plugin, Some("alpha.use")),
                "a permission of {owner} opened alpha's guard"
            );
        }
    }

    #[test]
    fn info_lists_every_plugin_with_its_manifest_in_registration_order() {
        let mut registry = Registry::default();
        registry
            .register(exporting("b", "s.b", 1, Some("b.use")))
            .unwrap();
        registry
            .register(exporting("a", "s.a", 1, Some("a.use")))
            .unwrap();
        let ids: Vec<_> = registry
            .info(|_| Grantable::default())
            .iter()
            .map(|i| i.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["b", "a"]);
    }

    fn with_action(consumers: &[&str], capability: Option<&str>) -> PluginSpec {
        use peersus_api::contributions::{Action, Source};
        let mut spec = exporting("alpha", "svc", 1, capability);
        spec.manifest.exports[0].consumers = consumers.iter().map(|s| (*s).to_owned()).collect();
        spec.manifest.exports[0]
            .methods
            .insert("go".to_owned(), member(capability));
        spec.manifest.contributions.actions.push(Action {
            id: "go".into(),
            label: "Go".into(),
            short_label: None,
            input: peersus_api::contributions::ActionInput::None,
            target: Source {
                service: "svc".into(),
                method: "go".into(),
            },
            capability: capability.map(str::to_owned),
            needs_device: false,
            destructive: false,
            timeout_ms: peersus_api::contributions::default_action_timeout(),
            chord: None,
            help: None,
        });
        spec
    }

    #[test]
    fn an_action_the_application_could_never_invoke_is_rejected_when_registered_not_when_clicked() {
        let mut registry = Registry::default();
        // The plugin declares a button but never lists the application as a consumer, so
        // the hub would refuse every click. That is reported now, as a clear error.
        assert_eq!(
            registry.register(with_action(&[], Some("alpha.use"))),
            Err(RegistryError::ActionUnreachable)
        );
        assert_eq!(
            registry.register(with_action(&["someone-else"], Some("alpha.use"))),
            Err(RegistryError::ActionUnreachable)
        );
        assert!(
            registry.is_empty(),
            "a rejected plugin leaves nothing behind"
        );
        // Listing the application makes the declaration honest.
        registry
            .register(with_action(&[peersus_plugin::hub::HOST], Some("alpha.use")))
            .unwrap();
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_plugin_without_actions_needs_no_application_consumer() {
        // Exporting nothing to the application is the author's choice and stays valid.
        let mut registry = Registry::default();
        registry
            .register(exporting("beta", "svc.two", 1, Some("beta.use")))
            .unwrap();
    }

    #[test]
    fn a_receive_export_must_name_the_capability_that_guards_it() {
        let mut registry = Registry::default();
        // Without a capability, default-deny has nothing to be a default of: a device is
        // permitted only through a grant, a grant can only name a capability, so no device
        // could ever send to this plugin. It would look functional and do nothing, so it is
        // refused when registered, with a reason, instead.
        assert_eq!(
            registry.register(exporting("sink", "sink.service", 1, None)),
            Err(RegistryError::ReceiveWithoutCapability)
        );
        assert!(
            registry.is_empty(),
            "a rejected plugin leaves nothing behind"
        );
        registry
            .register(exporting("sink", "sink.service", 1, Some("sink.write")))
            .unwrap();
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_plugin_that_does_not_receive_needs_no_receive_capability() {
        // Only `receive` is guarded by this rule. A plugin that exports other methods, with
        // or without capabilities, is not affected.
        let mut registry = Registry::default();
        let mut plugin = exporting("other", "other.service", 1, None);
        plugin.manifest.exports[0].methods.clear();
        plugin.manifest.exports[0]
            .methods
            .insert("status".to_owned(), member(None));
        registry.register(plugin).unwrap();
    }

    #[test]
    fn explicit_unsupported_platform_is_refused_before_registration_but_legacy_is_preserved() {
        use peersus_api::platform::OperatingSystem;
        let Some(current) = OperatingSystem::current() else {
            return;
        };
        let mut spec = exporting("platform-test", "platform.service", 1, Some("use"));
        let other = OperatingSystem::ALL
            .into_iter()
            .find(|os| *os != current)
            .unwrap();
        spec.manifest.platforms = Some(vec![other]);
        let mut registry = Registry::default();
        assert_eq!(
            registry.register(spec.clone()),
            Err(RegistryError::UnsupportedPlatform)
        );
        assert!(registry.is_empty());
        spec.manifest.platforms = Some(vec![current]);
        registry.register(spec).unwrap();
        let legacy = exporting("legacy", "legacy.service", 1, Some("use"));
        assert!(legacy.manifest.platforms.is_none());
        registry.register(legacy).unwrap();
    }
}
