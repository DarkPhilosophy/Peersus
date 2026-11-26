//! The person's preferences, relay token and grants, and each extension's own saved values.
use super::*;
use crate::platform_services::file_http::FileHttpServices;
use peersus_api::contributions::{EdgePressure, PublicRead, Setting};

/// An existing entry whose value changed is one restart, not both a removal and an addition.
fn changed_plugins(
    before: &std::collections::BTreeMap<PluginId, peersus_core::plugins::Values>,
    after: &std::collections::BTreeMap<PluginId, peersus_core::plugins::Values>,
) -> Vec<PluginId> {
    before
        .iter()
        .filter(|(id, values)| after.get(*id) != Some(*values))
        .map(|(id, _)| id)
        .chain(after.keys().filter(|id| !before.contains_key(*id)))
        .cloned()
        .collect()
}

fn public_read_setting(spec: &PluginSpec) -> Option<(&Setting, &PublicRead)> {
    spec.manifest
        .contributions
        .settings
        .iter()
        .find_map(|setting| {
            setting
                .public_read
                .as_ref()
                .map(|resource| (setting, resource))
        })
}

impl Runtime {
    fn public_read_agreement(&self, spec: &PluginSpec) -> Option<&str> {
        let digest = self.accepted_agreements.get(spec.id.as_str())?;
        let installed = spec.manifest.agreements.as_ref()?;
        (peersus_core::agreements::digest(installed) == *digest).then_some(digest.as_str())
    }

    pub(super) fn public_read_directory(&self) -> Option<&std::path::Path> {
        self.platform_services.as_deref()?.public_read_directory()
    }

    pub(super) fn refresh_public_read_settings(&mut self) {
        let permission = Permission::new(HOST, crate::platform_services::PUBLIC_HTTP_CAPABILITY);
        self.public_read_enabled = self
            .plugins
            .iter()
            .filter_map(|spec| {
                let (setting, resource) = public_read_setting(spec)?;
                if !setting.available_on(peersus_api::platform::OperatingSystem::current()) {
                    return None;
                }
                let allowed = self.standalone.get(&spec.id)?.allows(permission.as_ref()?);
                let digest = self.public_read_agreement(spec)?;
                let legacy_enabled = self
                    .settings
                    .current()
                    .plugins
                    .get(&spec.id)
                    .and_then(|values| values.get(&setting.id))
                    != Some(&Value::Toggle(false));
                (allowed
                    && FileHttpServices::public_read_approved(
                        self.public_read_directory()?,
                        &spec.id,
                        digest,
                        resource,
                        legacy_enabled,
                    ))
                .then(|| spec.id.clone())
            })
            .collect();
    }

    /// The same effective value is published to the window and to the guest; raw configuration cannot grant access.
    pub(super) fn with_public_read_value(
        &self,
        spec: &PluginSpec,
        mut values: peersus_core::plugins::Values,
    ) -> peersus_core::plugins::Values {
        if let Some((setting, _)) = public_read_setting(spec) {
            let _ = values.set(
                &spec.manifest.contributions.settings,
                Scope::Global,
                &setting.id,
                Value::Toggle(self.public_read_enabled.contains(&spec.id)),
            );
        }
        values
    }

    pub(super) fn public_read_may_restore(
        &self,
        plugin: &PluginId,
        origin: Option<&str>,
        root: Option<&str>,
    ) -> bool {
        self.plugins
            .find(plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| {
                public_read_setting(spec).is_none_or(|(_, scope)| {
                    self.public_read_enabled.contains(plugin)
                        && origin == Some(scope.origin.as_str())
                        && root == Some(scope.root.as_str())
                })
            })
    }

