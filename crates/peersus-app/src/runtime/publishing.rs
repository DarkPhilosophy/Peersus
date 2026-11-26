//! What the application tells windows and extensions: errors, plugin lists, snapshots, pages, transfers and statuses.
use super::*;

use peersus_api::{
    contributions::{Contributions, EdgePressure},
    platform::OperatingSystem,
};

fn project_settings(contributions: &mut Contributions, here: Option<OperatingSystem>) {
    contributions
        .settings
        .retain(|setting| setting.available_on(here));
    // A platform projection cannot keep a reference to a setting it does not show.
    if let Some(input) = &mut contributions.input
        && let EdgePressure::Setting { id } = &input.edge_pressure
        && !contributions
            .settings
            .iter()
            .any(|setting| setting.id == *id)
    {
        input.edge_pressure = EdgePressure::Off {};
    }
}

impl Runtime {
    /// One more error for the person to read: counted every time, kept once while it is the last one, sixteen at most.
    pub(super) fn say_error(&mut self, text: String) {
        self.errors_said = self.errors_said.wrapping_add(1);
        if self.errors.last().is_some_and(|last| *last == text) {
            return;
        }
        if self.errors.len() == 16 {
            self.errors.remove(0);
        }
        self.errors.push(text);
    }

    pub(super) fn error(&mut self, reason: &'static str) {
        // Only fixed application-owned sentences enter diagnostics, never plugin text.
        tracing::warn!(reason, "Owner operation failed");
        self.say_error(reason.into());
    }

    /// An error of one plugin: the UI names its host-assigned ID, while diagnostics retain
    /// only the fixed sentence. Broker call/connection metadata provides trace correlation
    /// without copying installed package identities into the log.
    pub(super) fn plugin_error(&mut self, plugin: &PluginId, reason: &'static str) {
        tracing::warn!(reason, "Plugin operation failed");
        self.say_plugin_notice(plugin, reason);
    }

    pub(super) fn say_plugin_notice(&mut self, plugin: &PluginId, reason: &'static str) {
        self.say_error(format!("[{}] {reason}", plugin.as_str()));
    }

    pub(super) fn plugin_status(&self, plugin: &PluginId) -> peersus_core::model::PluginStatus {
        let event = self.running.get(plugin).map(|r| r.health.borrow());
        status_of(event.as_deref())
    }

    /// Every plugin with what state it is in right now. The one place the registry's
    /// declarations and the supervisors' reports are put together, so that what a frontend is
    /// shown and what `peersus.plugins` tells a plugin can never disagree.
    pub(super) fn plugin_infos(&self) -> Vec<peersus_core::model::PluginInfo> {
        let mut plugins = self.plugins.info(grantable_for_window);
        let here = OperatingSystem::current();
        for info in &mut plugins {
            project_settings(&mut info.manifest.contributions, here);
            // No extension puts a page in the main navigation by declaring it there: that is the person's choice. A page its
            // manifest places `primary` is drawn in the extension's settings, like every page it declares there; one gets into
            // the navigation only as an offer (`peersus.pages`), which an extension makes when the person turns its option on.
            for page in &mut info.manifest.contributions.pages {
                if page.placement == peersus_api::contributions::Placement::Primary {
                    page.placement = peersus_api::contributions::Placement::Settings;
                }
            }
            info.manifest
                .contributions
                .pages
                .extend(self.page_offers.pages(&info.id).iter().cloned());
            self.page_offers.project_actions(
                &info.id,
                || self.hub.registration_generation(info.id.as_str()),
                &mut info.manifest.contributions,
            );
            // Read the supervisor's last report fresh each time: a status taken once at
            // start would go stale the moment a plugin fails or is restarted.
            info.status = self.plugin_status(&info.id);
            // What the folder allows is a different thing from what the process is doing, and is not overwritten by it:
            // the registry says every plugin in it is ready, and the person's switch is what makes one of them disabled.
            if self.disabled.contains(&info.id) {
                info.standing = peersus_core::model::Standing::Disabled;
            }
            info.agreed = crate::plugin_agreements::is_accepted(
                self.accepted_agreements.get(info.id.as_str()),
                &info.manifest,
            );
        }
        plugins
    }

