//! The person's preferences, relay token and grants, and each extension's own saved values.
use super::*;

impl Runtime {
    /// Persist new preferences. Only plugins whose own saved values changed have
    /// their work stopped, so one plugin's setting never interrupts another's.
    pub(super) async fn apply_settings(
        &mut self,
        settings: peersus_core::Settings,
    ) -> Result<(), &'static str> {
        let before = self.settings.current().plugins.clone();
        self.network
            .update_settings(&mut self.settings, settings)
            .await?;
        let after = &self.settings.current().plugins;
        let changed: Vec<PluginId> = before
            .keys()
            .chain(after.keys())
            .filter(|id| before.get(*id) != after.get(*id))
            .cloned()
            .collect();
        for id in changed {
            // A setting of the plugin's own can only be picked up by a new process.
            self.restart_plugin(&id);
        }
        Ok(())
    }

    /// Use this token for the personal relay and keep it for next time, or forget it when it is empty.
    ///
    /// The order is the point: the transport that uses it is bound first, so a token that cannot be applied is never saved,
    /// and it is saved second, so that what runs is what will be there after a restart. If saving fails the transport goes
    /// back to the token it had, and the person is told, so that nothing is running on a token that is not kept.
    pub(super) async fn set_relay_token(&mut self, token: &str) -> Result<(), &'static str> {
        let store = self
            .token_store
            .clone()
            .ok_or("This device cannot keep a relay token")?;
        let access = peersus_net::transport::RelayAccess::new(token);
        let previous = self.network.relay_access().cloned();
        self.network
            .set_relay_access(&mut self.settings, access)
            .await?;
        if let Err(refusal) = store.save(token) {
            // Back to what was kept, with the same guarded path. If even that is refused the first error is the one told.
            let _ = self
                .network
                .set_relay_access(&mut self.settings, previous)
                .await;
            return Err(refusal);
        }
        Ok(())
    }

    /// Save what one plugin may do, for one device or, with no device, outright.
    ///
    /// The two are different kinds of permission and each is checked against its own: what is
    /// about a device can be granted only for a device, and what is about no device only
    /// outright. Anything else is refused rather than stored somewhere it would mean nothing,
    /// or worse, would come to mean something later.
    pub(super) fn set_grants(
        &mut self,
        plugin: PluginId,
        peer: Option<PeerId>,
        grants: Grants,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let spec = self.plugins.get(index).ok_or("Unknown plugin")?;
        // Only what is grantable to this plugin can be granted: its own guards, and the
        // application's guards on what it imports. A name the plugin merely declared for
        // itself is neither, and does not become a permission by being granted.
        let allowed = if peer.is_some() {
            grantable_for_a_device(spec)
        } else {
            grantable_without_a_device(spec)
        };
        if let Some(peer) = &peer {
            self.peer(peer).ok_or("Device is not paired")?;
        }
        if grants
            .iter()
            .any(|permission| !allowed.contains(permission))
        {
            return Err("That permission cannot be granted to this plugin");
        }
        // Event subscriptions are admitted once, and revocation removes them. Granting
        // an event later cannot resurrect a refused/removed subscription in the same
        // process. Rebind only when a guard on an imported host event changes.
        let rebind_events = peer.is_none()
            && host_manifest(&self.plugins).exports.iter().any(|export| {
                spec.manifest.imports.iter().any(|import| {
                    import.provider == HOST
                        && import.service == export.service
                        && import.version == export.version
                        && import.events.iter().any(|event| {
                            export
                                .events
                                .get(event)
                                .and_then(|member| member.capability.as_ref())
                                .and_then(|capability| Permission::new(HOST, capability))
                                .is_some_and(|permission| {
                                    self.standalone
                                        .get(&plugin)
                                        .is_some_and(|saved| saved.allows(&permission))
                                        != grants.allows(&permission)
                                })
                        })
                })
            });
        let id = plugin.clone();
        match &peer {
            Some(peer) => self.network.set_grants(peer, plugin, grants),
            None => self.standalone.set(plugin, grants),
        }
        .map_err(|_| "Could not save permissions")?;
        // The decision is saved. Recompute what the application itself may call from every
        // saved decision, so the hub never disagrees with what the user chose.
        self.sync_host_grants();
        self.sync_plugin_grants();
        // A change of permission for a device withdraws what was already started for it. The
        // plugin itself keeps running and keeps serving every other device. A permission about
        // no device has no such work: what it guards is checked at the hub on every call.
        if let Some(peer) = &peer {
            self.withdraw_device_work(Some(&id), peer);
        } else {
            // Saved standalone grants can authorize transient file/HTTP handles.
            // A permission change invalidates those handles before another call.
            self.revoke_platform_calls(&id);
            if rebind_events {
                self.restart_plugin(&id);
            }
        }
        Ok(())
    }

    /// Make the hub's record of what the application may call match what the user has
    /// granted, recomputed from every device. Idempotent, so it is safe after any change
    /// and again at start-up to restore the picture after a restart.
    pub(super) fn sync_host_grants(&mut self) {
        let granted = host_grants(&self.plugins, &self.network.peers());
        let offered_host = host_manifest(&self.plugins);
        let platform_timeouts = crate::platform_services::method_timeouts(&offered_host);
        // Each step on its own: a manifest the hub refused must not also keep a permission the person withdrew in force.
        let manifest = self.host.update_host_manifest(offered_host);
        let timeouts = self.host.set_host_method_timeouts(platform_timeouts);
        let permissions = self.hub.set_permissions(HOST, granted);
        if manifest.and(timeouts).and(permissions).is_err() {
            self.error("Permissions could not be applied");
        }
    }

    /// Make the hub's record of what each plugin may present match what the user has
    /// granted, recomputed from every device and from what was granted outright. The hub
    /// records it whether or not the plugin's process is running at this moment, so a
    /// withdrawal made while a crashed plugin waits for its restart is as binding as any
    /// other. The only way it can fail is a permission the hub finds malformed, which is never
    /// something to ignore.
    pub(super) fn sync_plugin_grants(&mut self) {
        let peers = self.network.peers();
        let failed = self
            .plugins
            .iter()
            .map(|spec| {
                self.hub.set_permissions(
                    spec.id.as_str(),
                    plugin_grants(spec, &peers, self.standalone.all()),
                )
            })
            .any(|result| result.is_err());
        if failed {
            self.error("Permissions could not be applied");
        }
    }

    pub(super) async fn set_plugin_setting(
        &mut self,
        plugin: PluginId,
        peer: Option<PeerId>,
        setting: &str,
        value: Value,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let declared = self
            .plugins
            .get(index)
            .ok_or("Unknown plugin")?
            .manifest
            .contributions
            .settings
            .clone();
        let scope = declared
            .iter()
            .find(|s| s.id == setting)
            .ok_or("Unknown setting")?
            .scope;
        match (scope, peer) {
            (Scope::Device, Some(peer)) => {
                self.peer(&peer).ok_or("Device is not paired")?;
                let id = plugin.clone();
                self.network
                    .set_plugin_value(&peer, plugin, &declared, setting, value)
                    .map_err(|_| "Invalid value for this setting")?;
                // A device-level value may change what the plugin does for that device, so
                // that device's running work is withdrawn. The plugin is left running.
                self.withdraw_device_work(Some(&id), &peer);
            }
            (Scope::Global, None) => {
                let mut next = self.settings.current().clone();
                next.plugins
                    .entry(plugin)
                    .or_default()
                    .set(&declared, Scope::Global, setting, value)
                    .map_err(|_| "Invalid value for this setting")?;
                self.apply_settings(next).await?;
            }
            (Scope::Device, None) => return Err("Choose a device for this setting"),
            (Scope::Global, Some(_)) => return Err("This setting is not per device"),
        }
        Ok(())
    }
}