    pub(super) fn set_public_read_setting(
        &mut self,
        plugin: PluginId,
        id: &str,
        resource: &PublicRead,
        enabled: bool,
    ) -> Result<(), &'static str> {
        let spec = self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or("Unknown plugin")?;
        let (setting, expected) =
            public_read_setting(spec).ok_or("Unknown public resource setting")?;
        if setting.id != id || expected != resource {
            return Err("This setting changed. Review it in Settings before trying again.");
        }
        if !setting.available_on(peersus_api::platform::OperatingSystem::current()) {
            return Err("This setting is not available on this platform");
        }
        let digest = self
            .public_read_agreement(spec)
            .ok_or("Accept the extension agreement before enabling this setting")?
            .to_owned();
        let platform = self
            .platform_services
            .clone()
            .ok_or("This platform does not offer the requested facility")?;
        let data = platform
            .public_read_directory()
            .ok_or("This platform does not offer the requested facility")?;
        let permission = Permission::new(HOST, crate::platform_services::PUBLIC_HTTP_CAPABILITY)
            .ok_or("Public HTTPS is unavailable")?;
        if !grantable_without_a_device(spec).contains(&permission) {
            return Err("This extension does not import Public HTTPS");
        }
        let held = self.standalone.get(&plugin).cloned().unwrap_or_default();
        if enabled && !held.allows(&permission) {
            let next = applied(&held, &permission, true)
                .map_err(|_| "Could not change this permission")?;
            self.set_grants(plugin.clone(), None, next)?;
        }
        FileHttpServices::decide_public_read(data, &plugin, &digest, resource, enabled)?;
        if enabled {
            self.public_read_enabled.insert(plugin.clone());
        } else {
            self.public_read_enabled.remove(&plugin);
        }
        self.restart_plugin(&plugin);
        Ok(())
    }

    /// Persist new preferences. Only plugins whose own saved values changed have
    /// their work stopped, so one plugin's setting never interrupts another's.
    pub(super) async fn apply_settings(
        &mut self,
        settings: peersus_core::Settings,
    ) -> Result<(), &'static str> {
        let before = &self.settings.current().plugins;
        let changed = changed_plugins(before, &settings.plugins);
        // A complete preferences update can change an extension's values without going through
        // SetPluginSetting. Check the capture contract at the shared write boundary as well.
        for id in &changed {
            let Some(contributions) = self
                .plugins
                .find(id)
                .and_then(|index| self.plugins.get(index))
                .map(|spec| &spec.manifest.contributions)
            else {
                continue;
            };
            let Some(input) = &contributions.input else {
                continue;
            };
            let Some(setting) = input
                .pressure_setting(&contributions.settings)
                .map_err(|_| "Invalid value for this setting")?
            else {
                continue;
            };
            let old = before.get(id).and_then(|values| values.get(&setting.id));
            let new = settings
                .plugins
                .get(id)
                .and_then(|values| values.get(&setting.id));
            if new != old {
                if !setting.available_on(peersus_api::platform::OperatingSystem::current()) {
                    return Err("This setting is not available on this platform");
                }
                if let Some(value) = new {
                    value
                        .check(&setting.kind)
                        .map_err(|_| "Invalid value for this setting")?;
                }
            }
        }
        self.network
            .update_settings(&mut self.settings, settings)
            .await?;
        self.apply_private_mode();
        self.apply_crash_capture();
        for id in changed {
            // A setting of the plugin's own can only be picked up by a new process.
            self.restart_plugin(&id);
        }
        Ok(())
    }

    /// Starts or stops this owner's crash capture as the `crash_capture` preference says. Returns at once; the outcome
    /// arrives on the loop as a change of capture's status.
    pub(super) fn apply_crash_capture(&self) {
        if let Some(capture) = &self.crash_capture {
            capture.set(self.settings.current().crash_capture);
        }
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
        let device = peer
            .as_ref()
            .map(|peer| self.peer(peer).ok_or("Device is not paired"))
            .transpose()?;
        if grants
            .iter()
            .any(|permission| !allowed.contains(permission))
        {
            return Err("That permission cannot be granted to this plugin");
        }
        let before = match &device {
            Some(device) => device.grants.get(&plugin),
            None => self.standalone.get(&plugin),
        };
        let changed = before.map_or(!grants.is_empty(), |held| held != &grants);
        let withdrawn: Vec<_> = before
            .into_iter()
            .flat_map(Grants::iter)
            .filter(|permission| !grants.allows(permission))
            .cloned()
            .collect();
        // The broker drops revoked subscriptions itself. Only a newly granted event
        // can require re-binding a subscription that was refused earlier.
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
                                    grants.allows(&permission)
                                        && !self
                                            .standalone
                                            .get(&plugin)
                                            .is_some_and(|saved| saved.allows(&permission))
                                })
                        })
                })
            });
        let id = plugin.clone();
        // A status it turned on is said by every paired device until it is turned off, and without the permission it can no longer
        // turn it off itself: the statuses go with the permission (the devices are told only if one was on).
        let statuses_go = peer.is_none()
            && Permission::new(HOST, crate::status_service::STATUS_SET_CAPABILITY)
                .is_none_or(|permission| !grants.allows(&permission));
        let public_access_before_after = peer
            .is_none()
            .then(|| {
                Permission::new(HOST, crate::platform_services::PUBLIC_HTTP_CAPABILITY).map(
                    |permission| {
                        (
                            self.standalone
                                .get(&id)
                                .is_some_and(|saved| saved.allows(&permission)),
                            grants.allows(&permission),
                        )
                    },
                )
            })
            .flatten();
        match &peer {
            Some(peer) => self.network.set_grants(peer, plugin, grants),
            None => self.standalone.set(plugin, grants),
        }
        .map_err(|_| "Could not save permissions")?;
        if changed {
            self.board.clear_permission_failures(&id);
        }
        // The deliberate end uses the existing input method's old broker admission,
        // but carries only a cleanup marker. No input can be queued behind it.
        for permission in &withdrawn {
            self.withdraw_grant_work(&id, peer.as_ref(), permission);
        }
        // The decision is saved. Recompute what the application itself may call from every
        // saved decision, so the hub never disagrees with what the user chose.
        self.sync_host_grants();
        self.sync_plugin_grants();
        if peer.is_none() && changed {
            for permission in &withdrawn {
                self.platform_calls.revoke_permission(&id, permission);
                if let Some(platform) = &self.platform_services {
                    platform.revoke_permission(&id, permission);
                }
                if permission.name == crate::platform_services::NOTIFY_CAPABILITY {
                    self.notifications.revoke(&id);
                }
                // Taking the microphone back ends a recording at once, kept by nobody.
                if permission.name == peersus_api::audio::RECORD_CAPABILITY {
                    self.microphone.clear(&id);
                }
            }
            match public_access_before_after {
                Some((true, false)) => {
                    self.public_read_enabled.remove(&id);
                    if let Some(data) = self.public_read_directory() {
                        FileHttpServices::forget_public_read(data, &id);
                    }
                }
                // A same-scope update can have its existing grants restored separately from the package.
                Some((false, true)) => self.refresh_public_read_settings(),
                _ => {}
            }
            if statuses_go {
                self.forget_statuses(&id);
            }
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
        let contributions = &self
            .plugins
            .get(index)
            .ok_or("Unknown plugin")?
            .manifest
            .contributions;
        let declared_setting = contributions
            .settings
            .iter()
            .find(|s| s.id == setting)
            .ok_or("Unknown setting")?;
        if declared_setting.public_read.is_some() {
            return Err("Change this public resource in the extension's Settings");
        }
        if let Some(input) = &contributions.input
            && matches!(&input.edge_pressure, EdgePressure::Setting { id } if id == setting)
            && !declared_setting.available_on(peersus_api::platform::OperatingSystem::current())
        {
            return Err("This setting is not available on this platform");
        }
        let scope = declared_setting.scope;
        let declared = contributions.settings.clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_only_changed_plugins_once_each() {
        let before = serde_json::from_str(
            r#"{"edited":{"show":true},"removed":{},"untouched":{"show":true}}"#,
        )
        .unwrap();
        let after = serde_json::from_str(
            r#"{"added":{},"edited":{"show":false},"untouched":{"show":true}}"#,
        )
        .unwrap();
        let mut changed = changed_plugins(&before, &after);
        changed.sort();
        assert_eq!(
            changed.iter().map(PluginId::as_str).collect::<Vec<_>>(),
            ["added", "edited", "removed"]
        );
        assert!(changed_plugins(&after, &after).is_empty());
    }
}