    /// What the WINDOW is shown: every plugin in the registry, and after them every plugin that was found in the store and is
    /// not run, each with what its folder allows. A plugin that is not approved, changed or broken is therefore never
    /// missing from the window, and can be approved or removed from it.
    ///
    /// This is a separate list from [`Runtime::plugin_infos`], on purpose: that one is also what `peersus.plugins` tells a
    /// plugin about the others, and a plugin is not to learn which of the person's other plugins are blocked or unapproved.
    pub(super) fn window_plugin_infos(&self) -> Vec<peersus_core::model::PluginInfo> {
        let mut plugins = self.plugin_infos();
        // A file action can be taken back without ending work already admitted.
        // Give only the window its declaration alongside the Running status,
        // so the person can still cancel it. This does not offer a new run.
        for key in self.board.running_file_actions() {
            let Some(info) = plugins.iter_mut().find(|info| info.id == key.plugin) else {
                continue;
            };
            let actions = &mut info.manifest.contributions.actions;
            if actions.iter().any(|action| action.id == key.action) {
                continue;
            }
            if let Some(action) = self
                .plugins
                .find(&key.plugin)
                .and_then(|index| self.plugins.action(index, &key.action))
            {
                actions.push(action.clone());
            }
        }
        let here = OperatingSystem::current();
        plugins.extend(
            self.held_back
                .iter()
                .map(|held| peersus_core::model::PluginInfo {
                    id: held.id.clone(),
                    manifest: {
                        let mut manifest = held.manifest.clone();
                        project_settings(&mut manifest.contributions, here);
                        self.page_offers.project_actions(
                            &held.id,
                            || None,
                            &mut manifest.contributions,
                        );
                        manifest
                    },
                    standing: held.standing,
                    // Nothing of it runs, so there is no process to speak of.
                    status: peersus_core::model::PluginStatus::Stopped,
                    // Not run, so nothing can be granted to it, and the window does not offer.
                    grantable: peersus_core::model::Grantable::default(),
                    // Given back by the window with `ApprovePlugin`, only for what can be approved: what is broken has nothing to
                    // approve, so no digest is offered for it.
                    program: match held.standing {
                        peersus_core::model::Standing::NeedsApproval
                        | peersus_core::model::Standing::Changed => held.program.clone(),
                        _ => None,
                    },
                    package: self.installed_packages.get(&held.id).cloned(),
                    agreed: crate::plugin_agreements::is_accepted(
                        self.accepted_agreements.get(held.id.as_str()),
                        &held.manifest,
                    ),
                    from_store: self.installed_from_store.contains(&held.id),
                }),
        );
        // Author and link are for the PERSON. `plugin_infos` is also what a plugin is told about the others,
        // so the block is added here, to the window's list only, and so is where an installed package came from. So are the
        // package's own translations of its words (its contributions and its description): a plugin is told another's
        // manifest as written. What a capability lets it do is never taken from them: it is what a person grants on, and
        // stays the manifest's English, which the store reviews.
        let language = self.person_language();
        for info in &mut plugins {
            info.from_store = self.installed_from_store.contains(&info.id);
            if info.package.is_none() {
                info.package = self.installed_packages.get(&info.id).cloned();
            }
            if let Some(catalog) = self
                .plugins
                .find(&info.id)
                .and_then(|index| self.plugins.get(index))
                .and_then(|spec| crate::package_languages::of(&spec.languages, language))
            {
                info.manifest.contributions.translate(catalog);
                if let Some(package) = &mut info.package {
                    package.translate(catalog);
                }
            }
        }
        plugins
    }

    /// The person's language: the one they chose, else the device's, else English. What `peersus.language` answers and
    /// what a package's own words are shown in.
    pub(super) fn person_language(&self) -> Language {
        let chosen = self.settings.current().language;
        self.languages.as_ref().map_or_else(
            || crate::languages::person_language(chosen, None),
            |languages| languages.wanted(chosen),
        )
    }

    /// The packages waiting to be confirmed, as the same shape as an installed plugin, in the order
    /// of their names so that the window does not reshuffle them between snapshots.
    pub(super) fn staged_infos(&self) -> Vec<peersus_core::model::PluginInfo> {
        let mut infos: Vec<_> = self
            .staged
            .iter()
            .map(|(id, staged)| peersus_core::model::PluginInfo {
                id: id.clone(),
                manifest: staged.manifest.clone(),
                // Shown before it is installed, so it is not approved: the person is being asked, and that is what this is.
                standing: peersus_core::model::Standing::NeedsApproval,
                status: peersus_core::model::PluginStatus::Stopped,
                // Not installed yet, so nothing can be granted to it, and the window does not offer.
                grantable: peersus_core::model::Grantable::default(),
                program: None,
                package: staged.package.clone(),
                // Not accepted yet: the person has not been asked, and what they accepted of an earlier version does not
                // carry to a package that has not been confirmed.
                agreed: false,
                from_store: staged.from_store,
            })
            .collect();
        infos.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        infos
    }

