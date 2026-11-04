//! Search only data already displayed by Settings. No plugin calls, disk reads or secret values.
use crate::{
    i18n::{self, Language},
    view::{self, SettingsTab, View},
};
use peersus_core::{model::PeerId, plugins::PluginId};
use peersus_plugin_api::{
    contributions::{Kind, Scope},
    permission::Permission,
};

/// Stable destinations, independent of list positions and measured widget coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Group(SettingsTab),
    // Built-in controls lead to their group; dynamic records have exact item anchors.
    Plugin(PluginId),
    PluginPermissions(PluginId),
    Setting {
        plugin: PluginId,
        id: String,
        peer: Option<PeerId>,
    },
    Action {
        plugin: PluginId,
        id: String,
        peer: Option<PeerId>,
    },
    Permission {
        plugin: PluginId,
        permission: Permission,
        peer: Option<PeerId>,
    },
}
impl Target {
    pub fn tab(&self) -> SettingsTab {
        match self {
            Self::Group(tab) => *tab,
            Self::Plugin(_) | Self::Setting { .. } | Self::Action { .. } => SettingsTab::Extensions,
            Self::Permission { .. } | Self::PluginPermissions(_) => SettingsTab::Permissions,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    pub detail: String,
    pub target: Target,
}

/// What the platform actually offers, not an assumption inferred from a manifest.
#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub notifications: bool,
    pub camera: bool,
    pub overlay: bool,
    pub battery: bool,
    pub clipboard: bool,
}

/// Constructed from the current view whenever queried: installation, revocation and new transfers
/// take effect immediately. Labels/help/choices are indexed, never arbitrary saved text values.
pub fn entries(view: &View, language: Language, features: Features) -> Vec<Entry> {
    let tr = |text: &str| i18n::translate(language, text).to_owned();
    let mut entries: Vec<_> = SettingsTab::ALL
        .into_iter()
        .map(|tab| Entry {
            title: tr(tab.title()),
            detail: String::new(),
            target: Target::Group(tab),
        })
        .collect();
    entries.extend(
        view::SETTING_ENTRIES
            .iter()
            .filter(|entry| match entry.title {
                "Notifications" => features.notifications,
                "Camera" => features.camera,
                _ => true,
            })
            .map(|entry| Entry {
                title: tr(entry.title),
                detail: tr(entry.note),
                target: Target::Group(entry.tab),
            }),
    );
    let mut control = |tab, title: &str, detail: String| {
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.title == tr(title) && entry.target == Target::Group(tab))
        {
            entry.detail.push_str(" · ");
            entry.detail.push_str(&detail);
        } else {
            entries.push(Entry {
                title: tr(title),
                detail,
                target: Target::Group(tab),
            });
        }
    };
    control(
        SettingsTab::Device,
        "Device name",
        view.settings.device_name.clone(),
    );
    control(
        SettingsTab::Device,
        "Language",
        peersus_core::config::Language::ALL
            .iter()
            .map(|language| language.name())
            .collect::<Vec<_>>()
            .join(" · "),
    );
    control(SettingsTab::Device, "Choose folder", tr("INCOMING FILES"));
    if cfg!(target_os = "android") {
        control(
            SettingsTab::Device,
            "Two fingers open Settings",
            tr(
                "Touch the screen with two fingers to bring Settings down from the top. It does nothing while Settings is already open.",
            ),
        );
    }
    if features.overlay {
        control(
            SettingsTab::Device,
            "Offers over other apps",
            tr(
                "Off: the list of the files of an offer opens in Peersus. Allow it to see the list over other apps.",
            ),
        );
    }
    if features.battery {
        control(
            SettingsTab::Device,
            "Battery",
            tr(
                "Limited: while the phone is idle the system may cut the network of the background service. Allow it in the battery settings.",
            ),
        );
    }
    if features.clipboard {
        control(
            SettingsTab::Device,
            "Background clipboard access",
            "Stellar Shizuku optional provider permission background synchronization".into(),
        );
    }
    control(
        SettingsTab::Appearance,
        "Appearance",
        [
            tr("System"),
            tr("Light"),
            tr("Dark"),
            tr("Custom"),
            tr("Accent colour"),
        ]
        .join(" · "),
    );
    control(SettingsTab::Appearance, "CUSTOM THEME", String::new());
    for (_, title) in crate::appearance::ROLES {
        control(SettingsTab::Appearance, title, "CUSTOM THEME".into());
    }
    control(
        SettingsTab::Appearance,
        "SAVED THEME LIBRARY",
        String::new(),
    );
    for title in view.settings.saved_themes.keys() {
        control(SettingsTab::Appearance, title, "SAVED THEME LIBRARY".into());
    }
    control(
        SettingsTab::Appearance,
        "IMPORT / EXPORT",
        "Import theme file · Export theme file · JSON".into(),
    );
    control(
        SettingsTab::Network,
        "Relay",
        [
            tr("Project relay"),
            tr("Public relays (n0)"),
            tr("My own relay"),
            tr("Direct connection is always tried first."),
            tr("Move up"),
            tr("Move down"),
        ]
        .join(" · "),
    );
    control(
        SettingsTab::About,
        crate::support::TITLE,
        [
            crate::support::DESCRIPTION,
            crate::support::PAYMENT_NOTE,
            crate::support::TERMS,
        ]
        .join(" "),
    );
    control(
        SettingsTab::About,
        "IMPORTANT — DATA AND SECURITY RISKS",
        crate::about::DISCLAIMER.join(" "),
    );
    for section in crate::about::SECTIONS {
        for credit in section.credits {
            control(
                SettingsTab::About,
                credit.name,
                format!(
                    "{} · {} · {}",
                    tr(section.title),
                    credit.licence,
                    tr(credit.purpose)
                ),
            );
        }
    }
    for plugin in &view.plugins {
        let plugin_name = plugin.id.as_str();
        entries.push(Entry {
            title: plugin_name.into(),
            detail: tr(view::plugin_word(plugin).0),
            target: Target::Plugin(plugin.id.clone()),
        });
        for (capability, description) in &plugin.manifest.capabilities {
            entries.push(Entry {
                title: capability.clone(),
                detail: format!("{plugin_name} · {description}"),
                target: Target::Plugin(plugin.id.clone()),
            });
        }
        if view.devices.is_empty() {
            for row in view::PermissionRows::of_in(
                language,
                plugin,
                &plugin.grantable.per_device,
                &peersus_core::plugins::Grants::default(),
            ) {
                entries.push(Entry {
                    title: row.permission.name,
                    detail: format!(
                        "{plugin_name} · {} · {}",
                        row.description,
                        tr("Pair a device to allow this plugin anything for it.")
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
                entries.push(Entry {
                    title: tr(&setting.label),
                    detail: format!(
                        "{context} · {} · {} · {choices}",
                        setting.id,
                        tr(setting.help.as_deref().unwrap_or(""))
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
                        && action.input == peersus_plugin_api::contributions::ActionInput::None
                })
            {
                entries.push(Entry {
                    title: tr(&action.label),
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
            for row in view::PermissionRows::of_in(language, plugin, offered, held) {
                entries.push(Entry {
                    title: row.permission.name.clone(),
                    detail: format!(
                        "{context} · {} · {}",
                        row.description,
                        tr(if row.allowed { "on" } else { "off" })
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
            entries.push(Entry {
                title: listing.plugin.as_str().to_owned(),
                detail: format!("{} · {}", listing.author, listing.description),
                target: Target::Plugin(listing.plugin.clone()),
            });
        }
    }
    entries
}

/// Each query word may match a different field, in any order. Exact/contiguous titles beat
/// descriptive context. Every matching result remains available; the UI bounds its viewport.
pub fn search(query: &str, view: &View, language: Language, features: Features) -> Vec<Entry> {
    if query.trim().is_empty() || query.chars().count() > 256 {
        return Vec::new();
    }
    let tokens: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
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
                        let exact = field.to_lowercase().contains(token);
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
    use peersus_core::model::{
        ActionState, ActionStatus, Grantable, PluginInfo, PluginStatus, Standing,
    };
    use peersus_plugin_api::{
        contributions::{Action, Setting, Source},
        manifest::Manifest,
    };

    fn plugin() -> PluginInfo {
        let mut manifest = Manifest::default();
        manifest.capabilities.insert(
            "documents.publish".into(),
            "Publish selected documents".into(),
        );
        manifest
            .capabilities
            .insert("hidden.power".into(), "Not offered by the owner".into());
        manifest.contributions.settings.push(Setting {
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
            input: peersus_plugin_api::contributions::ActionInput::None,
            target: Source {
                service: "documents".into(),
                method: "publish".into(),
            },
            capability: None,
            needs_device: false,
            destructive: false,
            timeout_ms: 1000,
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
        }
    }
    fn find(query: &str, view: &View) -> Vec<Entry> {
        search(query, view, Language::English, Features::default())
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
        assert!(!entries(&view, Language::English, Features::default()).iter().any(|entry| matches!(&entry.target, Target::Permission { permission, .. } if permission.name == "hidden.power")));
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
        });
        let everything = entries(&view, Language::English, Features::default());
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
                Language::English,
                Features {
                    camera: true,
                    ..Features::default()
                }
            )
            .iter()
            .any(|entry| entry.title == "Camera")
        );
        assert!(
            !find("Stellar Shizuku", &view)
                .iter()
                .any(|entry| entry.title == "Background clipboard access")
        );
        assert!(
            search(
                "Stellar Shizuku",
                &view,
                Language::English,
                Features {
                    clipboard: true,
                    ..Features::default()
                }
            )
            .iter()
            .any(|entry| entry.target == Target::Group(SettingsTab::Device)
                && entry.title == "Background clipboard access")
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
                Language::English,
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
                Language::Romanian,
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
