//! Calls into extensions and from them: invoked actions, input, pages, deliveries the hub hands the application, and sends.
use super::*;

pub(super) const UNKNOWN_PAGE: &str = "Unknown page";

/// What the owner must do *after* replying. A read never invalidates the window or peer admission.
pub(super) enum AfterReply {
    None,
    Publish,
    Restart(PluginId),
}

impl Runtime {
    pub(super) fn invoke_selected(
        &mut self,
        plugin: PluginId,
        action: String,
        path: Option<PathBuf>,
        held: Option<(u64, String)>,
    ) -> Result<(), &'static str> {
        let key = Key {
            plugin: plugin.clone(),
            action: action.clone(),
            device: None,
        };
        let result = (|| {
            let services = self
                .platform_services
                .clone()
                .ok_or("Selected input is unavailable")?;
            let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
            let declaration = self
                .plugins
                .action(index, &action)
                .ok_or("Unknown action")?;
            if declaration.input != peersus_api::contributions::ActionInput::SelectedFile {
                return Err("This action does not accept selected input");
            }
            let input = match (path, held) {
                (Some(path), None) => crate::platform_services::SelectedInput::Path(path),
                (None, Some((number, name))) => crate::platform_services::SelectedInput::Held {
                    registry: self.held.clone(),
                    number,
                    name,
                },
                _ => return Err("Choose exactly one file for this action"),
            };
            let context = services.select_for_action(&plugin, input, &action)?;
            if let Err(error) = self.invoke(plugin.clone(), &action, None, Some(context)) {
                services.discard_selection(&plugin, context);
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(reason = error, "Selected action refused before admission");
            self.board.rejected(key);
        }
        result
    }

    /// Run an action a plugin declared, on behalf of the user.
    ///
    /// Two independent layers decide. Here, the *device* layer: the user must have granted
    /// this plugin that capability for the chosen device, and the device must be online.
    /// Then the hub, the *plugin* layer: the plugin must list the application as a
    /// consumer of that export and the capability must be granted to the application.
    /// The device is data inside the call, never the identity it is made under.
    ///
    /// The reply acknowledges that the call was admitted. The plugin's answer arrives
    /// later and, if it is an error, is published as a fixed-text message.
    pub(super) fn invoke(
        &mut self,
        plugin: PluginId,
        action: &str,
        peer: Option<PeerId>,
        context: Option<u64>,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let action = self
            .plugins
            .action(index, action)
            .ok_or("Unknown action")?
            .clone();
        // What its own extension took back is not offered, and a press that was already on its way is refused the same.
        if self.page_offers.withholds(&plugin, &action.id) {
            return Err("This action is not available now");
        }
        let stages_input = matches!(
            action.input,
            peersus_api::contributions::ActionInput::SelectedFile
                | peersus_api::contributions::ActionInput::SharedText
        );
        if stages_input != context.is_some() {
            return Err("This action requires its declared selected input");
        }
        let device = match (action.needs_device, peer) {
            (true, Some(peer)) => {
                let peer = self.peer(&peer).ok_or("Device is not paired")?;
                if !permitted(&peer, &plugin, action.capability.as_deref()) {
                    return Err("Action is not permitted for this device");
                }
                if self.network.connection(&peer.id).is_none() {
                    return Err("Device is offline");
                }
                self.opened_by_person(&peer.id, Some(&plugin));
                Some(peer.id)
            }
            (true, None) => return Err("Choose a device for this action"),
            (false, None) => None,
            (false, Some(_)) => return Err("This action does not use a device"),
        };
        let version = self
            .plugins
            .get(index)
            .and_then(|spec| {
                spec.manifest
                    .exports
                    .iter()
                    .find(|e| e.service == action.target.service)
            })
            .map(|export| export.version)
            .ok_or("Unknown action")?;
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        let mut args = match &device {
            Some(peer) => peer_argument(&self.aliases, &plugin, peer),
            None => serde_json::Value::Null,
        };
        if let Some(context) = context {
            if args.is_null() {
                args = serde_json::json!({});
            }
            args.as_object_mut()
                .ok_or("Action arguments are invalid")?
                .insert("context".into(), serde_json::json!(context));
        }
        // Registered before it is sent, so a second press made while this one is still
        // running is already seen as a repeat. The key is the action the user pressed, not
        // the method it happens to call: two actions may share a method.
        let key = Key {
            plugin: plugin.clone(),
            action: action.id.clone(),
            device: device.clone(),
        };
        // The outbound boundary where a user action becomes a broker call: admission
        // refusals, submit failures and dispatch all carry the same call number.
        let dispatch = |stage: &'static str, outcome: &'static str| {
            peersus_diagnostics::dev_trace!(
                stage,
                call = id,
                outcome,
                "Plugin action dispatch decided"
            );
        };
        self.in_flight.start(key.clone(), id).map_err(|refused| {
            let (sentence, outcome) = match refused {
                Refused::Pending => ("That action is already running", "pending"),
                Refused::Full => ("Too many operations are pending", "full"),
            };
            dispatch("admission", outcome);
            sentence
        })?;
        self.board.running(key);
        let sent = self.host.submit(peersus_api::message::Operation::Call {
            id,
            provider: plugin.as_str().to_owned(),
            service: action.target.service,
            version,
            method: action.target.method,
            args,
            timeout_ms: action.timeout_ms,
        });
        if let Err(error) = sent {
            dispatch("submit-failed", peersus_diagnostics::refusal(&error));
            // The hub never recorded this call, so no result will ever come to close it.
            // Left registered, the action could never be started again.
            // The board must not be left saying "running" for a call that never started.
            if let Some(key) = self.in_flight.finish_key(id) {
                self.board.finished(
                    &key,
                    ActionState::Failed {
                        reason: ActionFailure::Unavailable,
                    },
                );
            }
            return Err(refusal_text(&error));
        }
        dispatch("dispatched", "ok");
        Ok(())
    }

    /// Validated platform input, queued and sent to the extension's declared transport.
    pub(super) fn forward_input(
        &mut self,
        plugin: PluginId,
        peer: PeerId,
        events: Vec<serde_json::Value>,
        end: bool,
    ) -> Result<(), &'static str> {
        use peersus_api::contributions::{MAX_POINTER_BATCH, PointerInput};
        const NOT_UNDERSTOOD: &str = "That input was not understood";
        if events.len() > MAX_POINTER_BATCH {
            return Err(NOT_UNDERSTOOD);
        }
        let events = events
            .into_iter()
            .map(|event| {
                let event: PointerInput =
                    serde_json::from_value(event).map_err(|_| NOT_UNDERSTOOD)?;
                event.validate().map_err(|_| NOT_UNDERSTOOD)?;
                Ok(event)
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        let index = self.plugins.find(&plugin).ok_or("Unknown plugin")?;
        let declared = self
            .plugins
            .get(index)
            .and_then(|spec| spec.manifest.contributions.input.as_ref())
            .cloned()
            .ok_or("This extension declares no input transport")?;
        let source = declared.source;
        let peer = self.peer(&peer).ok_or("Device is not paired")?;
        if !permitted(&peer, &plugin, Some(&declared.capability)) {
            return Err("This is not permitted for this device");
        }
        if self.network.connection(&peer.id).is_none() {
            return Err("Device is offline");
        }
        self.opened_by_person(&peer.id, Some(&plugin));
        let device = Some(peer.id);
        let key = crate::surfaces::SurfaceKey { plugin, device };
        if let Some(batch) = self.surfaces.offer(&key, events, end)? {
            self.send_surface_batch(&key, &source, batch)?;
        }
        Ok(())
    }

    /// One call carrying a batch of surface input: `{"peer": .., "events": [..], "end": .., "at": ..}`.
    pub(super) fn send_surface_batch(
        &mut self,
        key: &crate::surfaces::SurfaceKey,
        source: &peersus_api::contributions::Source,
        batch: crate::surfaces::Batch,
    ) -> Result<(), &'static str> {
        let index = self.plugins.find(&key.plugin).ok_or("Unknown plugin")?;
        let version = self
            .plugins
            .get(index)
            .and_then(|spec| {
                spec.manifest
                    .exports
                    .iter()
                    .find(|e| e.service == source.service)
            })
            .map(|export| export.version)
            .ok_or("Unknown page")?;
        let mut args = match &key.device {
            Some(peer) => peer_argument(&self.aliases, &key.plugin, peer),
            None => serde_json::json!({}),
        };
        let object = args.as_object_mut().ok_or("Input arguments are invalid")?;
        object.insert(
            "events".into(),
            serde_json::to_value(&batch.events).map_err(|_| "Input arguments are invalid")?,
        );
        object.insert("end".into(), serde_json::json!(batch.end));
        // One monotonic clock of this owner, so an extension can pace a long stream (rotate a connection before the other
        // side's deadline) without a clock of its own. Only differences between two of these mean anything.
        object.insert("at".into(), serde_json::json!(owner_clock()));
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        self.surfaces.sent(key, id);
        let sent = self.host.submit(peersus_api::message::Operation::Call {
            id,
            provider: key.plugin.as_str().to_owned(),
            service: source.service.clone(),
            version,
            method: source.method.clone(),
            args,
            // Input that has waited this long is no longer what the person is doing.
            timeout_ms: 5_000,
        });
        if let Err(error) = sent {
            self.surfaces.unsent(key);
            return Err(refusal_text(&error));
        }
        Ok(())
    }

    /// What a person did on a page an extension shows: checked, queued behind what is still out for
    /// that page, and sent to the page's source.
    pub(super) fn page_event(
        &mut self,
        plugin: PluginId,
        page: String,
        event: peersus_api::elements::Event,
    ) -> Result<(), &'static str> {
        event
            .validate()
            .map_err(|_| "That input was not understood")?;
        self.page_source(&plugin, &page)?;
        self.person_is_on(&plugin);
        let key = crate::page_views::PageKey { plugin, page };
        // A window names a chosen device by the owner's key, which an extension never learns: it is told the alias it knows the
        // device by, and only of a device the chooser offers as it is shown now.
        let event = match event {
            peersus_api::elements::Event::Choose { id, device } => {
                if !self.page_views.choosable(&key, &id, device.as_deref()) {
                    return Err("That input was not understood");
                }
                if let Some(device) = &device {
                    // Choosing a device on an extension's page is the person's act for that device, through that extension.
                    self.opened_by_person(
                        &peersus_core::model::PeerId(device.clone()),
                        Some(&key.plugin),
                    );
                }
                peersus_api::elements::Event::Choose {
                    device: device.map(|device| {
                        self.aliases
                            .alias(&key.plugin, &peersus_core::model::PeerId(device))
                    }),
                    id,
                }
            }
            other => other,
        };
        if let Some(event) = self
            .page_views
            .offer(&key, event, std::time::Instant::now())?
        {
            self.send_page_event(&key, event)?;
        }
        Ok(())
    }

