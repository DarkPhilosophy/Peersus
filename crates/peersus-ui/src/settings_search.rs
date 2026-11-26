//! Search only data already displayed by Settings. No plugin calls, disk reads or secret values.
use crate::settings_index::Index;
pub use crate::settings_index::{Entry, Target};
use crate::{
    i18n::{self, Language},
    view::{self, SettingsTab, View},
};
use peersus_api::contributions::{Kind, Scope};
use peersus_core::model::PeerId;

/// What the platform actually offers, not an assumption inferred from a manifest.
#[derive(Clone, Copy, Debug, Default)]
pub struct Features<'a> {
    pub notifications: bool,
    pub camera: bool,
    pub overlay: bool,
    /// Whether an offer is announced outside the window at an edge the person may choose.
    pub offer_place: bool,
    pub battery: bool,
    pub window_bar: bool,
    /// The platform's settings for services that an installed extension uses, as Settings shows them now: the title of each and
    /// the further words it may be looked for by. The interface names none of its own.
    pub service_settings: &'a [(&'static str, &'static str)],
}

/// Constructed from the current view whenever queried: installation, revocation and new transfers
/// take effect immediately. Labels/help/choices are indexed, never arbitrary saved text values.
pub fn entries(view: &View, language: Language, features: Features<'_>) -> Vec<Entry> {
    let mut index = Index::new(language);
    for tab in SettingsTab::ALL {
        index.set_tab(tab);
        index.add(tab.title(), String::new());
    }
    for tab in SettingsTab::ALL {
        index.set_tab(tab);
        match tab {
            // How things stand: nothing on it is set there.
            SettingsTab::Overview => {}
            SettingsTab::Device => device(&mut index, view, features),
            SettingsTab::Appearance => {
                crate::appearance::search(&mut index, view);
                if features.window_bar {
                    use crate::settings_page::{WINDOW_BAR_POSITION, WINDOW_BAR_POSITION_NOTE};
                    index.add(WINDOW_BAR_POSITION, index.tr(WINDOW_BAR_POSITION_NOTE));
                }
            }
            SettingsTab::Languages => crate::languages_page::search(&mut index, view),
            SettingsTab::Network => crate::relay_choice::search(&mut index),
            SettingsTab::Extensions => extensions(&mut index, view),
            // Every permission row belongs to an extension and carries its own anchor: added with the extensions.
            SettingsTab::Permissions => {}
            SettingsTab::Binds => crate::binds_page::search(&mut index, view),
            SettingsTab::Diagnostics => crate::diagnostics_page::search(&mut index),
            SettingsTab::About => crate::about::search(&mut index),
        }
    }
    index.into_entries()
}