    /// The picture a window may be given for `digest`: this device's own, or the one a paired device
    /// shows now, and nothing else. What is kept in the cache but is nobody's picture any more is not
    /// produced, so a window cannot read through the folder for pictures it was never meant to see.
    pub(super) fn picture_for(
        &self,
        digest: &peersus_core::profile::Digest,
    ) -> Option<peersus_core::profile::Picture> {
        if let Some(own) = self
            .network
            .own_picture()
            .filter(|own| own.digest() == *digest)
        {
            return Some(own.clone());
        }
        let peer = self
            .network
            .peer_digests()
            .into_iter()
            .find(|(_, held)| held == digest)?
            .0;
        self.network.peer_picture(&peer)
    }

    /// Advertise only what each paired device can send here.
    pub(super) fn publish(&mut self) {
        self.prune_inbound_visits();
        if self.outbound_visit.is_some()
            && self
                .capture
                .as_ref()
                .and_then(|key| key.device.as_ref())
                .is_some_and(|peer| !self.may_send_now(peer))
        {
            self.outbound_visit_lost(self.last_visit, false);
        }
        // Running by the same word the window shows (`status_of`), and not switched off: an extension that would not answer is
        // not said to take anything.
        let running: std::collections::BTreeSet<PluginId> = self
            .running
            .iter()
            .filter(|(id, process)| {
                !self.disabled.contains(*id)
                    && status_of(Some(&process.health.borrow()))
                        == peersus_core::model::PluginStatus::Running
            })
            .map(|(id, _)| id.clone())
            .collect();
        let accepts = self
            .network
            .peers()
            .iter()
            .map(|peer| {
                let services = super::admission::accepted_from(
                    &self.plugins,
                    &running,
                    peer,
                    self.standalone.all(),
                );
                (peer.id.clone(), services)
            })
            .collect();
        self.network.set_accepts(accepts);
        self.fill_choosers(None);
        let advertised_change = self.network.take_accepts_changed();
        self.publish_snapshot(advertised_change);
    }

    /// Fill in the devices of the choosers on the pages of `plugin` (of every extension with none) with what is true now. Returns
    /// whether any page changed. A page's chooser is filled when its tree comes and again at every publish, since what makes a
    /// device choosable (a connection, a permission, a status, what the device said it takes) ends in one.
    pub(super) fn fill_choosers(&mut self, plugin: Option<&PluginId>) -> bool {
        let peers = self.network.peers();
        let by_device = self.network.peer_accepts();
        let watch = Permission::new(HOST, crate::status_service::STATUS_WATCH_CAPABILITY);
        let (plugins, standalone) = (&self.plugins, self.standalone.all());
        self.page_views.fill_devices(plugin, |plugin, pick, show| {
            let Some(spec) = plugins.find(plugin).and_then(|index| plugins.get(index)) else {
                return Vec::new();
            };
            let own = own_streams(spec);
            let reads_statuses = watch
                .as_ref()
                .is_some_and(|watch| plugin_grants(spec, &peers, standalone).contains(watch));
            device_entries(
                plugin,
                &grantable_for_a_device(spec),
                &peers,
                &Taken {
                    own: &own,
                    by_device: Some(&by_device),
                },
                reads_statuses,
                pick,
                show,
            )
        })
    }

    /// What extensions keep for their pages' pictures. A read that panicked while holding it left nothing half-written (every
    /// change is one insert or removal), so a poisoned lock is still used.
    pub(super) fn kept_pictures(
        &self,
    ) -> std::sync::MutexGuard<'_, crate::kept_pictures::KeptPictures> {
        self.pictures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Write the digest of each kept picture into the pictures the pages of `plugin` (of every extension with none) place.
    /// Returns whether any page changed.
    pub(super) fn fill_pictures(&mut self, plugin: Option<&PluginId>) -> bool {
        let kept = self
            .pictures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.page_views.fill_pictures(plugin, |plugin, id| {
            kept.digest(plugin, id).map(|digest| digest.to_string())
        })
    }

    /// After a `keep` or `forget` finished: the pages that place what changed are filled in again and the windows told.
    pub(super) fn publish_pictures_if_changed(&mut self) {
        let changed = self.kept_pictures().take_changed();
        if changed && self.fill_pictures(None) {
            self.publish_pages();
        }
    }