    /// The source of an extension page whose content the extension gives, offered on this system,
    /// and the version of the service it is in.
    pub(super) fn page_source(
        &self,
        plugin: &PluginId,
        page: &str,
    ) -> Result<(peersus_api::contributions::Source, u32), &'static str> {
        use peersus_api::contributions::Surface;
        let spec = self
            .plugins
            .find(plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or("Unknown plugin")?;
        let declared = spec
            .manifest
            .contributions
            .pages
            .iter()
            .chain(self.page_offers.pages(plugin))
            .find(|p| p.id == page && p.surface == Some(Surface::Elements))
            .ok_or(UNKNOWN_PAGE)?;
        let here = peersus_api::platform::OperatingSystem::current();
        if !declared.platforms.is_empty()
            && !here.is_some_and(|os| declared.platforms.contains(&os))
        {
            return Err("This page is not offered on this system");
        }
        let source = declared.source.clone().ok_or(UNKNOWN_PAGE)?;
        let version = spec
            .manifest
            .exports
            .iter()
            .find(|e| e.service == source.service)
            .map(|export| export.version)
            .ok_or(UNKNOWN_PAGE)?;
        Ok((source, version))
    }

    pub(super) fn send_page_event(
        &mut self,
        key: &crate::page_views::PageKey,
        event: peersus_api::elements::Event,
    ) -> Result<(), &'static str> {
        let (source, version) = self.page_source(&key.plugin, &key.page)?;
        self.next_call = self.next_call.checked_add(1).ok_or("Too many calls")?;
        let id = self.next_call;
        self.page_views.sent(key, id);
        let sent = self.host.submit(peersus_api::message::Operation::Call {
            id,
            provider: key.plugin.as_str().to_owned(),
            service: source.service,
            version,
            method: source.method,
            // The page is named as well: one source can draw the same content on more than one page
            // (see `page_offers`), each of its own size, and only the extension can keep them apart.
            args: serde_json::json!({ "event": event, "page": key.page }),
            // A page that takes longer than this to answer a press is not answering.
            timeout_ms: 5_000,
        });
        if let Err(error) = sent {
            self.page_views.unsent(key);
            return Err(refusal_text(&error));
        }
        Ok(())
    }

    /// Deal with one thing the hub delivered to the application.
    ///
    /// Every call made *to* the application is answered, even though it offers nothing
    /// yet. A call left unanswered keeps a reserved slot of the caller's queue until its
    /// deadline, so ignoring one would let a plugin stall itself, and a flood of them
    /// could stall the application's own queue.
    pub(super) async fn handle_host_delivery(
        &mut self,
        delivery: peersus_api::message::Delivery,
    ) -> bool {
        use peersus_api::message::{Delivery, Operation};
        match delivery {
            Delivery::Result { id, result } => {
                peersus_diagnostics::dev_trace!(
                    stage = "owner-result",
                    call = id,
                    outcome = peersus_diagnostics::outcome(&result),
                    "Plugin call completed"
                );
                // However it ended, the call is over, so the same action may run again. The
                // hub gives the caller a result for every outcome, including a timeout and a
                // provider that went away, so this is the one place a call is closed.
                // The board is told how it ended in the application's own words: only the
                // code is read, never anything the plugin wrote.
                // However the call ended, a transfer it announced is over with it. A peer that
                // keeps sending a little at a time would otherwise keep the plugin inside this
                // one call for as long as it liked. Only the inbound registry is told: it is
                // keyed by call numbers, which is what this is. What a plugin sends out is
                // keyed by claims, a different kind of number, and is never touched by this.
                self.carrying.stop_call(id);
                // Surface input is not an action: its answer only lets the next batch go.
                if self.surfaces.owns(id) {
                    let failed = action_failure_sentence(&result);
                    let answered = if failed.is_some() {
                        self.surfaces.failed(id)
                    } else {
                        self.surfaces.answered(id)
                    };
                    let mut changed = failed.is_some();
                    if let (Some(sentence), Some((key, _))) = (failed, &answered) {
                        let plugin = key.plugin.clone();
                        self.plugin_error(&plugin, sentence);
                        // Nobody listening is the usual case (no pointer is away): not an error.
                        let _ = self.surface_failures.send(key.clone());
                    }
                    if let Some((key, Some(batch))) = answered {
                        let source = self
                            .plugins
                            .find(&key.plugin)
                            .and_then(|index| self.plugins.get(index))
                            .and_then(|spec| spec.manifest.contributions.input.as_ref())
                            .map(|input| input.source.clone());
                        if let Some(source) = source
                            && let Err(sentence) = self.send_surface_batch(&key, &source, batch)
                        {
                            self.plugin_error(&key.plugin, sentence);
                            changed = true;
                        }
                    }
                    return changed;
                }
                if self.page_views.owns(id) {
                    let failed = action_failure_sentence(&result);
                    let value = match &result {
                        peersus_api::message::Outcome::Ok { value } => Some(value),
                        peersus_api::message::Outcome::Error { .. } => None,
                    };
                    let answered = self.page_views.answered(id, value);
                    let mut changed = failed.is_some()
                        || answered.as_ref().is_some_and(|(_, _, changed)| *changed);
                    if let Some((key, _, true)) = &answered {
                        self.page_frames.set_page(
                            &key.plugin,
                            &key.page,
                            self.page_views.canvas_ids(key),
                        );
                        let peers = self.network.peers();
                        self.page_views.resolve_devices(key, |alias| {
                            self.aliases
                                .resolve(&key.plugin, alias, peers.iter().map(|p| &p.id))
                                .map(|id| id.0.clone())
                        });
                        self.fill_choosers(Some(&key.plugin));
                    }
                    if let (Some(sentence), Some((key, _, _))) = (failed, &answered) {
                        let plugin = key.plugin.clone();
                        self.plugin_error(&plugin, sentence);
                    }
                    if let Some((key, Some(next), _)) = answered
                        && let Err(sentence) = self.send_page_event(&key, next)
                    {
                        self.plugin_error(&key.plugin, sentence);
                        changed = true;
                    }
                    // A page's answer changes its view and nothing else of the owner's: the
                    // windows get that, not a fresh account of the network, the store and the
                    // plugins per sample of a drag.
                    if changed {
                        self.publish_pages();
                    }
                    return false;
                }
                let finished = self.in_flight.finish_key(id);
                let received = self.receiving.remove(&id);
                if let Some(key) = &finished {
                    self.board.finished(key, action_state(&result));
                }
                if let Some(sentence) = action_failure_sentence(&result) {
                    if finished.is_some() {
                        peersus_diagnostics::dev_trace!(
                            stage = "action-failed",
                            call = id,
                            outcome = peersus_diagnostics::outcome(&result),
                            "Plugin action failed"
                        );
                    }
                    match finished.map(|key| key.plugin).or(received) {
                        Some(plugin) => self.plugin_error(&plugin, sentence),
                        None => self.error(sentence),
                    }
                }
            }
            // Only ever delivered to the provider of a call, never to its caller.
            Delivery::Cancelled { ticket } => {
                peersus_diagnostics::dev_trace!(
                    stage = "owner-cancelled",
                    ticket,
                    "Plugin call cancelled"
                );
                self.platform_calls.cancel(ticket);
            }
            Delivery::Event { .. } => return false,
            Delivery::Call {
                ticket,
                caller,
                service,
                method,
                args,
                ..
            } => {
                peersus_diagnostics::dev_trace!(
                    stage = "owner-receive",
                    ticket,
                    method = peersus_diagnostics::method(&method),
                    "Plugin host call received"
                );
                if service == crate::display_service::SERVICE && method == "arrange" {
                    let result = self.arrange_displays(&caller, &args).await;
                    let changed = matches!(result, peersus_api::message::Outcome::Ok { .. });
                    let _ = self.host.submit(Operation::Reply { ticket, result });
                    return changed;
                }
                if let Some((_, _, capability, _)) = crate::platform_services::METHODS
                    .iter()
                    .find(|(offered, name, _, _)| *offered == service && *name == method)
                {
                    let granted = PluginId::new(&caller).ok().filter(|caller| {
                        self.standalone.all().get(caller).is_some_and(|grants| {
                            Permission::new(HOST, *capability)
                                .is_some_and(|permission| grants.allows(&permission))
                        })
                    });
                    const NOT_ALLOWED: &str =
                        "This extension has not been allowed to use that facility";
                    const NOT_OFFERED: &str = "This platform does not offer the requested facility";
                    let busy = |reason| ("busy", reason);
                    let result = match (granted, self.platform_services.clone()) {
                        // Not allowed by the person is said as that: "not available" would send them looking for
                        // something missing instead of to the extension's permissions.
                        (None, _) => Err(("denied", NOT_ALLOWED)),
                        // The application's own list answers on every platform: no adapter is asked.
                        (Some(caller), _)
                            if service == crate::platform_services::NOTIFY_SERVICE =>
                        {
                            let list = self.notifications.clone();
                            self.platform_calls
                                .start(ticket, caller.clone(), move || list.show(caller, args))
                                .map_err(busy)
                        }
                        (Some(caller), Some(platform)) => {
                            let mut args = self.name_tag(&caller, &service, args);
                            if *capability == crate::platform_services::PUBLIC_HTTP_CAPABILITY
                                && matches!(method.as_str(), "authorize-read" | "restore-read")
                                && let Some(fields) = args.as_object_mut()
                            {
                                // The extension cannot choose the agreement that binds an approval.
                                fields.remove("_accepted_agreement");
                                if !self.public_read_may_restore(
                                    &caller,
                                    fields.get("origin").and_then(serde_json::Value::as_str),
                                    fields.get("root").and_then(serde_json::Value::as_str),
                                ) {
                                    let _ = self.host.submit(Operation::Reply {
                                        ticket,
                                        result: peersus_api::message::Outcome::error(
                                            "denied", "Enable this public resource in the extension's settings",
                                        ),
                                    });
                                    return false;
                                }
                                if let Some(digest) = self.accepted_agreements.get(caller.as_str())
                                {
                                    fields.insert(
                                        "_accepted_agreement".to_owned(),
                                        digest.clone().into(),
                                    );
                                }
                            }
                            self.platform_calls
                                .start(ticket, caller.clone(), move || {
                                    platform.dispatch(crate::platform_services::Call {
                                        caller,
                                        service,
                                        method,
                                        args,
                                    })
                                })
                                .map_err(busy)
                        }
                        (Some(_), None) => Err(("unavailable", NOT_OFFERED)),
                    };
                    if let Err((code, reason)) = result {
                        peersus_diagnostics::dev_trace!(
                            stage = "platform-admission",
                            ticket,
                            outcome = code,
                            "Platform operation refused"
                        );
                        let _ = self.host.submit(Operation::Reply {
                            ticket,
                            result: peersus_api::message::Outcome::error(code, reason),
                        });
                    }
                    return false;
                }
                let (result, after) = self.answer_plugin_call(&caller, &service, &method, &args);
                let _ = self.host.submit(Operation::Reply { ticket, result });
                // After the answer, never before: see `answer_plugin_call`.
                return match after {
                    AfterReply::None => false,
                    AfterReply::Publish => true,
                    AfterReply::Restart(target) => {
                        self.restart_plugin(&target);
                        true
                    }
                };
            }
        }
        true
    }

    pub(super) async fn arrange_displays(
        &mut self,
        caller: &str,
        args: &serde_json::Value,
    ) -> peersus_api::message::Outcome {
        use peersus_api::message::Outcome;
        let Ok(plugin) = PluginId::new(caller) else {
            return Outcome::error("refused", "Unknown caller");
        };
        if !self.standalone.all().get(&plugin).is_some_and(|grants| {
            Permission::new(HOST, crate::display_service::ARRANGE)
                .is_some_and(|p| grants.allows(&p))
        }) {
            return Outcome::error(
                "refused",
                "Allow desktop arrangement in extension permissions first",
            );
        }
        let (peers, displays) = (self.network.peers(), self.network.peer_displays());
        let placements = match crate::display_service::arrange(
            args,
            &self.aliases,
            &plugin,
            &crate::display_service::Desktops {
                own: self.network.own_displays(),
                peers: &peers,
                displays: &displays,
                placements: &self.settings.current().layout,
            },
            |peer| seen_by(peer, &plugin).is_some(),
        ) {
            Ok(placements) => placements,
            Err(reason) => return Outcome::error("invalid-arrangement", reason),
        };
        let patch = peersus_core::config::SettingsPatch {
            layout: Some(Box::new(placements)),
            ..Default::default()
        };
        let settings = match patch.apply(self.settings.current()) {
            Ok(settings) => settings,
            Err(error) => return Outcome::error("invalid-arrangement", error.words()),
        };
        match self.apply_settings(settings).await {
            Ok(()) => Outcome::ok(self.displays_seen_by(&plugin)),
            Err(reason) => Outcome::error("could-not-save", reason),
        }
    }

    /// `peersus.displays` `inspect` for `plugin`: this computer's screens and the paired desktops it was allowed to see.
    fn displays_seen_by(&self, plugin: &PluginId) -> serde_json::Value {
        let (peers, displays) = (self.network.peers(), self.network.peer_displays());
        crate::display_service::inspect(
            &self.aliases,
            plugin,
            &self.settings.current().device_name,
            &crate::display_service::Desktops {
                own: self.network.own_displays(),
                peers: &peers,
                displays: &displays,
                placements: &self.settings.current().layout,
            },
            |peer| seen_by(peer, plugin).is_some(),
        )
    }

    /// What the application answers a plugin that called it, and the plugin to restart once
    /// that answer is on its way, if the call asked for one. `caller` is stamped by the hub
    /// from the channel, never taken from the message. `peersus.peers` offers `list` and
    /// `send`, `peersus.plugins` offers `list` and `restart`; every other call is answered as
    /// unknown rather than as a success it did not perform.
    ///
    /// A restart is returned and not performed here, because the plugin asked to be restarted
    /// may be the caller itself. The answer is submitted first and the supervisor then gives
    /// the process a short, bounded window before it ends it, so a plugin that restarts itself
    /// usually sees its request succeed. That is best effort and NOT a guarantee: nothing
    /// acknowledges that the plugin read the answer, and a process still there when the window
    /// closes is stopped all the same. What is promised is the restart; a plugin that must
    /// know it happened asks `list` from its next process.
    pub(super) fn answer_plugin_call(
        &mut self,
        caller: &str,
        service: &str,
        method: &str,
        args: &serde_json::Value,
    ) -> (peersus_api::message::Outcome, AfterReply) {
        use peersus_api::message::Outcome;
        let unknown = || Outcome::error("unknown-method", "The application does not offer that");
        let Ok(plugin) = PluginId::new(caller) else {
            return (unknown(), AfterReply::None);
        };
        let answer = |outcome| (outcome, AfterReply::None);
        let changed = |outcome| (outcome, AfterReply::Publish);
        match (service, method) {
            (crate::page_offers::SERVICE, "set") => {
                let Some(spec) = self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                else {
                    return answer(unknown());
                };
                // Whether the person turned on the extension's own global toggle of that id: only a value the person saved
                // counts, never what the manifest declares as its default.
                let saved = self.settings.current().plugins.get(&plugin);
                let chosen = |option: &str| {
                    spec.manifest.contributions.settings.iter().any(|setting| {
                        setting.id == option
                            && setting.scope == peersus_api::contributions::Scope::Global
                            && matches!(
                                setting.kind,
                                peersus_api::contributions::Kind::Toggle { .. }
                            )
                    }) && saved.and_then(|values| values.get(option))
                        == Some(&peersus_core::plugins::Value::Toggle(true))
                };
                match self.page_offers.set(&plugin, &spec.manifest, args, chosen) {
                    Ok(Some(removed)) => {
                        for page in &removed {
                            self.page_frames.set_page(&plugin, page, Vec::new());
                        }
                        let calls = self.page_views.forget_pages(&plugin, &removed);
                        self.cancel_calls(calls);
                        changed(Outcome::ok(serde_json::json!({})))
                    }
                    Ok(None) => answer(Outcome::ok(serde_json::json!({}))),
                    Err(reason) => answer(Outcome::error("refused", reason)),
                }
            }
            (crate::page_offers::ACTIONS_SERVICE, "withhold") => {
                let Some(spec) = self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                else {
                    return answer(unknown());
                };
                match self.page_offers.withhold(&plugin, &spec.manifest, args) {
                    Ok(true) => changed(Outcome::ok(serde_json::json!({}))),
                    Ok(false) => answer(Outcome::ok(serde_json::json!({}))),
                    Err(reason) => answer(Outcome::error("refused", reason)),
                }
            }
            (crate::page_frames::SERVICE, crate::page_frames::OPEN) => {
                let Some(claim) = crate::page_frames::open(&self.page_frames, plugin, args) else {
                    return answer(Outcome::error(
                        "refused",
                        "That page canvas is not open for a new stream",
                    ));
                };
                answer(Outcome::ok(serde_json::json!({ "claim": claim })))
            }
            // What the caller keeps for its own drawings, and lets go of. The caller is the plugin the HOST knows it is, so a layer is
            // never kept for, or drawn as, another's. A refusal says one sentence and changes nothing.
            (
                crate::drawing_layers::SERVICE,
                method @ (crate::drawing_layers::KEEP | crate::drawing_layers::FORGET),
            ) => {
                if self.plugins.find(&plugin).is_none() {
                    return answer(unknown());
                }
                let kept = if method == crate::drawing_layers::KEEP {
                    self.layers.keep(&plugin, args)
                } else {
                    self.layers.forget(&plugin, args)
                };
                match kept {
                    Ok(changed) => {
                        if changed {
                            self.publish_layers();
                        }
                        answer(Outcome::ok(serde_json::json!({})))
                    }
                    Err(reason) => answer(Outcome::error("refused", reason)),
                }
            }
            (crate::display_service::SERVICE, "inspect")
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(self.displays_seen_by(&plugin)))
            }
            (PLATFORM_SERVICE, "inspect")
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(
                    serde_json::to_value(peersus_api::platform::HostPlatform::current())
                        .unwrap_or(serde_json::Value::Null),
                ))
            }
            // The one thing an extension may learn of its own agreement: whether this person accepted exactly the one it
            // carries now. Nothing here lets it read the pages, change them, or accept for anyone.
            (EXTENSION_SERVICE, "tos") if args.as_object().is_some_and(|args| args.is_empty()) => {
                let Some(spec) = self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                else {
                    return answer(unknown());
                };
                answer(Outcome::ok(serde_json::json!({
                    "accepted": crate::plugin_agreements::is_accepted(
                        self.accepted_agreements.get(plugin.as_str()),
                        &spec.manifest,
                    ),
                })))
            }
            // The person's acceptance, once, of the application's warning about installing from outside the store. It is the person's and
            // not an extension's, so it answers the same for every extension and says nothing else.
            (EXTENSION_SERVICE, "outside_store")
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                if self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                    .is_none()
                {
                    return answer(unknown());
                }
                answer(Outcome::ok(serde_json::json!({
                    "accepted": self.settings.current().outside_store_risk_accepted,
                })))
            }
            // The person's language, for an extension's own words: the same for every extension that asked for it.
            (LANGUAGE_SERVICE, "get") if args.as_object().is_some_and(|args| args.is_empty()) => {
                let language = self.person_language();
                answer(Outcome::ok(
                    serde_json::json!({ "language": language.tag() }),
                ))
            }
            // One part of how this device is doing; the hub has already refused a caller not granted that part. Storage is
            // that of the folder received files go to, else of the application's own data.
            (crate::system_readings::SERVICE, method)
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                let settings = self.settings.current();
                let storage = settings.save_folder.as_deref().or_else(|| {
                    self.packages
                        .as_ref()
                        .map(crate::plugin_manager::Manager::data_directory)
                });
                answer(
                    crate::system_readings::answer(
                        method,
                        storage,
                        self.platform_services
                            .as_deref()
                            .and_then(|platform| platform.readings()),
                    )
                    .map_or_else(unknown, Outcome::ok),
                )
            }
            (CONFIG_SERVICE, "get") if args.as_object().is_some_and(|args| args.is_empty()) => {
                let Some(spec) = self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                else {
                    return answer(unknown());
                };
                let saved = self.settings.current().plugins.get(&plugin);
                let approved = spec
                    .manifest
                    .contributions
                    .settings
                    .iter()
                    .any(|s| s.public_read.is_some())
                    .then(|| self.with_public_read_value(spec, saved.cloned().unwrap_or_default()));
                let mut config = configuration(
                    spec,
                    approved.as_ref().or(saved),
                    &self.aliases,
                    &self.network.peers(),
                );
                let features: Vec<_> = peersus_api::elements::PAGE_FEATURES
                    .into_iter()
                    .chain([peersus_api::elements::FEATURE_CANVAS])
                    .collect();
                config[peersus_api::elements::PAGE_FEATURES_KEY] = serde_json::json!(features);
                answer(Outcome::ok(config))
            }
            (PEERS_SERVICE, "list") => {
                let Some(spec) = self
                    .plugins
                    .find(&plugin)
                    .and_then(|index| self.plugins.get(index))
                else {
                    return answer(unknown());
                };
                let own = own_streams(spec);
                let by_device = self.network.peer_accepts();
                answer(Outcome::ok(peers_listing_with(
                    &self.aliases,
                    &plugin,
                    &grantable_for_a_device(spec),
                    &self.network.peers(),
                    &Taken {
                        own: &own,
                        by_device: Some(&by_device),
                    },
                )))
            }
            (PEERS_SERVICE, "send") => answer(match self.begin_send(&plugin, args) {
                Ok(claim) => Outcome::ok(serde_json::json!({ "claim": claim })),
                Err(reason) => Outcome::error("refused", reason),
            }),
            (PLUGINS_SERVICE, "list") => answer(Outcome::ok(plugins_listing(
                self.plugins
                    .iter()
                    .map(|spec| (&spec.id, self.plugin_status(&spec.id))),
            ))),
            (PLUGINS_SERVICE, "restart") => match parse_restart(args) {
                Err(reason) => answer(Outcome::error("refused", reason)),
                Ok(target) if self.plugins.find(&target).is_none() => {
                    answer(Outcome::error("refused", "There is no such plugin"))
                }
                Ok(target) if self.disabled.contains(&target) => answer(Outcome::error(
                    "refused",
                    "Enable the extension before restarting it",
                )),
                Ok(target) => (
                    Outcome::ok(serde_json::json!({})),
                    AfterReply::Restart(target),
                ),
            },
            // What is going on now, for a plugin that has only just begun to listen or that was restarted: the transfers still under way, said exactly
            // as the events say them (see `transfer_events`), so the answer to `active` can never carry a name and the answer to `active-named` can
            // never carry less than `changed-named` does. The hub has already refused a caller that was not granted the permission that guards each.
            (
                crate::transfer_events::TRANSFERS_SERVICE,
                crate::transfer_events::TRANSFERS_ACTIVE,
            ) if args.as_object().is_some_and(|args| args.is_empty()) => answer(Outcome::ok(
                crate::transfer_events::active(&self.network.transfers(), false),
            )),
            (
                crate::transfer_events::TRANSFERS_SERVICE,
                crate::transfer_events::TRANSFERS_ACTIVE_NAMED,
            ) if args.as_object().is_some_and(|args| args.is_empty()) => answer(Outcome::ok(
                crate::transfer_events::active(&self.network.transfers(), true),
            )),
            // What plugins make of this device's statuses, and what they read of the others'. The caller is the plugin the HOST knows it is,
            // so the owner of a status is never a thing a plugin says; the hub has already refused one that was not granted the permission
            // that guards each method. A refusal says one sentence and changes nothing.
            (
                crate::status_service::STATUS_SERVICE,
                method @ (crate::status_service::STATUS_DEFINE
                | crate::status_service::STATUS_SET
                | crate::status_service::STATUS_CLEAR),
            ) => {
                let applied =
                    crate::status_service::apply(&mut self.statuses, &plugin, method, args);
                let outcome = match applied.answer {
                    Ok(value) => Outcome::ok(value),
                    Err(reason) => Outcome::error("refused", reason),
                };
                if applied.changed {
                    self.say_statuses();
                    changed(outcome)
                } else {
                    answer(outcome)
                }
            }
            (crate::status_service::STATUS_SERVICE, crate::status_service::STATUS_LIST)
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(crate::status_service::list(
                    &self.aliases,
                    &plugin,
                    &self.network.peers(),
                    |peer| seen_by(peer, &plugin).is_some(),
                )))
            }
            (crate::status_service::STATUS_SERVICE, crate::status_service::STATUS_OWN)
                if args.as_object().is_some_and(|args| args.is_empty()) =>
            {
                answer(Outcome::ok(crate::status_service::own(
                    &self.statuses.defined_by(&plugin),
                    &self.statuses.active(),
                )))
            }
            _ => answer(unknown()),
        }
    }

    /// What a device's own pointer is labelled with, decided here and never by the extension: an input call that asks for a name
    /// (`"show_name": true`) and names a device by the alias this extension was given gets that device's name as its user knows it.
    /// A `label` the extension wrote itself is dropped, so no extension can put words on the screen as if a device had said them.
    pub(super) fn name_tag(
        &self,
        caller: &PluginId,
        service: &str,
        mut args: serde_json::Value,
    ) -> serde_json::Value {
        use crate::platform_services::input_arg;
        if service != crate::platform_services::INPUT_SERVICE {
            return args;
        }
        let Some(object) = args.as_object_mut() else {
            return args;
        };
        object.remove(input_arg::LABEL);
        let wanted = object.remove(input_arg::SHOW_NAME) == Some(serde_json::Value::Bool(true));
        if let (true, Some(alias)) = (
            wanted,
            object
                .get(input_arg::OWNER)
                .and_then(|owner| owner.as_str()),
        ) {
            let peers = self.network.peers();
            if let Some(device) =
                self.aliases
                    .resolve(caller, alias, peers.iter().map(|peer| &peer.id))
                && let Some(peer) = peers.iter().find(|peer| &peer.id == device)
            {
                object.insert(
                    input_arg::LABEL.into(),
                    serde_json::Value::String(peer.name.clone()),
                );
            }
        }
        args
    }

    pub(super) fn revoke_platform_calls(&mut self, plugin: &PluginId) {
        self.platform_calls.revoke(plugin);
        if let Some(platform) = &self.platform_services {
            platform.revoke(plugin);
        }
        self.notifications.revoke(plugin);
    }

    /// Tell every paired device what this one now says of itself besides being awake: the statuses that are on. Called after anything that
    /// can change them, and only then. A refusal changes nothing, so it is never told.
    pub(super) fn say_statuses(&mut self) {
        // The set is bounded by `Statuses` and cannot hold one twice, so the network never refuses it; if it ever did, what the device says
        // stays as it was, which is the honest thing to say.
        let _ = self.network.set_statuses(self.statuses.active());
    }

    /// A plugin's statuses go, defined and on. The paired devices are told only if one of them was on: nothing else the device says
    /// changed, and a device must not be written to for nothing.
    pub(super) fn forget_statuses(&mut self, plugin: &PluginId) {
        if self.statuses.forget_plugin(plugin) {
            self.say_statuses();
        }
    }

    /// Open a stream to a device for a plugin that asked to send, and set aside what the
    /// plugin needs to start writing into it. Returns the number it must present.
    ///
    /// Everything that can refuse comes first and reserves nothing: the arguments, then the
    /// device layer (`authorize_send`: this device, online, granted this plugin), then a live
    /// connection to it. Only then is a number issued and one task started. That task opens
    /// the device's stream, waits for the plugin's data connection, and carries the bytes;
    /// it is recorded under the device and the plugin from the start, so withdrawing a
    /// permission or forgetting the device stops it whether its stream is open yet or not.
    pub(super) fn begin_send(
        &mut self,
        plugin: &PluginId,
        args: &serde_json::Value,
    ) -> Result<u64, &'static str> {
        use rand::Rng;
        let request = parse_send(args)?;
        let peers = self.network.peers();
        let peer = authorize_send(&self.aliases, plugin, &request.device, &peers)?;
        let spec = self
            .plugins
            .find(plugin)
            .and_then(|index| self.plugins.get(index))
            .ok_or(SEND_REFUSED)?;
        authorize_service_send(spec, peer, &request.service, request.version)?;
        let device = peer.id.clone();
        if !self.may_send_now(&device) {
            return Err(super::privacy::PRIVATE_REFUSED);
        }
        let connection = self.network.connection(&device).ok_or(SEND_REFUSED)?;
        let (tx, rx) = oneshot::channel();
        let owner = crate::parked::Owner {
            peer: device,
            plugin: plugin.clone(),
        };
        let claim = self
            .outbound
            .park_fresh(
                owner.clone(),
                tx,
                std::time::Instant::now(),
                crate::parked::MAX_WAIT,
                || rand::rng().next_u64() & !crate::page_frames::CLAIM_BIT,
            )
            .map_err(|_| SEND_REFUSED)?;
        let handle = self.transfers.spawn(async move {
            let outcome = carry_out(connection, request, rx, OPEN_LIMIT).await;
            (Direction::Outbound, claim, outcome)
        });
        self.carrying_out.started(claim, owner, handle);
        Ok(claim)
    }
}

/// Milliseconds since this owner first needed the clock, on a clock that never goes back.
fn owner_clock() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    u64::try_from(
        EPOCH
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