/// The Device page (drawn in `lib.rs`): its rows, and the platform's own where the platform has them.
fn device(index: &mut Index, view: &View, features: Features<'_>) {
    index.add("Device name", view.settings.device_name.clone());
    let languages = view::language_choices(&view.languages, i18n::messages())
        .into_iter()
        .map(|choice| choice.name)
        .collect::<Vec<_>>()
        .join(" · ");
    index.add("Language", languages);
    index.add("Interface size", String::new());
    let start = index.tr("Start without opening a window.");
    index.add("Run in the background", start);
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        let title = "Start when you sign in";
        let words = index.tr(
            "Off: Peersus starts when you open it. This is separate from Run in the background.",
        );
        index.add(title, words);
        #[cfg(target_os = "linux")]
        let platform_words = "Your session runs before you sign in (lingering). The keyring is locked until you sign in, so Peersus waits and does not connect before then.";
        #[cfg(target_os = "macos")]
        let platform_words = "This user service runs only in your signed-in macOS session. It does not control the login screen.";
        #[cfg(windows)]
        let platform_words = "Windows applies this account's choice at the next sign-in. No service instance runs for a signed-out account.";
        index.add(title, index.tr(platform_words));
    }
    if features.notifications {
        let words = index.tr("On: an offer of a file is shown even when the program is closed.");
        index.add("Notifications", words);
    }
    if features.camera {
        let words =
            index.tr("Choose the camera that reads a code, and press Test to see what it shows.");
        index.add("Camera", words);
    }
    let words = index.tr("Off: your paired devices see this device online, with its name, picture and statuses. On: this device stays connected and sees them, but tells them nothing about itself, so they show it as offline, and extensions send nothing on their own. Sending a file to a device, or choosing a device in an extension, shows this device to that one device until a short while after you are done.");
    index.add("Private mode", words);
    let words = index.tr("Off: after choosing where to send, you hold the circle until it fills, so a wrong choice costs nothing. On: touching the circle sends at once.");
    index.add("Send instantly", words);
    let words = index.tr("Off: you are asked where each file goes. On: files go straight to the folder below, without asking.");
    index.add("Save incoming files on their own", words);
    let words = index.tr("INCOMING FILES");
    index.add("Choose folder", words);
    if cfg!(target_os = "android") {
        let words = index.tr("Touch the screen with two fingers to bring Settings down from the top. It does nothing while Settings is already open.");
        index.add("Two fingers open Settings", words);
        let words = index.tr("Leaving with Back takes a second press soon after the first, so that Back pressed by accident does not close Peersus.");
        index.add("Back twice to leave", words);
    }
    if features.overlay {
        let words = index.tr("Off: the list of the files of an offer opens in Peersus. Allow it to see the list over other apps.");
        index.add("Offers over other apps", words);
    }
    if features.offer_place {
        let words = index.tr(
            "The edge of the screen where an offer of a file is shown over what you are doing.",
        );
        index.add("Where an offer appears", words);
    }
    if features.battery {
        let words = index.tr("Limited: while the phone is idle the system may cut the network of the background service. Allow it in the battery settings.");
        index.add("Battery", words);
    }
    for (title, words) in features.service_settings {
        index.add(title, *words);
    }
}