    fn publish_snapshot(&mut self, advertised_change: bool) {
        let plugins = self.window_plugin_infos();
        let transfers = self.network.transfers();
        self.tell_plugins_of_transfers(&transfers);
        let peers = self.network.peers();
        self.notifications
            .retain_missing_permissions(|cause| self.permission_still_missing(cause, &peers));
        self.notifications
            .retain_unavailable_recipients(|peer, service, version| {
                peers.iter().any(|paired| &paired.id == peer)
                    && matches!(self.plugins.route(service, version), Route::Unknown)
            });
        self.tell_plugins_of_statuses(&peers);
        // Only a device that is paired now is ever told as near, and only one that is not as a stranger, whatever the
        // adapter said and whenever it said it. Strangers are told only while a window looks for them.
        let sought = self
            .nearby_sought
            .is_some_and(|at| at.elapsed() < NEARBY_SOUGHT_FOR);
        let measuring = self.settings.current().nearby_send
            && self
                .nearby_measured
                .is_some_and(|at| at.elapsed() < NEARBY_SOUGHT_FOR);
        let near: Vec<PeerId> = self
            .nearby
            .near
            .iter()
            .filter(|id| peers.iter().any(|peer| &peer.id == *id))
            .cloned()
            .collect();
        let nearby = peersus_core::nearby::Found {
            // Only of the devices that are near, whatever the adapter said.
            receiving: self
                .nearby
                .receiving
                .iter()
                .filter(|id| near.contains(id))
                .cloned()
                .collect(),
            near,
            strangers: self
                .nearby
                .strangers
                .iter()
                .filter(|stranger| sought && !peers.iter().any(|peer| peer.id == stranger.id))
                .cloned()
                .collect(),
            trouble: self.nearby.trouble,
            measuring,
            strongest: self.nearby.strongest.filter(|_| measuring),
        };
        let mut settings = self.settings.current().clone();
        for spec in self.plugins.iter().filter(|spec| {
            spec.manifest
                .contributions
                .settings
                .iter()
                .any(|s| s.public_read.is_some())
        }) {
            let values = settings.plugins.entry(spec.id.clone()).or_default();
            *values = self.with_public_read_value(spec, std::mem::take(values));
        }
        let mut next = Snapshot {
            settings,
            peers,
            grants: self.standalone.all().clone(),
            plugins,
            staged: self.staged_infos(),
            store: self.store_state(),
            update: self.update_state(),
            actions: self.board.statuses(),
            transfers,
            pairing: self.network.pairing(),
            extension_prompts: self
                .extension_prompts
                .as_ref()
                .map_or_else(Vec::new, |p| p.snapshot()),
            notifications: self.notifications.snapshot(),
            opened: self.opened.clone(),
            errors: self.errors.clone(),
            errors_said: self.errors_said,
            profile_picture: self
                .network
                .own_picture()
                .map(peersus_core::profile::Picture::digest),
            peer_pictures: self.network.peer_digests(),
            displays: self.network.own_displays().to_vec(),
            peer_displays: self.network.peer_displays(),
            files_code: self.network.files_code(),
            link_tests: self.network.link_tests(),
            typed_addresses: self.network.typed_addresses(),
            listening_port: self.network.listening_port(),
            relay: Some(self.network.relay_diagnostics()),
            relay_probes: self.network.relay_probes(),
            page_views: self.page_views.snapshot(),
            kept_layers: self.layers.stamps(),
            nearby,
            languages: self
                .languages
                .as_ref()
                .map(|languages| languages.state(self.settings.current().language))
                .unwrap_or_default(),
            diagnostics: self
                .crash_capture
                .as_ref()
                .map(crate::crash_capture::CrashCapture::state)
                .unwrap_or_default(),
        };
        self.publish_configuration_changes(&next, advertised_change);
        // A changed advertisement can halt a live input visit during configuration publication.
        if next.errors_said != self.errors_said {
            next.errors.clone_from(&self.errors);
            next.errors_said = self.errors_said;
        }
        // An extension that shows words of its own is told when the person's language changes. One that cannot keep up misses
        // it, as with every published event, and `get` always answers what is true now.
        let previous = self.snapshots.borrow();
        let told = crate::languages::person_language(
            previous.settings.language,
            previous.languages.device,
        );
        drop(previous);
        let now = crate::languages::person_language(next.settings.language, next.languages.device);
        if told != now {
            let _ = self.host.submit(peersus_api::message::Operation::Publish {
                service: LANGUAGE_SERVICE.to_owned(),
                version: 1,
                event: LANGUAGE_CHANGED.to_owned(),
                data: serde_json::json!({ "language": now.tag() }),
            });
        }
        self.send_if_changed(next);
    }

