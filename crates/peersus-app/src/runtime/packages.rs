//! Installing, updating, enabling and removing extension packages, from a file or the store.
use super::*;

impl Runtime {
    /// The store of packages, or the reason there is none.
    pub(super) fn packages(&self) -> Result<&crate::plugin_manager::Manager, &'static str> {
        self.packages
            .as_ref()
            .ok_or("Plugins cannot be installed here")
    }

    /// Stop an installed plugin and keep it from running until it is enabled, also after the
    /// application restarts. It stays installed and keeps what it was granted.
    pub(super) async fn disable_plugin(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        let spec = self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or("Unknown plugin")?;
        // Bundled services have no external folder. Keep their existing in-memory switch;
        // adding a package store must not make their lifecycle depend on a missing folder.
        if let Some(manager) = &self.packages
            && spec.program.is_some()
        {
            manager
                .set_disabled(plugin.as_str(), true)
                .map_err(|refusal| refusal.0)?;
        }
        self.disabled.insert(plugin.clone());
        self.stop_plugin(&plugin).await;
        Ok(())
    }

    /// Let an installed plugin run again, and remember that it is to run at the next start too. It
    /// keeps what it was granted.
    pub(super) fn enable_plugin(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        let spec = self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or("Unknown plugin")?;
        if let Some(manager) = &self.packages
            && spec.program.is_some()
        {
            manager
                .set_disabled(plugin.as_str(), false)
                .map_err(|refusal| refusal.0)?;
        }
        self.disabled.remove(&plugin);
        self.start_if_stopped(&plugin)
    }

    /// Approve a plugin that was found in the store and is not run, as the person was shown it, and start it. The digest
    /// given is the one this owner reported with the plugin; the manager refuses if the program is not that one any more.
    ///
    /// What the plugin had been granted before is forgotten, as for a replacement: a program that was changed is a different
    /// program, and must never start with the rights the old one was given. A plugin approved for the first time has none.
    pub(super) fn approve_plugin(
        &mut self,
        plugin: PluginId,
        program: &str,
    ) -> Result<(), &'static str> {
        let held = self
            .held_back
            .iter()
            .find(|held| held.id == plugin)
            .ok_or("Unknown plugin")?;
        // The owner reported a digest only for what can be approved, and the window can only give back what it was shown:
        // a digest that is not the one reported for this plugin is not what it saw.
        if !matches!(
            held.standing,
            peersus_core::model::Standing::NeedsApproval | peersus_core::model::Standing::Changed
        ) || held.program.as_deref() != Some(program)
        {
            return Err("The plugin is not what was shown; look at it again");
        }
        self.packages()?
            .approve(plugin.as_str(), program)
            .map_err(|refusal| refusal.0)?;
        // The old rights are forgotten before it runs, so it never runs with them.
        self.hub.forget(plugin.as_str());
        self.network
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.standalone
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.sync_host_grants();
        self.sync_plugin_grants();
        let manager = self.packages()?;
        let spec = crate::discovery::load_as(
            manager.store(),
            &manager.store().join(plugin.as_str()),
            plugin.as_str(),
        )?;
        self.plugins
            .register(spec.clone())
            .map_err(|_| "The plugin could not be registered")?;
        self.sync_host_grants();
        self.held_back.retain(|held| held.id != plugin);
        self.start_one(spec);
        Ok(())
    }

    /// Start the plugin if it is not running, without touching what is remembered about whether it
    /// is switched off. What puts a plugin back after a removal the store refused.
    pub(super) fn start_if_stopped(&mut self, plugin: &PluginId) -> Result<(), &'static str> {
        let index = self.plugins.find(plugin).ok_or("Unknown plugin")?;
        let spec = self.plugins.get(index).cloned().ok_or("Unknown plugin")?;
        if self.running.contains_key(plugin) {
            return Ok(());
        }
        self.start_one(spec);
        Ok(())
    }

    /// Take a plugin out: stop it, delete its package, forget everything it was granted in the same
    /// step, and drop it from the registry. A package installed afterwards under the same name
    /// starts with nothing.
    pub(super) async fn remove_package(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        // Refused before anything is touched when there is no store or it is not installed.
        self.packages()?;
        if self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| spec.program.is_none())
        {
            return Err("Bundled plugins cannot be removed");
        }
        // A plugin that was found in the store and is not run (not approved, changed, broken) is not in the registry and has
        // no process, and it is exactly the one a person most needs to remove: so it is known by the list of what was held
        // back, and everything below that is about a running process is skipped for it. The store removes it by the same
        // rule as any other.
        let held_back = self.held_back.iter().any(|held| held.id == plugin);
        if !held_back {
            self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        }
        // The process is stopped FIRST, so that its files are never taken from under it while it
        // runs; then its files, what it was granted, and its place in the registry go.
        let was_running = self.running.contains_key(&plugin);
        self.stop_plugin(&plugin).await;
        if let Err(refusal) = self.packages()?.remove(plugin.as_str()) {
            // The store would not delete it: nothing was removed, so what was stopped to make room
            // for the removal is started again, and a refused "Remove" never leaves the plugin
            // quietly switched off.
            if was_running {
                let _ = self.start_if_stopped(&plugin);
            }
            return Err(refusal.0);
        }
        let _ = self.plugins.remove(&plugin);
        self.held_back.retain(|held| held.id != plugin);
        // The hub keeps what a participant was granted when its process ends, so that a restart
        // finds it. Removing a package is not a restart: what it was granted goes from memory too,
        // or a package installed under the same name in this same run would inherit it.
        self.hub.forget(plugin.as_str());
        // And it is no longer remembered as switched off: a package installed under the same name
        // is a new plugin, and starts as one.
        self.disabled.remove(&plugin);
        // What this person accepted of it goes with it: installing it again asks again, and it can no longer say it was
        // accepted. Failing to write that is not a reason to leave a removed extension installed, so it is not an error.
        let data = self.packages()?.data_directory().to_path_buf();
        let _ = crate::plugin_agreements::forget(&data, plugin.as_str());
        if let Some(data) = self.public_read_directory() {
            crate::platform_services::file_http::FileHttpServices::forget_public_read(
                data, &plugin,
            );
        }
        self.accepted_agreements.remove(plugin.as_str());
        self.network
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.standalone
            .forget_plugin(&plugin)
            .map_err(|_| "Could not forget what it was granted")?;
        self.sync_host_grants();
        self.sync_plugin_grants();
        // Its key bindings go too: a package installed later under the same name starts again from its own suggestions.
        self.change_bindings(|bindings| peersus_core::bindings::forget_plugin(bindings, &plugin))
            .await;
        Ok(())
    }

    /// Change the key bindings as `change` says, when it says it changed them. Failing to save that is said in the log and is not a
    /// reason to undo what caused it (a removal, an installation): the window refuses a binding whose action is gone when it is pressed.
    pub(super) async fn change_bindings(
        &mut self,
        change: impl FnOnce(&mut peersus_core::bindings::Bindings) -> bool,
    ) {
        let mut settings = self.settings.current().clone();
        if change(&mut settings.bindings) && self.apply_settings(settings).await.is_err() {
            tracing::warn!("Key bindings could not be saved");
        }
    }

    /// Read the package in `source` and keep it ready for confirmation. Nothing is installed.
    pub(super) fn stage_install(&mut self, source: &std::path::Path) -> Result<(), &'static str> {
        self.packages()?;
        // A folder is the package itself. A file is an archive of one, unpacked into a private
        // folder that is kept for as long as the package waits, and read from there exactly as a
        // folder chosen by hand is: one set of rules, one set of refusals.
        let unpacked = match std::fs::metadata(source) {
            Ok(found) if found.is_file() => Some(
                crate::archive::unpack_in(source, &self.plugin_directory)
                    .map_err(|refused| refused.0)?,
            ),
            _ => None,
        };
        let source = unpacked.as_ref().map_or(source, |held| held.root());
        let id = crate::plugin_manager::package_id(source).map_err(|refusal| refusal.0)?;
        let plugin = PluginId::new(id).map_err(|_| "The package has no valid name")?;
        if self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| spec.program.is_none())
        {
            return Err("An external package cannot replace a bundled plugin");
        }
        // What it asks for is read now and kept, so that what the person is shown is what is
        // installed if they confirm: confirming names no path and reads nothing again.
        let (manifest, package) = crate::discovery::read_package_and_info(source)?;
        // Refused before the person is shown anything: a package that carries no licence and notice cannot be installed.
        agreement_digest(&manifest)?;
        crate::plugin_host::check_external_execution(manifest.execution)?;
        // And its digest now, so that confirming can tell if anything in the folder has changed
        // since: the person is shown a package, and a package is what they confirm.
        let digest = crate::plugin_manager::package_digest(source).map_err(|refusal| refusal.0)?;
        let source = source.to_path_buf();
        self.staged.insert(
            plugin,
            Staged {
                from_store: false,
                source,
                manifest,
                digest,
                _unpacked: unpacked,
                package,
            },
        );
        Ok(())
    }

    /// Let go of a package that was shown and is not going to be installed. Nothing of it is kept.
    pub(super) fn cancel_install(&mut self, plugin: &PluginId) -> Result<(), &'static str> {
        self.staged
            .remove(plugin)
            .map(|_| ())
            .ok_or("Nothing was shown for that plugin")
    }

    /// Read what each installed package says about itself from its folder, and tell the store which version is
    /// installed under each name. Bounded: at most [`crate::discovery::MAX_PLUGINS`] manifests of at most
    /// [`crate::discovery::MAX_MANIFEST`] bytes. A folder that is broken or not approved still counts: an update
    /// should be offered for it. Called after anything that can change the installed set, never from `publish`.
    pub(super) fn refresh_installed(&mut self) {
        let Some(manager) = &self.packages else {
            return;
        };
        let mut found = std::collections::BTreeMap::new();
        let mut from_store = std::collections::BTreeSet::new();
        if let Ok(entries) = manager.list() {
            for entry in entries.into_iter().take(crate::discovery::MAX_PLUGINS) {
                let Ok(id) = PluginId::new(entry.id.as_str()) else {
                    continue;
                };
                let folder = manager.store().join(&entry.id);
                if crate::plugin_folder::came_from_store(&folder) {
                    from_store.insert(id.clone());
                }
                if let Ok((_, Some(package))) = crate::discovery::read_package_and_info(&folder) {
                    found.insert(id, package);
                }
            }
        }
        self.installed_packages = found;
        self.installed_from_store = from_store;
        self.refresh_public_read_settings();
        if let Some(slot) = self.store.as_mut() {
            let versions = self
                .installed_packages
                .iter()
                .map(|(id, package)| (id.clone(), package.version.clone()))
                .collect();
            slot.service.set_installed(&versions);
        }
    }

    /// The store's state as a window is told it. Without a configured store the store does not exist
    /// here, and the window is told it is not set up.
    pub(super) fn store_state(&self) -> peersus_core::model::StoreState {
        let mut state = match &self.store {
            Some(slot) => slot.service.state().clone(),
            None => peersus_core::model::StoreState {
                status: peersus_core::model::StoreStatus::NotConfigured,
                ..Default::default()
            },
        };
        state.asked = self.store_asked.clone();
        state
    }

    /// A store link asked for `plugin`'s entry: every window is told, with a new number, and acts on it once. When no catalog
    /// is held yet it is fetched, so that the window has the entry to show; an extension the catalog does not list is shown as
    /// the store shows anything missing. Nothing is installed here.
    pub(super) fn show_store_entry(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        let serial = self
            .store_asked
            .as_ref()
            .map_or(1, |asked| asked.serial.wrapping_add(1));
        self.store_asked = Some(peersus_core::model::StoreAsked { serial, plugin });
        let held = self.store.as_ref().map(|slot| slot.service.state().status);
        if matches!(
            held,
            Some(peersus_core::model::StoreStatus::Idle | peersus_core::model::StoreStatus::Failed)
        ) {
            // The window still shows the entry, or why the store has nothing: a refresh that cannot start now is not an error of the link.
            let _ = self.start_store_refresh();
        }
        Ok(())
    }

    /// Application updates as a window is told them. Without a way to apply one they are not set up here.
    pub(super) fn update_state(&self) -> peersus_core::model::UpdateState {
        self.update
            .as_ref()
            .map(|slot| slot.service.state().clone())
            .unwrap_or_default()
    }

    /// Whether nothing a restart would cut short is under way.
    pub(super) fn idle(&self) -> bool {
        self.network.pairing().is_empty()
            && !self
                .network
                .transfers()
                .iter()
                .any(|transfer| transfer.state == TransferState::Running)
    }

    /// Begin looking for a release. Returns at once; [`Self::finish_update`] verifies what was found.
    pub(super) fn start_update_check(&mut self, asked: bool) -> Result<(), &'static str> {
        const UNAVAILABLE: &str = "Application updates are not available here";
        let fetcher = self
            .store
            .as_ref()
            .map(|slot| slot.fetcher.clone())
            .ok_or(UNAVAILABLE)?;
        let slot = self.update.as_mut().ok_or(UNAVAILABLE)?;
        slot.service.begin_check().map_err(|refusal| refusal.0)?;
        slot.schedule.started(tokio::time::Instant::now());
        slot.asked = asked;
        let channel = slot.service.channel();
        self.update_tasks.spawn(async move {
            UpdateOutcome::Checked(crate::app_update::discover(fetcher.as_ref(), channel).await)
        });
        Ok(())
    }

    /// Begin downloading the release the last check verified. What is fetched is the signed
    /// release's. Where applying it ends the application, not while that would cut something short.
    pub(super) fn start_update_install(&mut self, asked: bool) -> Result<(), &'static str> {
        const UNAVAILABLE: &str = "Application updates are not available here";
        let idle = self.idle();
        let fetcher = self
            .store
            .as_ref()
            .map(|slot| slot.fetcher.clone())
            .ok_or(UNAVAILABLE)?;
        let slot = self.update.as_mut().ok_or(UNAVAILABLE)?;
        if slot.installer.ends_application() && !idle {
            return Err("A transfer or a pairing is under way; update when it has finished");
        }
        let fetch = slot.service.begin_download().map_err(|refusal| refusal.0)?;
        slot.asked = asked;
        self.update_tasks.spawn(async move {
            UpdateOutcome::Downloaded(fetcher.fetch(fetch.name, fetch.limit).await)
        });
        Ok(())
    }

    /// End the owner so that the update that is in place runs. Not while that would cut something short.
    pub(super) fn start_restart(&mut self) -> Result<(), &'static str> {
        if !self.idle() {
            return Err("A transfer or a pairing is under way; restart when it has finished");
        }
        let slot = self
            .update
            .as_mut()
            .ok_or("Application updates are not available here")?;
        slot.service.begin_restart().map_err(|refusal| refusal.0)?;
        self.restart = Some(tokio::time::Instant::now() + RESTART_NOTICE);
        Ok(())
    }

    /// One step of an update finished. Everything that trusts happens here; the installer is given
    /// only bytes that are exactly what the signed release pinned, and runs away from the loop.
    pub(super) fn finish_update(&mut self, outcome: UpdateOutcome) {
        let reopen = self.settings.current().restart_after_update;
        let Some(slot) = self.update.as_mut() else {
            return;
        };
        let failed = match outcome {
            UpdateOutcome::Checked(found) => {
                let result = slot.service.finish_check(found);
                slot.schedule
                    .finished(result.is_ok(), tokio::time::Instant::now());
                // Each check allows one automatic attempt: an install that failed (the network went
                // away mid-download) is tried again after the next check, not in a loop before it.
                slot.tried = None;
                result.err().map(|refusal| refusal.0)
            }
            UpdateOutcome::Downloaded(bytes) => match slot.service.finish_download(bytes) {
                Ok(verified) => {
                    let installer = slot.installer.clone();
                    self.update_tasks.spawn_blocking(move || {
                        UpdateOutcome::Installed(installer.install(verified, reopen))
                    });
                    None
                }
                Err(refusal) => Some(refusal.0),
            },
            UpdateOutcome::Installed(Ok(())) => {
                slot.service.installed();
                None
            }
            UpdateOutcome::Installed(Err(words)) => {
                slot.service.failed(words);
                Some(words)
            }
        };
        if let Some(words) = failed
            && slot.asked
        {
            self.error(words);
        }
    }

    /// What the application does about its own updates without being asked: look for a release
    /// now and then; with the person's first switch on, put a verified one in place; with the
    /// second, start again once that is done. One step at a time, and never over a transfer.
    pub(super) fn automatic_update(&mut self) {
        if !self.update_tasks.is_empty() || self.restart.is_some() || self.store.is_none() {
            return;
        }
        use peersus_core::model::UpdateStatus;
        enum Step {
            Restart,
            Install,
            Check,
        }
        let settings = self.settings.current();
        let (install, restart) = (settings.update_automatically, settings.restart_after_update);
        let idle = self.idle();
        let Some(slot) = self.update.as_mut() else {
            return;
        };
        let step = if restart && slot.service.waiting_for_restart() {
            idle.then_some(Step::Restart)
        } else if install
            && slot.service.available()
            && slot.tried != slot.service.state().offered
            && (idle || !slot.installer.ends_application())
        {
            slot.tried.clone_from(&slot.service.state().offered);
            Some(Step::Install)
        } else if matches!(
            slot.service.state().status,
            UpdateStatus::Idle
                | UpdateStatus::Current
                | UpdateStatus::Available
                | UpdateStatus::Failed
        ) && slot.schedule.due(tokio::time::Instant::now())
        {
            Some(Step::Check)
        } else {
            None
        };
        let started = match step {
            Some(Step::Restart) => self.start_restart().is_ok(),
            Some(Step::Install) => self.start_update_install(false).is_ok(),
            Some(Step::Check) => self.start_update_check(false).is_ok(),
            None => false,
        };
        if started {
            self.publish();
        }
    }

    /// Fetch what languages need, if anything: the offer of this release when it is not held, the file of the language
    /// wanted when it is offered and not held, or the one `asked` for. One fetch at a time, in a task; [`crate::languages`]
    /// checks and keeps what it brings on the loop.
    pub(super) fn automatic_languages(&mut self, asked: Option<Language>) {
        if !self.language_tasks.is_empty() {
            return;
        }
        let Some(fetcher) = self.store.as_ref().map(|slot| slot.fetcher.clone()) else {
            return;
        };
        let chosen = self.settings.current().language;
        let Some(languages) = self.languages.as_mut() else {
            return;
        };
        let Some(wanted) = languages.due(chosen, asked, tokio::time::Instant::now()) else {
            return;
        };
        self.language_tasks.spawn(async move {
            crate::languages::fetch(fetcher.as_ref(), crate::languages::RELEASE, wanted).await
        });
        self.publish();
    }

    pub(super) fn auto_store_refresh(&mut self) {
        let due = self.store.as_ref().is_some_and(|slot| {
            slot.fetcher.branch_backed()
                && slot.service.state().status != peersus_core::model::StoreStatus::NotConfigured
                && slot.refresh.due(tokio::time::Instant::now())
        });
        if due && self.store_tasks.is_empty() && self.start_store_refresh().is_ok() {
            self.publish();
        }
    }

    /// Begin fetching the catalog. Returns at once: the download runs in a task and its result is
    /// verified on the event loop by [`Self::finish_store`], so a slow network never stalls the owner.
    pub(super) fn start_store_refresh(&mut self) -> Result<(), &'static str> {
        let slot = self
            .store
            .as_mut()
            .ok_or("The extension store is not available here")?;
        let branch_backed = slot.fetcher.branch_backed();
        if branch_backed && !slot.refresh.due(tokio::time::Instant::now()) {
            return Ok(());
        }
        slot.service.begin_refresh().map_err(|refusal| refusal.0)?;
        slot.refresh.started(tokio::time::Instant::now());
        let fetcher = slot.fetcher.clone();
        let cached = slot.service.discovery_cache();
        self.store_tasks.spawn(async move {
            if branch_backed {
                return StoreOutcome::Discovered(
                    crate::store_branch::discover(fetcher.as_ref(), cached.as_deref()).await,
                );
            }
            let fetched = async {
                let catalog = fetcher
                    .fetch(
                        crate::store::CATALOG_FILE.to_owned(),
                        crate::catalog::MAX_CATALOG_BYTES,
                    )
                    .await?;
                let signature = fetcher
                    .fetch(crate::store::SIGNATURE_FILE.to_owned(), 64)
                    .await?;
                Ok((catalog, signature))
            };
            StoreOutcome::Refreshed(fetched.await)
        });
        Ok(())
    }

    /// Begin downloading the approved package for `plugin`. What is fetched is the signed entry's, never
    /// the command's. Refused before anything is fetched when packages cannot be installed here.
    pub(super) fn start_store_install(&mut self, plugin: PluginId) -> Result<(), &'static str> {
        self.packages()?;
        if self
            .plugins
            .find(&plugin)
            .and_then(|index| self.plugins.get(index))
            .is_some_and(|spec| spec.program.is_none())
        {
            return Err("An external package cannot replace a bundled plugin");
        }
        // A package the person staged by hand for this name is waiting on THEIR confirmation. A store
        // install must not silently replace it (the staged card would then install something else).
        if self.staged.contains_key(&plugin) {
            return Err(
                "A package for this name is waiting to be confirmed; confirm or cancel it first",
            );
        }
        let slot = self
            .store
            .as_mut()
            .ok_or("The extension store is not available here")?;
        let fetch = slot
            .service
            .begin_install(&plugin)
            .map_err(|refusal| refusal.0)?;
        let fetcher = slot.fetcher.clone();
        self.store_tasks.spawn(async move {
            StoreOutcome::Downloaded {
                plugin,
                bytes: fetcher.fetch(fetch.archive, fetch.limit).await,
            }
        });
        Ok(())
    }

    /// A download finished. Everything that trusts or installs happens here, on the event loop. Each
    /// branch computes one result and there is ONE exit that releases the store, so a failure can never
    /// leave it reading as busy. `self.store` is never taken out: an install awaits and may publish, and
    /// a snapshot published meanwhile must still carry the store's real state.
    pub(super) async fn finish_store(&mut self, outcome: StoreOutcome) {
        let scratch = self.plugin_directory.clone();
        let Some(slot) = self.store.as_mut() else {
            return;
        };
        let refreshing = matches!(
            &outcome,
            StoreOutcome::Refreshed(_) | StoreOutcome::Discovered(_)
        );
        let result: Result<(), &'static str> = match outcome {
            StoreOutcome::Discovered(Ok(index)) => slot
                .service
                .finish_discovery(index)
                .map_err(|refusal| refusal.0),
            StoreOutcome::Refreshed(Ok((catalog, signature))) => slot
                .service
                .finish_refresh(&catalog, &signature)
                .map_err(|refusal| refusal.0),
            StoreOutcome::Refreshed(Err(refusal))
            | StoreOutcome::Discovered(Err(refusal))
            | StoreOutcome::Downloaded {
                bytes: Err(refusal),
                ..
            } => {
                slot.service.failed(refusal.0);
                Err(refusal.0)
            }
            StoreOutcome::Downloaded {
                plugin,
                bytes: Ok(bytes),
            } => {
                match slot.service.finish_install(&plugin, bytes, &scratch) {
                    // The service already recorded and released the failure.
                    Err(refusal) => Err(refusal.0),
                    // The ordinary confirm path: replacement, rollback and forgetting grants all live there.
                    Ok(verified) => {
                        let installed = self.confirm_verified(plugin, verified).await;
                        match (self.store.as_mut(), installed) {
                            (Some(slot), Ok(())) => {
                                slot.service.done();
                                Ok(())
                            }
                            (Some(slot), Err(reason)) => {
                                slot.service.failed(reason);
                                Err(reason)
                            }
                            (None, outcome) => outcome,
                        }
                    }
                }
            }
        };
        if refreshing && let Some(slot) = self.store.as_mut() {
            slot.refresh
                .finished(result.is_ok(), tokio::time::Instant::now());
        }
        // Every outcome can change what is installed or what is listed: a refresh adopts new listings that need
        // their installed versions, and an install changes one. Read once here, off the publish path.
        self.refresh_installed();
        if refreshing {
            self.auto_update_tried.clear();
        }
        if let Err(reason) = result {
            self.error(reason);
        }
        self.start_automatic_update();
    }

    /// Start the next opted-in store update: a newer signed version, or the same version for an
    /// installed copy that is damaged or changed. One at a time; the next starts when it finishes.
    pub(super) fn start_automatic_update(&mut self) {
        let chosen = &self.settings.current().auto_update;
        let Some(slot) = self.store.as_ref() else {
            return;
        };
        if chosen.is_empty()
            || slot.service.state().status != peersus_core::model::StoreStatus::Ready
            || !self.store_tasks.is_empty()
        {
            return;
        }
        let damaged = |plugin: &PluginId| {
            self.held_back.iter().any(|held| {
                &held.id == plugin
                    && matches!(
                        held.standing,
                        peersus_core::model::Standing::Broken
                            | peersus_core::model::Standing::Changed
                    )
            })
        };
        let next = slot.service.state().listings.iter().find_map(|listing| {
            let plugin = &listing.plugin;
            let wanted = chosen.contains(plugin)
                && !self.auto_update_tried.contains(plugin)
                && !self.staged.contains_key(plugin)
                && listing.installed_version.as_deref().is_some_and(|have| {
                    use peersus_api::package::Version;
                    damaged(plugin)
                        || matches!(
                            (Version::parse(have), Version::parse(&listing.version)),
                            (Some(have), Some(offered)) if offered > have
                        )
                });
            wanted.then(|| plugin.clone())
        });
        if let Some(plugin) = next {
            self.auto_update_tried.insert(plugin.clone());
            if let Err(reason) = self.start_store_install(plugin) {
                self.error(reason);
            }
            self.publish();
        }
    }

    /// Install what the store verified through [`Self::confirm_install`]. The click on Install was the confirmation of
    /// the package; the licences and notice are a separate decision, the person's own. They go through unasked ONLY when
    /// this person already accepted exactly this agreement (an update that changed no word of it); otherwise the verified
    /// package is kept staged, as a package chosen by hand is, and installs when they have read it and accepted.
    pub(super) async fn confirm_verified(
        &mut self,
        plugin: PluginId,
        verified: crate::store_service::Verified,
    ) -> Result<(), &'static str> {
        let source = verified.unpacked.root().to_path_buf();
        let (manifest, package) = crate::discovery::read_package_and_info(&source)?;
        crate::plugin_host::check_external_execution(manifest.execution)?;
        let agreed = agreement_digest(&manifest)?;
        let digest = crate::plugin_manager::package_digest(&source).map_err(|refusal| refusal.0)?;
        // The unpacked folder moves into the staged entry so it lives until the install has finished.
        self.staged.insert(
            plugin.clone(),
            Staged {
                from_store: true,
                source,
                manifest,
                digest,
                _unpacked: Some(verified.unpacked),
                package,
            },
        );
        if self.accepted_agreements.get(plugin.as_str()) == Some(&agreed) {
            self.confirm_install(plugin, Some(agreed), false).await
        } else {
            self.error(
                "An extension needs you to read and accept its licence and notice; open Extensions",
            );
            Ok(())
        }
    }

    /// Install the package that was staged for `plugin`, and only that one, and start it. No restart is needed. What it may do
    /// is [`peersus_core::plugins::Installation`]'s rule: a store update keeps what the person allowed that it still asks for;
    /// anything else starts with no permissions. `agreement` is the digest of the licences and notice the
    /// person accepted: the package is installed only if it is the agreement of this very package, and the acceptance
    /// is recorded before anything is put in place, and undone if nothing is.
    pub(super) async fn confirm_install(
        &mut self,
        plugin: PluginId,
        agreement: Option<String>,
        risk_acknowledged: bool,
    ) -> Result<(), &'static str> {
        let staged = self
            .staged
            .remove(&plugin)
            .ok_or("Nothing was shown for that plugin")?;
        // Taken out first, so that a refusal does not leave it waiting to be confirmed again as
        // if nothing had happened: the person has to choose it again and be shown it again.
        if !crate::plugin_manager::still_what_was_shown(
            &staged.digest,
            crate::plugin_manager::package_digest(&staged.source),
        ) {
            return Err("The package changed after it was shown; choose it again");
        }
        let agreed = agreement_digest(&staged.manifest)?;
        if agreement.as_deref() != Some(agreed.as_str()) {
            return Err("The licence and notice were not accepted, so nothing was installed");
        }
        // The application's warning about a package from outside the store is accepted once (`outside_store_risk_accepted`); until then
        // the package is installed only if the window says it was read to its end and held.
        if !staged.from_store
            && !risk_acknowledged
            && !self.settings.current().outside_store_risk_accepted
        {
            return Err(
                "A package from outside the store is installed only after its warning was read to the end and held to confirm",
            );
        }
        let (source, from_store) = (staged.source, staged.from_store);
        let data = self.packages()?.data_directory().to_path_buf();
        // Recorded first and undone on a refusal, so that an installed extension never lacks the record of what was
        // accepted for it, and a refused one never gains one.
        let before = self.accepted_agreements.get(plugin.as_str()).cloned();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        crate::plugin_agreements::accept(&data, plugin.as_str(), &agreed, now)
            .map_err(|_| "Your acceptance could not be recorded, so nothing was installed")?;
        self.accepted_agreements
            .insert(plugin.as_str().to_owned(), agreed);
        // A name that is installed is REPLACED: the old plugin is taken out first (stopped, its files kept aside until the new
        // one is in), by the same steps as `remove_package`, so there is one way a plugin goes and no second one to drift.
        // What it was granted goes with it, unless this is a store update (`Installation`): then what the new version still
        // asks for is kept, and the rest forgotten, before the new one starts.
        let held_back = self.held_back.iter().any(|held| held.id == plugin);
        let replaced = self.plugins.find(&plugin).is_some() || held_back;
        let installation = {
            let folder = self.packages()?.store().join(plugin.as_str());
            let present = replaced.then(|| peersus_core::plugins::Present {
                from_store: crate::plugin_folder::came_from_store(&folder),
                // What the folder says now, not what was registered: a package changed under a running plugin is not sound.
                sound: !held_back
                    && matches!(
                        crate::plugin_folder::look(&folder).state,
                        crate::plugin_state::State::Ready | crate::plugin_state::State::Disabled
                    ),
                version: self
                    .installed_packages
                    .get(&plugin)
                    .map(|package| package.version.as_str()),
            });
            peersus_core::plugins::Installation::of(
                present,
                from_store,
                staged
                    .package
                    .as_ref()
                    .map(|package| package.version.as_str()),
            )
        };
        // The switch is the person's and not the old package's: an extension they switched off is
        // replaced (an automatic update above all) without being started, and stays off.
        let switched_off = replaced && self.disabled.contains(&plugin);
        let kept_files = if replaced {
            match self
                .set_aside_installed(&plugin, installation.keeps_grants())
                .await
            {
                Ok(directory) => Some(directory),
                Err(refusal) => {
                    self.restore_acceptance(&data, &plugin, before);
                    return Err(refusal);
                }
            }
        } else {
            None
        };
        match self.install_and_start(&plugin, &source, switched_off, from_store, installation) {
            Ok(()) => {
                // The old package is gone for good only now that the new one is in.
                if let Some(directory) = kept_files {
                    let _ = std::fs::remove_dir_all(directory);
                }
                // A new extension's suggested keys are bound where they are free; an update keeps what the person made of them.
                if !replaced && !cfg!(target_os = "android") {
                    let suggested: Vec<(String, peersus_api::chord::Chord)> = self
                        .plugins
                        .find(&plugin)
                        .and_then(|index| self.plugins.get(index))
                        .map(|spec| {
                            spec.manifest
                                .contributions
                                .actions
                                .iter()
                                .filter(|action| action.bindable() && !action.needs_device)
                                .filter_map(|action| Some((action.id.clone(), action.chord?)))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.change_bindings(|bindings| {
                        peersus_core::bindings::bind_suggested(bindings, &plugin, suggested)
                    })
                    .await;
                }
                Ok(())
            }
            Err(refusal) => {
                // Not installed: the old plugin is put back exactly as it was, and started again
                // unless it was switched off.
                if let Some(directory) = kept_files {
                    self.put_back_installed(&plugin, &directory, switched_off);
                }
                self.restore_acceptance(&data, &plugin, before);
                Err(refusal)
            }
        }
    }

    /// Put what was accepted for `plugin` back as it was before an installation that did not happen.
    pub(super) fn restore_acceptance(
        &mut self,
        data: &std::path::Path,
        plugin: &PluginId,
        before: Option<String>,
    ) {
        match before {
            Some(digest) => {
                let _ = crate::plugin_agreements::accept(data, plugin.as_str(), &digest, 0);
                self.accepted_agreements
                    .insert(plugin.as_str().to_owned(), digest);
            }
            None => {
                let _ = crate::plugin_agreements::forget(data, plugin.as_str());
                self.accepted_agreements.remove(plugin.as_str());
            }
        }
    }

    /// Install the package in `source` as `plugin`, register it and start it; or, when the person
    /// had switched off the one it replaces, leave it switched off. `from_store` is recorded in its folder. On a store update
    /// (`installation`), what the person allowed under the name is narrowed to what the new version asks for BEFORE it is
    /// registered or runs; anything else under the name was already forgotten by [`Self::set_aside_installed`].
    pub(super) fn install_and_start(
        &mut self,
        plugin: &PluginId,
        source: &std::path::Path,
        switched_off: bool,
        from_store: bool,
        installation: peersus_core::plugins::Installation,
    ) -> Result<(), &'static str> {
        let manager = self.packages()?;
        if from_store {
            manager.install_from_store(source)
        } else {
            manager.install(source)
        }
        .map_err(|refusal| refusal.0)?;
        if switched_off {
            // Written into the new folder so that it holds after a restart too. A marker that could
            // not be written does not undo an installation that succeeded: it is off for this run.
            let _ = manager.set_disabled(plugin.as_str(), true);
        }
        let spec = crate::discovery::load_as(
            manager.store(),
            &manager.store().join(plugin.as_str()),
            plugin.as_str(),
        )?;
        if installation.keeps_grants() {
            let asked = super::grantable(&spec);
            let narrowed = self
                .network
                .narrow_plugin(plugin, &asked)
                .and_then(|_| self.standalone.narrow_plugin(plugin, &asked));
            // Fails closed: what cannot be narrowed is forgotten, as for any replacement; and if even that cannot be written,
            // the new package does not start, and the caller puts the old one back.
            if narrowed.is_err() {
                self.network
                    .forget_plugin(plugin)
                    .and_then(|_| self.standalone.forget_plugin(plugin))
                    .map_err(|_| "Could not forget what it was granted")?;
            }
        }
        self.plugins
            .register(spec.clone())
            .map_err(|_| "The plugin could not be registered")?;
        self.held_back.retain(|held| &held.id != plugin);
        self.sync_host_grants();
        self.sync_plugin_grants();
        if switched_off {
            self.disabled.insert(plugin.clone());
        } else {
            self.start_one(spec);
        }
        Ok(())
    }

    /// The first half of a replacement: the plugin that is installed under this name is stopped,
    /// taken out of the store but KEPT, dropped from the registry, and everything it was granted
    /// forgotten, by the same steps as a removal; except, on a store update (`keep_grants`), what the person allowed, which
    /// [`Self::install_and_start`] narrows to what the new version asks for. Returns where its files are kept.
    pub(super) async fn set_aside_installed(
        &mut self,
        plugin: &PluginId,
        keep_grants: bool,
    ) -> Result<PathBuf, &'static str> {
        self.stop_plugin(plugin).await;
        let kept = match self.packages()?.retire(plugin.as_str()) {
            Ok(kept) => kept,
            Err(refusal) => {
                // Nothing was taken out, so what was stopped to make room is started again.
                let _ = self.start_if_stopped(plugin);
                return Err(refusal.0);
            }
        };
        let _ = self.plugins.remove(plugin);
        self.hub.forget(plugin.as_str());
        self.disabled.remove(plugin);
        // A failure to forget what it was granted is a failure of the replacement, and the old one
        // is put back by the caller: a new plugin must never start with the old one's rights. Out of the registry, what is
        // kept for a store update is held by nothing until the new version is narrowed and registered.
        let forgot = if keep_grants {
            Ok(0)
        } else {
            self.network
                .forget_plugin(plugin)
                .and_then(|_| self.standalone.forget_plugin(plugin))
        };
        self.sync_host_grants();
        self.sync_plugin_grants();
        forgot
            .map(|_| kept)
            .map_err(|_| "Could not forget what it was granted")
    }

    /// The second half, when the new package could not go in: the old plugin is put back exactly as
    /// it was, registered again and started again. A replacement forgot what it had been granted, and that is not
    /// restored: a failed replacement leaves it with nothing allowed, never with more than it had. A store update kept it,
    /// narrowed at most, so it comes back with no more than it had. One that was switched off comes back listed and switched
    /// off, as its folder says.
    pub(super) fn put_back_installed(
        &mut self,
        plugin: &PluginId,
        kept: &std::path::Path,
        switched_off: bool,
    ) {
        let Ok(manager) = self.packages() else { return };
        if manager.restore(plugin.as_str(), kept).is_err() {
            return;
        }
        let state = crate::plugin_folder::look(&manager.store().join(plugin.as_str())).state;
        let off = switched_off && state == crate::plugin_state::State::Disabled;
        if !(state.may_run() || off) {
            return;
        }
        let Ok(spec) = crate::discovery::load_as(
            manager.store(),
            &manager.store().join(plugin.as_str()),
            plugin.as_str(),
        ) else {
            return;
        };
        if self.plugins.register(spec).is_ok() {
            self.sync_host_grants();
            self.sync_plugin_grants();
            if off {
                self.disabled.insert(plugin.clone());
            } else {
                let _ = self.start_if_stopped(plugin);
            }
        }
    }
}