/// Each installed extension with its settings, actions and permissions, by device; and what the store lists that is not installed.
fn extensions(index: &mut Index, view: &View) {
    for plugin in &view.plugins {
        let plugin_name = plugin.id.as_str();
        index.push(Entry {
            title: plugin_name.into(),
            detail: index.tr(view::plugin_word(plugin).0),
            target: Target::Plugin(plugin.id.clone()),
        });
        for (capability, description) in &plugin.manifest.capabilities {
            index.push(Entry {
                title: capability.clone(),
                detail: format!("{plugin_name} · {description}"),
                target: Target::Plugin(plugin.id.clone()),
            });
        }
        if view.devices.is_empty() {
            for row in view::PermissionRows::of_in(
                index.language(),
                plugin,
                &plugin.grantable.per_device,
                &peersus_core::plugins::Grants::default(),
            ) {
                index.push(Entry {
                    title: row.permission.name,
                    detail: format!(
                        "{plugin_name} · {} · {}",
                        row.description,
                        index.tr("Pair a device to allow this plugin anything for it.")
                    ),
                    target: Target::PluginPermissions(plugin.id.clone()),
                });
            }
        }
        let scopes = std::iter::once((None, None)).chain(
            view.devices
                .iter()
                .map(|device| (Some(PeerId(device.id.clone())), Some(device))),
        );
        for (peer, device) in scopes {
            let scope = if peer.is_some() {
                Scope::Device
            } else {
                Scope::Global
            };
            let context = device.map_or_else(
                || plugin_name.to_owned(),
                |device| format!("{plugin_name} · {}", device.name),
            );
            for setting in plugin
                .manifest
                .contributions
                .settings
                .iter()
                .filter(|setting| setting.scope == scope)
            {
                let choices = match &setting.kind {
                    Kind::Choice { choices, .. } => choices.join(" · "),
                    _ => String::new(),
                };
                index.push(Entry {
                    title: setting.label.clone(),
                    detail: format!(
                        "{context} · {} · {} · {choices}",
                        setting.id,
                        setting.help.as_deref().unwrap_or("")
                    ),
                    target: Target::Setting {
                        plugin: plugin.id.clone(),
                        id: setting.id.clone(),
                        peer: peer.clone(),
                    },
                });
            }
            for action in plugin
                .manifest
                .contributions
                .actions
                .iter()
                .filter(|action| {
                    action.needs_device == peer.is_some()
                        && matches!(
                            action.input,
                            peersus_api::contributions::ActionInput::None
                                | peersus_api::contributions::ActionInput::SecretText
                        )
                })
            {
                index.push(Entry {
                    title: action.label.clone(),
                    detail: format!("{context} · {}", action.id),
                    target: Target::Action {
                        plugin: plugin.id.clone(),
                        id: action.id.clone(),
                        peer: peer.clone(),
                    },
                });
            }
            let offered = if peer.is_some() {
                &plugin.grantable.per_device
            } else {
                &plugin.grantable.outright
            };
            let empty = peersus_core::plugins::Grants::default();
            let held = device
                .and_then(|device| device.grants.get(&plugin.id))
                .or_else(|| {
                    if peer.is_none() {
                        view.grants.get(&plugin.id)
                    } else {
                        None
                    }
                })
                .unwrap_or(&empty);
            for row in view::PermissionRows::of_in(index.language(), plugin, offered, held) {
                index.push(Entry {
                    title: row.permission.name.clone(),
                    detail: format!(
                        "{context} · {} · {}",
                        row.description,
                        index.tr(if row.allowed { "on" } else { "off" })
                    ),
                    target: Target::Permission {
                        plugin: plugin.id.clone(),
                        permission: row.permission,
                        peer: peer.clone(),
                    },
                });
            }
        }
    }
    for listing in &view.store.listings {
        if !view
            .plugins
            .iter()
            .any(|plugin| plugin.id == listing.plugin)
        {
            index.push(Entry {
                title: listing.plugin.as_str().to_owned(),
                detail: format!("{} · {}", listing.author, listing.description),
                target: Target::Plugin(listing.plugin.clone()),
            });
        }
    }
}