    fn publish_configuration_changes(&mut self, next: &Snapshot, advertised_change: bool) {
        let previous = self.snapshots.borrow().clone();
        let grants_or_settings = previous.grants != next.grants
            || previous.settings.plugins != next.settings.plugins
            || previous.peers.len() != next.peers.len()
            || next.peers.iter().any(|peer| {
                previous
                    .peers
                    .iter()
                    .find(|old| old.id == peer.id)
                    .is_none_or(|old| {
                        old.online != peer.online
                            || old.grants != peer.grants
                            || old.plugin_values != peer.plugin_values
                    })
            });
        if !grants_or_settings && !advertised_change {
            return;
        }
        let accepts = self.network.peer_accepts();
        if advertised_change {
            let stopped = self.capture.as_ref().and_then(|key| {
                let peer = key.device.as_ref()?;
                let spec = self
                    .plugins
                    .find(&key.plugin)
                    .and_then(|index| self.plugins.get(index))?;
                let service = &spec.manifest.contributions.input.as_ref()?.source.service;
                let reference = own_streams(spec)
                    .into_iter()
                    .find(|route| &route.service == service)?;
                let was_ready = self
                    .previous_accepts
                    .get(peer)
                    .is_some_and(|routes| routes.contains(&reference));
                let declined = accepts
                    .get(peer)
                    .is_some_and(|routes| !routes.contains(&reference));
                (was_ready && declined).then(|| key.clone())
            });
            if let Some(stopped) = stopped {
                self.halt_capture(
                    &stopped.plugin,
                    stopped.device.as_ref(),
                    peersus_api::message::Outcome::NotAccepted {},
                );
            }
        }
        let mut changed = BTreeSet::new();
        for spec in self.plugins.iter().filter(|spec| {
            spec.manifest.imports.iter().any(|import| {
                import.provider == HOST
                    && !import.events.is_empty()
                    && (import.service == CONFIG_SERVICE
                        || crate::platform_services::EVENTS
                            .iter()
                            .any(|(service, ..)| *service == import.service))
            })
        }) {
            let owned_view = |state: &Snapshot| {
                configuration(
                    spec,
                    state.settings.plugins.get(&spec.id),
                    &self.aliases,
                    &state.peers,
                    state.grants.get(&spec.id),
                )
            };
            let own = own_streams(spec);
            let changed_routes = !own.is_empty()
                && next.peers.iter().any(|peer| {
                    let visible = seen_by(peer, &spec.id).is_some();
                    let held = self.previous_accepts.get(&peer.id);
                    let fresh = accepts.get(&peer.id);
                    visible
                        && own.iter().any(|service| {
                            held.is_some_and(|set| set.contains(service))
                                != fresh.is_some_and(|set| set.contains(service))
                        })
                });
            if (grants_or_settings && owned_view(&previous) != owned_view(next)) || changed_routes {
                changed.insert(spec.id.as_str());
            }
        }
        if !changed.is_empty() {
            let _ = self
                .host
                .publish_host_notice(CONFIG_SERVICE, 1, CONFIG_CHANGED, &changed);
            // The clipboard wake remains useful without a config subscription,
            // but only if that caller's own configuration changed.
            for (service, event, _, _) in crate::platform_services::EVENTS {
                let _ = self.host.publish_host_notice(service, 1, event, &changed);
            }
        }
        self.previous_accepts = accepts;
    }

    /// Hand the windows a state only when it differs from the one they have: the network reports
    /// every keepalive as a change, and a window woken for nothing draws itself for nothing.
    pub(super) fn send_if_changed(&self, next: Snapshot) {
        if **self.snapshots.borrow() == next {
            return;
        }
        self.snapshots.send_replace(Arc::new(next));
    }

    /// File progress and result expiry change no other part of the owner snapshot.
    pub(super) fn publish_actions(&self) {
        let mut next = (**self.snapshots.borrow()).clone();
        next.actions = self.board.statuses();
        self.send_if_changed(next);
    }