/// Each query word may match a different field, in any order. Exact/contiguous titles beat
/// descriptive context. Every matching result remains available; the UI bounds its viewport.
pub fn search(query: &str, view: &View, language: Language, features: Features<'_>) -> Vec<Entry> {
    if query.trim().is_empty() || query.chars().count() > 256 {
        return Vec::new();
    }
    let tokens: Vec<_> = query.split_whitespace().map(view::folded).collect();
    let mut hits: Vec<_> = entries(view, language, features)
        .into_iter()
        .enumerate()
        .filter_map(|(order, entry)| {
            let group = i18n::translate(language, entry.target.tab().title());
            let score = tokens.iter().try_fold(0u32, |sum, token| {
                let best = [
                    (entry.title.as_str(), 8),
                    (entry.detail.as_str(), 2),
                    (group, 1),
                ]
                .into_iter()
                .filter_map(|(field, weight)| {
                    view::fuzzy_score(token, field).map(|score| {
                        let exact = view::folded(field).contains(token.as_str());
                        weight * (score + if exact { 32 } else { 0 })
                    })
                })
                .max()?;
                Some(sum.saturating_add(best))
            })?;
            Some((score, order, entry))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, _, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Phase;
    use peersus_api::{
        contributions::{Action, Setting, Source},
        manifest::Manifest,
        permission::Permission,
    };
    use peersus_core::{
        model::{ActionState, ActionStatus, Grantable, PluginInfo, PluginStatus, Standing},
        plugins::PluginId,
    };

    fn plugin() -> PluginInfo {
        let mut manifest = Manifest::new(peersus_api::manifest::Execution::Process);
        manifest.capabilities.insert(
            "documents.publish".into(),
            "Publish selected documents".into(),
        );
        manifest
            .capabilities
            .insert("hidden.power".into(), "Not offered by the owner".into());
        manifest.contributions.settings.push(Setting {
            public_read: None,
            platforms: Vec::new(),
            id: "schedule".into(),
            label: "Delivery schedule".into(),
            help: Some("Choose when documents are synchronized".into()),
            scope: Scope::Global,
            kind: Kind::Text {
                max_len: 64,
                default: String::new(),
            },
        });
        manifest.contributions.actions.push(Action {
            id: "publish".into(),
            label: "Publish document".into(),
            short_label: None,
            input: peersus_api::contributions::ActionInput::None,
            target: Source {
                service: "documents".into(),
                method: "publish".into(),
            },
            capability: None,
            needs_device: false,
            destructive: false,
            timeout_ms: 1000,
            chord: None,
            help: None,
        });
        PluginInfo {
            id: PluginId::new("community-notes").unwrap(),
            manifest,
            standing: Standing::Ready,
            status: PluginStatus::Stopped,
            grantable: Grantable {
                outright: vec![Permission::new("community-notes", "documents.publish").unwrap()],
                per_device: vec![],
            },
            program: None,
            package: None,
            agreed: false,
            from_store: false,
        }
    }
    fn find(query: &str, view: &View) -> Vec<Entry> {
        search(query, view, Language::ENGLISH, Features::default())
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn desktop_startup_search_opens_device_settings_by_title_or_platform_help() {
        let view = View::default();
        #[cfg(target_os = "linux")]
        let help = "lingering keyring";
        #[cfg(target_os = "macos")]
        let help = "signed-in macOS session";
        #[cfg(windows)]
        let help = "Windows next sign-in";
        for query in ["Start when you sign in", help] {
            assert!(find(query, &view).iter().any(|entry| {
                entry.title == "Start when you sign in"
                    && entry.target == Target::Group(SettingsTab::Device)
            }));
        }
    }

    #[test]
    fn unknown_community_plugin_contributions_are_live_and_fuzzy_across_fields() {
        let mut view = View::default();
        assert!(find("community-notes", &view).is_empty());
        view.plugins.push(plugin());
        assert!(
            find("community-notes", &view)
                .iter()
                .any(|entry| matches!(entry.target, Target::Plugin(_)))
        );
        assert!(find("schdl community-notes", &view).iter().any(
            |entry| matches!(entry.target, Target::Setting { ref id, .. } if id == "schedule")
        ));
        assert!(
            find("document publish", &view).iter().any(
                |entry| matches!(entry.target, Target::Action { ref id, .. } if id == "publish")
            )
        );
        view.plugins.clear();
        assert!(find("community-notes", &view).is_empty());
    }

    #[test]
    fn extension_search_uses_the_words_from_the_extension_not_the_app_catalog() {
        let mut info = plugin();
        let setting = &mut info.manifest.contributions.settings[0];
        setting.label = "Battery".into();
        setting.help = Some("Choose where the window controls and main navigation appear.".into());
        info.manifest.contributions.actions[0].label = "Language".into();
        let view = View {
            plugins: vec![info],
            ..View::default()
        };
        let language = crate::i18n::fixture::language("ro");
        let features = Features {
            battery: true,
            ..Features::default()
        };
        let entries = entries(&view, language, features);
        let setting = entries
            .iter()
            .find(|entry| matches!(&entry.target, Target::Setting { id, .. } if id == "schedule"))
            .expect("the extension setting is indexed");
        assert_eq!(setting.title, "Battery");
        assert!(
            setting
                .detail
                .contains("Choose where the window controls and main navigation appear.")
        );
        assert!(entries.iter().any(|entry| {
            matches!(&entry.target, Target::Action { id, .. } if id == "publish")
                && entry.title == "Language"
        }));
        assert!(entries.iter().any(|entry| {
            entry.target == Target::Group(SettingsTab::Device) && entry.title == "Baterie"
        }));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn shortcut_search_keeps_the_extensions_label_and_help() {
        let mut info = plugin();
        let action = &mut info.manifest.contributions.actions[0];
        action.label = "Battery".into();
        action.help = Some("Choose where the window controls and main navigation appear.".into());
        let view = View {
            plugins: vec![info],
            ..View::default()
        };
        let shortcuts = search(
            "community-notes",
            &view,
            crate::i18n::fixture::language("ro"),
            Features::default(),
        );
        let entry = shortcuts
            .iter()
            .find(|entry| entry.target == Target::Group(SettingsTab::Binds))
            .expect("the extension shortcut can be found");
        assert_eq!(entry.title, "Battery");
        assert!(
            entry
                .detail
                .contains("Choose where the window controls and main navigation appear.")
        );
    }

    #[test]
    fn catalog_extension_search_target_survives_install_and_removal_without_duplicates() {
        let mut view = View::default();
        let mut installed = plugin();
        installed.manifest.capabilities.clear();
        let id = installed.id.clone();
        view.store.listings.push(peersus_core::model::StoreListing {
            plugin: id.clone(),
            version: "1.0.0".into(),
            author: "Community".into(),
            link: "https://example.org/extension".into(),
            description: "Notes".into(),
            bytes: 0,
            installed_version: None,
        });
        let extension_targets = |view: &View| {
            find("community-notes", view)
                .into_iter()
                .filter_map(|entry| match entry.target {
                    Target::Plugin(plugin) => Some(plugin),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(extension_targets(&view), vec![id.clone()]);
        view.plugins.push(installed);
        assert_eq!(extension_targets(&view), vec![id.clone()]);
        view.plugins.clear();
        assert_eq!(extension_targets(&view), vec![id]);
    }

    #[test]
    fn permission_hits_are_only_owner_offered_and_keep_scope_and_owner_identity() {
        let mut view = View {
            plugins: vec![plugin()],
            ..View::default()
        };
        let hits = find("publish permissions", &view);
        assert!(hits.iter().any(|entry| matches!(&entry.target, Target::Permission { permission, peer: None, .. } if permission.owner == "community-notes" && permission.name == "documents.publish")));
        assert!(!entries(&view, Language::ENGLISH, Features::default()).iter().any(|entry| matches!(&entry.target, Target::Permission { permission, .. } if permission.name == "hidden.power")));
        view.plugins[0].grantable.outright.clear();
        assert!(
            !find("publish permissions", &view)
                .iter()
                .any(|entry| matches!(entry.target, Target::Permission { .. }))
        );
    }

    #[test]
    fn the_search_of_settings_never_lists_transfers_or_results_of_actions_however_many_there_are() {
        let mut view = View {
            plugins: vec![plugin()],
            ..View::default()
        };
        // A long history: it is the Transfers page's, with its own search, and must cost the search of Settings nothing.
        for n in 0..2_000 {
            view.transfers.push(view::Progress {
                id: format!("transfer-{n}"),
                name: format!("release-notes-{n}.pdf"),
                folders: vec![],
                bytes: 24,
                incoming: false,
                from: None,
                peer: "peer".into(),
                percent: 100,
                code: None,
                expires_at: None,
                saved_as: None,
                phase: Phase::Done,
            });
        }
        view.actions.push(ActionStatus {
            plugin: view.plugins[0].id.clone(),
            action: "publish".into(),
            peer: None,
            state: ActionState::Done,
            progress: None,
        });
        let everything = entries(&view, Language::ENGLISH, Features::default());
        assert!(
            everything
                .iter()
                .all(|entry| !entry.title.starts_with("release-notes-")),
            "a transfer of the history was in the search of Settings"
        );
        assert!(
            find("release-notes", &view).is_empty(),
            "typing the name of a file found it in Settings"
        );
        // The result of an action is shown under the plugin that made it (`ActionButton`), not as a second copy in a list.
        assert!(
            find("publish completed", &view)
                .iter()
                .all(|entry| !entry.detail.contains("completed"))
        );
    }

    #[test]
    fn search_does_not_index_saved_free_text_or_invent_platform_controls() {
        use peersus_core::plugins::{Value, Values};
        let info = plugin();
        let mut values = Values::default();
        values
            .set(
                &info.manifest.contributions.settings,
                Scope::Global,
                "schedule",
                Value::Text("private-draft-unique".into()),
            )
            .unwrap();
        let mut view = View {
            plugins: vec![info.clone()],
            ..View::default()
        };
        view.settings.plugins.insert(info.id, values);
        assert!(find("private-draft-unique", &view).is_empty());
        assert!(
            !find("Camera", &view)
                .iter()
                .any(|entry| entry.title == "Camera")
        );
        assert!(
            search(
                "Camera",
                &view,
                Language::ENGLISH,
                Features {
                    camera: true,
                    ..Features::default()
                }
            )
            .iter()
            .any(|entry| entry.title == "Camera")
        );
        // A platform setting for a service: not invented where Settings shows none, and found by the words its adapter gave it.
        assert!(
            !find("made-up provider", &view)
                .iter()
                .any(|entry| entry.title == "A platform consent")
        );
        assert!(
            search(
                "made-up provider",
                &view,
                Language::ENGLISH,
                Features {
                    service_settings: &[("A platform consent", "made-up provider words")],
                    ..Features::default()
                }
            )
            .iter()
            .any(|entry| entry.target == Target::Group(SettingsTab::Device)
                && entry.title == "A platform consent")
        );
        // The battery row: not invented where the platform has none, and found where it has, by its title and by its words in another language.
        assert!(
            !find("Battery", &view)
                .iter()
                .any(|entry| entry.title == "Battery")
        );
        assert!(
            search(
                "Battery",
                &view,
                Language::ENGLISH,
                Features {
                    battery: true,
                    ..Features::default()
                }
            )
            .iter()
            .any(|entry| entry.target == Target::Group(SettingsTab::Device)
                && entry.title == "Battery")
        );
        assert!(
            search(
                "serviciului din fundal",
                &view,
                crate::i18n::fixture::language("ro"),
                Features {
                    battery: true,
                    ..Features::default()
                }
            )
            .iter()
            .any(|entry| entry.title == "Baterie"),
            "the Romanian words of the battery row were not found"
        );
    }

    #[test]
    fn the_window_bar_position_is_only_indexed_where_the_control_is_drawn() {
        let view = View::default();
        for window_bar in [false, true] {
            let results = search(
                crate::settings_page::WINDOW_BAR_POSITION,
                &view,
                Language::ENGLISH,
                Features {
                    window_bar,
                    ..Features::default()
                },
            );
            assert_eq!(
                results.iter().any(|entry| {
                    entry.target == Target::Group(SettingsTab::Appearance)
                        && entry.title == crate::settings_page::WINDOW_BAR_POSITION
                }),
                window_bar
            );
        }
    }

    #[test]
    fn more_than_six_matches_remain_reachable_and_exact_titles_rank_before_context() {
        let mut view = View::default();
        for number in 0..20 {
            let mut info = plugin();
            info.id = PluginId::new(format!("community-{number}")).unwrap();
            view.plugins.push(info);
        }
        assert_eq!(
            find("delivery schedule", &view)
                .iter()
                .filter(|entry| matches!(entry.target, Target::Setting { .. }))
                .count(),
            20
        );
        assert!(matches!(
            find("delivery schedule", &view)[0].target,
            Target::Setting { .. }
        ));
        assert!(find("", &view).is_empty());
        assert!(find(&"x".repeat(257), &view).is_empty());
    }
}