    /// The last snapshot with only the page views and the errors brought up to date. What a page
    /// answers arrives as often as a finger moves; the rest of the snapshot did not change with it.
    pub(super) fn publish_pages(&self) {
        let mut next = (**self.snapshots.borrow()).clone();
        next.page_views = self.page_views.snapshot();
        next.kept_layers = self.layers.stamps();
        next.errors = self.errors.clone();
        self.send_if_changed(next);
    }

    /// The last snapshot with only what extensions keep for their drawings brought up to date. A `keep` arrives as often as an
    /// extension has loaded something, and nothing else of the snapshot changed with it.
    pub(super) fn publish_layers(&self) {
        let kept = self.layers.stamps();
        if self.snapshots.borrow().kept_layers == kept {
            return;
        }
        let mut next = (**self.snapshots.borrow()).clone();
        next.kept_layers = kept;
        self.send_if_changed(next);
    }

    /// Say to the plugins that listen what changed in the transfers: what [`crate::transfer_events::to_tell`] decides, and nothing of its own. The
    /// record is the one the windows are shown (read once, in [`Self::publish`]), so a plugin and a window never see two versions of one change.
    pub(super) fn tell_plugins_of_transfers(&self, transfers: &[peersus_core::model::Transfer]) {
        use crate::transfer_events as events;
        let anyone = !consumers_of(&self.plugins, events::TRANSFERS_SERVICE).is_empty();
        let mut told = self.told_transfers.borrow_mut();
        let (to_tell, keep) = events::to_tell(told.as_deref(), transfers, anyone);
        for transfer in to_tell {
            for (event, data) in [
                (events::TRANSFERS_CHANGED, events::changed(transfer)),
                (
                    events::TRANSFERS_CHANGED_NAMED,
                    events::changed_named(transfer),
                ),
            ] {
                // A subscriber that cannot keep up misses this one rather than stalling the application: the hub refuses the whole delivery
                // then, and `active` is always there to ask what is true now. The hub tells each event only to those granted its capability.
                let _ = self.host.submit(peersus_api::message::Operation::Publish {
                    service: events::TRANSFERS_SERVICE.to_owned(),
                    version: 1,
                    event: event.to_owned(),
                    data,
                });
            }
        }
        *told = keep;
    }

    /// Say to the plugins that listen that what a paired device says of itself changed: the event carries nothing, and a plugin asks
    /// `list`, which shows it only the devices it was allowed to see. One payload goes to everyone who listens, so it cannot name a device:
    /// a device one plugin may see is a device another may not. The hub tells it only to those granted `status.watch`.
    pub(super) fn tell_plugins_of_statuses(&self, peers: &[peersus_core::model::Peer]) {
        use crate::status_service as status;
        let anyone = !consumers_of(&self.plugins, status::STATUS_SERVICE).is_empty();
        let mut told = self.told_statuses.borrow_mut();
        let (to_tell, keep) = status::to_tell(told.as_ref(), peers, anyone);
        if !to_tell.is_empty() {
            // A subscriber that cannot keep up misses this one rather than stalling the application: `list` always says what is true now.
            let _ = self.host.submit(peersus_api::message::Operation::Publish {
                service: status::STATUS_SERVICE.to_owned(),
                version: 1,
                event: status::STATUS_CHANGED.to_owned(),
                data: serde_json::json!({}),
            });
        }
        *told = keep;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_projection_hides_the_pressure_setting_and_its_reference_together() {
        let declared: Contributions = serde_json::from_value(serde_json::json!({
            "input": {
                "source": {"service": "carrier", "method": "send"},
                "capability": "carrier.use",
                "edge_pressure": {"type": "setting", "id": "effort"}
            },
            "settings": [
                {"id": "effort", "label": "Effort", "platforms": ["linux"],
                    "kind": {"type": "number", "min": 0, "max": 400, "default": 80}},
                {"id": "show-label", "label": "Show label",
                    "kind": {"type": "toggle", "default": true}}
            ]
        }))
        .unwrap();
        let mut visible = declared.clone();
        project_settings(&mut visible, Some(OperatingSystem::Linux));
        assert_eq!(visible, declared);

        for platform in [
            OperatingSystem::Windows,
            OperatingSystem::Android,
            OperatingSystem::Macos,
        ] {
            let mut projected = declared.clone();
            project_settings(&mut projected, Some(platform));
            let [other] = projected.settings.as_slice() else {
                panic!("only the setting for this platform should remain");
            };
            assert_eq!(other.id, "show-label");
            assert_eq!(projected.input.unwrap().edge_pressure, EdgePressure::Off {});
        }
    }
}
