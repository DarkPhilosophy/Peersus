//! What the application offers extensions: its own manifest, the permissions that guard it with the words a person reads,
//! and the services each extension is offered for what it imported.
use super::*;
use crate::plugin_manager::NO_AGREEMENT;

/// Declare, in the application's own manifest, a capability a plugin declared, carrying the
/// plugin's own description of it.
///
/// The words the user reads when granting are the ones the plugin's author wrote, copied and
/// never rewritten here: the application cannot shorten, soften or invent what a permission
/// means. A capability the plugin did not declare is not declared on its behalf; a registered
/// plugin cannot reach that case, because a manifest naming an undeclared capability is
/// refused when it is validated.
fn declare(
    manifest: &mut peersus_api::manifest::Manifest,
    plugin: &crate::plugin_host::PluginSpec,
    capability: &str,
) {
    if let Some(description) = plugin.manifest.capabilities.get(capability) {
        manifest
            .capabilities
            .insert(capability.to_owned(), description.clone());
    }
}

/// The application's import of one service of one plugin, made empty the first time it is asked for.
fn import_of<'a>(
    manifest: &'a mut peersus_api::manifest::Manifest,
    provider: &str,
    service: &str,
    version: u32,
) -> &'a mut peersus_api::manifest::Import {
    let position = manifest
        .imports
        .iter()
        .position(|import| import.provider == provider && import.service == service)
        .unwrap_or_else(|| {
            manifest.imports.push(peersus_api::manifest::Import {
                provider: provider.to_owned(),
                service: service.to_owned(),
                version,
                methods: BTreeSet::new(),
                events: BTreeSet::new(),
            });
            manifest.imports.len() - 1
        });
    &mut manifest.imports[position]
}

/// The application's own hub manifest, derived from what plugins declare.
///
/// For every action a plugin offers, the application imports exactly that method from
/// exactly that plugin, so the hub can route the user's choice to it, and declares the
/// capability that action needs so the user can grant it. Nothing is added beyond what
/// plugins themselves declared, and nothing is granted. Whether a call is then *allowed*
/// is still the hub's decision: the plugin must list the application as a consumer of
/// that export and the capability must have been granted, so a plugin that does not want
/// to be driven from the interface simply does not list it.
pub(crate) fn host_manifest(plugins: &Registry) -> peersus_api::manifest::Manifest {
    let mut manifest = peersus_api::manifest::Manifest::default();
    for spec in plugins.iter() {
        for action in &spec.manifest.contributions.actions {
            let service = &action.target.service;
            let Some(export) = spec.manifest.exports.iter().find(|e| &e.service == service) else {
                continue;
            };
            import_of(&mut manifest, spec.id.as_str(), service, export.version)
                .methods
                .insert(action.target.method.clone());
            // Declaring a capability only makes it possible to be granted one. Nothing is
            // granted here: that stays an explicit decision of the user.
            if let Some(capability) = &action.capability {
                declare(&mut manifest, spec, capability);
            }
        }
        // What a plugin takes in from peers is called by the application too, so the hub
        // must be able to route that call: import `receive` from every plugin that exports
        // it, and declare the capability that guards it so the user can grant it. Nothing
        // is imported for a plugin that offers no `receive`.
        for export in &spec.manifest.exports {
            let Some(guard) = export.methods.get(crate::plugin_host::RECEIVE_METHOD) else {
                continue;
            };
            import_of(
                &mut manifest,
                spec.id.as_str(),
                &export.service,
                export.version,
            )
            .methods
            .insert(crate::plugin_host::RECEIVE_METHOD.to_owned());
            if let Some(capability) = &guard.capability {
                declare(&mut manifest, spec, capability);
            }
        }
        // A page's surface sends what the person does to its source method, so that method is
        // the application's to call, guarded as the page declares, exactly as for an action.
        for (source, capability) in spec
            .manifest
            .contributions
            .pages
            .iter()
            .filter_map(|page| {
                page.surface
                    .and(page.source.as_ref())
                    .map(|source| (source, page.capability.as_ref()))
            })
            .chain(
                spec.manifest
                    .contributions
                    .input
                    .iter()
                    .map(|input| (&input.source, Some(&input.capability))),
            )
        {
            let Some(export) = spec
                .manifest
                .exports
                .iter()
                .find(|e| e.service == source.service)
            else {
                continue;
            };
            import_of(
                &mut manifest,
                spec.id.as_str(),
                &source.service,
                export.version,
            )
            .methods
            .insert(source.method.clone());
            if let Some(capability) = capability {
                declare(&mut manifest, spec, capability);
            }
        }
    }
    // What the application offers plugins that asked for it. It obeys the rule it sets: the
    // permission that guards one of its methods is declared and described in its own manifest,
    // as a plugin's would be, and the words are the application's because it is the author.
    manifest.exports = host_exports(plugins);
    for &(service, capability, description) in HOST_CAPABILITIES {
        if manifest.exports.iter().any(|e| e.service == service) {
            manifest
                .capabilities
                .insert(capability.to_owned(), description.to_owned());
        }
    }
    manifest
}

/// Every permission that guards something the application offers extensions: the service it belongs to, its name, and what it lets
/// an extension do, in the words a person reads when granting it.
///
/// The one list. The application's manifest declares from it whichever belong to a service it exports, and whatever shows a
/// permission of the application asks [`host_capability_description`], so a permission cannot be offered here and be without words
/// on a screen.
pub const HOST_CAPABILITIES: &[(&str, &str, &str)] = &[
    (PEERS_SERVICE, PEERS_SEND_CAPABILITY, PEERS_SEND_DESCRIPTION),
    (
        PEERS_SERVICE,
        PEERS_WATCH_CAPABILITY,
        PEERS_WATCH_DESCRIPTION,
    ),
    (
        crate::display_service::SERVICE,
        crate::display_service::ARRANGE,
        crate::display_service::DESCRIPTION,
    ),
    (
        PLUGINS_SERVICE,
        PLUGINS_RESTART_CAPABILITY,
        PLUGINS_RESTART_DESCRIPTION,
    ),
    (
        crate::transfer_events::TRANSFERS_SERVICE,
        crate::transfer_events::TRANSFERS_WATCH_CAPABILITY,
        crate::transfer_events::TRANSFERS_WATCH_DESCRIPTION,
    ),
    (
        crate::transfer_events::TRANSFERS_SERVICE,
        crate::transfer_events::TRANSFERS_NAMES_CAPABILITY,
        crate::transfer_events::TRANSFERS_NAMES_DESCRIPTION,
    ),
    (
        crate::status_service::STATUS_SERVICE,
        crate::status_service::STATUS_DEFINE_CAPABILITY,
        crate::status_service::STATUS_DEFINE_DESCRIPTION,
    ),
    (
        crate::status_service::STATUS_SERVICE,
        crate::status_service::STATUS_SET_CAPABILITY,
        crate::status_service::STATUS_SET_DESCRIPTION,
    ),
    (
        crate::status_service::STATUS_SERVICE,
        crate::status_service::STATUS_WATCH_CAPABILITY,
        crate::status_service::STATUS_WATCH_DESCRIPTION,
    ),
    (
        crate::platform_services::FILES_SERVICE,
        crate::platform_services::FILES_CAPABILITY,
        crate::platform_services::FILES_DESCRIPTION,
    ),
    (
        crate::platform_services::HTTP_SERVICE,
        crate::platform_services::HTTP_CAPABILITY,
        crate::platform_services::HTTP_DESCRIPTION,
    ),
    (
        crate::platform_services::CLIPBOARD_SERVICE,
        crate::platform_services::CLIPBOARD_READ_CAPABILITY,
        crate::platform_services::CLIPBOARD_READ_DESCRIPTION,
    ),
    (
        crate::platform_services::CLIPBOARD_SERVICE,
        crate::platform_services::CLIPBOARD_WRITE_CAPABILITY,
        crate::platform_services::CLIPBOARD_WRITE_DESCRIPTION,
    ),
    (
        crate::platform_services::CLIPBOARD_SERVICE,
        crate::platform_services::CLIPBOARD_WATCH_CAPABILITY,
        crate::platform_services::CLIPBOARD_WATCH_DESCRIPTION,
    ),
    (
        crate::platform_services::INPUT_SERVICE,
        crate::platform_services::INPUT_CAPABILITY,
        crate::platform_services::INPUT_DESCRIPTION,
    ),
    (
        crate::platform_services::NOTIFY_SERVICE,
        crate::platform_services::NOTIFY_CAPABILITY,
        crate::platform_services::NOTIFY_DESCRIPTION,
    ),
    (
        crate::platform_services::location::SERVICE,
        crate::platform_services::location::CAPABILITY,
        crate::platform_services::location::PERMISSION,
    ),
    (
        crate::system_readings::SERVICE,
        crate::system_readings::PROCESSOR.capability,
        crate::system_readings::PROCESSOR.permission,
    ),
    (
        crate::system_readings::SERVICE,
        crate::system_readings::GRAPHICS.capability,
        crate::system_readings::GRAPHICS.permission,
    ),
    (
        crate::system_readings::SERVICE,
        crate::system_readings::MEMORY.capability,
        crate::system_readings::MEMORY.permission,
    ),
    (
        crate::system_readings::SERVICE,
        crate::system_readings::STORAGE.capability,
        crate::system_readings::STORAGE.permission,
    ),
    (
        crate::system_readings::SERVICE,
        crate::system_readings::NETWORK.capability,
        crate::system_readings::NETWORK.permission,
    ),
    (
        crate::system_readings::SERVICE,
        crate::system_readings::BATTERY.capability,
        crate::system_readings::BATTERY.permission,
    ),
    (
        crate::display_service::SERVICE,
        crate::display_service::WATCH,
        crate::display_service::WATCH_DESCRIPTION,
    ),
    (
        PLUGINS_SERVICE,
        PLUGINS_WATCH_CAPABILITY,
        PLUGINS_WATCH_DESCRIPTION,
    ),
    (
        PLATFORM_SERVICE,
        PLATFORM_READ_CAPABILITY,
        PLATFORM_READ_DESCRIPTION,
    ),
    (
        LANGUAGE_SERVICE,
        LANGUAGE_READ_CAPABILITY,
        LANGUAGE_READ_DESCRIPTION,
    ),
];

/// What a permission of the application lets an extension do, by the permission's name; `None` for a name the application does not
/// declare.
pub fn host_capability_description(capability: &str) -> Option<&'static str> {
    HOST_CAPABILITIES
        .iter()
        .find(|(_, name, _)| *name == capability)
        .map(|(_, _, description)| *description)
}

/// The service the application offers plugins about the paired devices.
pub const PEERS_SERVICE: &str = "peersus.peers";
/// Read-only platform facts, not handles, paths or active privilege grants.
pub const PLATFORM_SERVICE: &str = "peersus.platform";
/// Caller-scoped effective configuration, never a dump of saved preferences.
pub const CONFIG_SERVICE: &str = "peersus.config";
/// The capability that guards `send`. It names no device: the user grants it to a plugin for
/// each device separately, exactly as for what a device may do through a plugin.
pub const PEERS_SEND_CAPABILITY: &str = "peers.send";
/// What that permission lets a plugin do, in the words a person reads when granting it. One
/// copy, because the application's manifest and anything that shows the permission must say
/// the same thing.
pub const PEERS_SEND_DESCRIPTION: &str =
    "Send data to a paired device you choose. It cannot see any device's key.";
/// The capability that guards `list`, granted for each device separately: a plugin is told a device exists, its name and
/// whether it is online or asleep, only for a device the person allowed it to see.
pub const PEERS_WATCH_CAPABILITY: &str = "peers.watch";
pub const PEERS_WATCH_DESCRIPTION: &str = "See this paired device: its name, and whether it is online or asleep. It cannot see the device's key or send it anything.";

/// The service the application offers plugins about the other plugins.
pub const PLUGINS_SERVICE: &str = "peersus.plugins";
/// The capability that guards `restart`. Unlike `peers.send` it is about no device at all, so
/// the user grants it to a plugin once, whether or not any device is paired.
pub const PLUGINS_RESTART_CAPABILITY: &str = "plugins.restart";
/// What that permission lets a plugin do. It is among the most a plugin can be allowed, so
/// the words say how much, in the same copy for the manifest and for whatever shows it.
pub const PLUGINS_RESTART_DESCRIPTION: &str = "Interrupt and restart ANY plugin, including ones you did not install for this purpose. \
     It cannot read their data or change what they are allowed to do.";

/// The event `peersus.plugins` publishes when a plugin's state changes.
pub const PLUGINS_STATE_CHANGED: &str = "state-changed";
/// The capability that guards `list` and `state-changed`: which other extensions are installed is the person's to tell.
pub const PLUGINS_WATCH_CAPABILITY: &str = "plugins.watch";
pub const PLUGINS_WATCH_DESCRIPTION: &str = "See which other extensions are installed here and whether each is running. It cannot read their data or permissions.";
/// The capability that guards `peersus.platform` `inspect`.
pub const PLATFORM_READ_CAPABILITY: &str = "platform.read";
pub const PLATFORM_READ_DESCRIPTION: &str =
    "See which operating system and processor architecture this device has.";
/// The capability that guards `peersus.language` `get` and `changed`.
pub const LANGUAGE_READ_CAPABILITY: &str = "language.read";
pub const LANGUAGE_READ_DESCRIPTION: &str =
    "See the language you use the application in, to show its own words in it.";

/// The application's methods that need no permission, and why each: every one of them is about the caller itself, and
/// tells it nothing of this device, the person, another device or another extension. Every other method the application
/// offers is guarded by a permission the person grants (see [`HOST_CAPABILITIES`]); a test holds that nothing else is
/// offered unguarded, so a new method cannot be added without a permission by accident.
pub const CALLER_OWN_METHODS: &[(&str, &str)] = &[
    // Its own effective settings, and the devices it was granted something for.
    (CONFIG_SERVICE, "get"),
    // Where its own declared pages are also offered.
    (crate::page_offers::SERVICE, "set"),
    // Which of its own declared actions are offered now.
    (crate::page_offers::ACTIONS_SERVICE, "withhold"),
    // Whether the person accepted its own licence and notice.
    (EXTENSION_SERVICE, "tos"),
    // Whether the person accepted the warning about installing from outside the store, which it already needed to be here.
    (EXTENSION_SERVICE, "outside_store"),
    // The statuses it made itself.
    (
        crate::status_service::STATUS_SERVICE,
        crate::status_service::STATUS_OWN,
    ),
];

/// What an extension can ask of the application about its own agreement: a single answer, `accepted`, true or false. It
/// cannot see the pages, change them, or press the buttons that accept them (`extension.tos.accepted`).
pub const EXTENSION_SERVICE: &str = "peersus.extension";

/// The person's language, for an extension that shows words of its own: `get` answers it, and `changed` is published when it
/// changes. An extension translates its own words; whatever it does not translate it shows as it wrote it.
pub const LANGUAGE_SERVICE: &str = "peersus.language";
/// The event [`LANGUAGE_SERVICE`] publishes when the person's language changes.
pub const LANGUAGE_CHANGED: &str = "changed";

/// The digest of the licences and notice a package carries, or the refusal that it carries none.
pub(super) fn agreement_digest(
    manifest: &peersus_api::manifest::Manifest,
) -> Result<String, &'static str> {
    manifest
        .agreements
        .as_ref()
        .map(peersus_core::agreements::digest)
        .ok_or(NO_AGREEMENT)
}

/// The plugins that asked the application for version 1 of one of its services.
pub(super) fn consumers_of(plugins: &Registry, service: &str) -> BTreeSet<String> {
    consumers_among(plugins.iter(), service)
}

/// [`consumers_of`] for any set of plugins, registered or not.
fn consumers_among<'a>(
    plugins: impl IntoIterator<Item = &'a PluginSpec>,
    service: &str,
) -> BTreeSet<String> {
    plugins
        .into_iter()
        .filter(|spec| {
            spec.manifest.imports.iter().any(|import| {
                import.provider == HOST && import.service == service && import.version == 1
            })
        })
        .map(|spec| spec.id.as_str().to_owned())
        .collect()
}

/// What the application exports to plugins, derived from which of them asked for it.
///
/// Each service is offered only to the plugins that imported it; with no such plugin that
/// service is not offered at all, and the two are independent. Only the methods the
/// application really has are offered. `peersus.peers` `list` shows a device's name and
/// whether it is online but never a key; `peersus.plugins` `list` shows a plugin's id and
/// state but never its manifest, permissions or location. `send` and `restart` are guarded,
/// so a plugin that asks for them can still do nothing until the user grants them.
pub(crate) fn host_exports(plugins: &Registry) -> Vec<peersus_api::manifest::Export> {
    exports_for(&plugins.iter().collect::<Vec<_>>())
}

/// [`host_exports`] for any set of plugins: what one plugin alone would be offered is `exports_for(&[spec])`, with no registry
/// to build and no manifest to check again.
pub(crate) fn exports_for(plugins: &[&PluginSpec]) -> Vec<peersus_api::manifest::Export> {
    use peersus_api::manifest::{Export, Member};
    use std::collections::BTreeMap;
    let mut exports = Vec::new();
    let consumers = consumers_among(plugins.iter().copied(), crate::display_service::SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: crate::display_service::SERVICE.into(),
            version: 1,
            description: "Scoped desktop geometry and the person's saved arrangement.".into(),
            consumers,
            methods: BTreeMap::from([
                ("inspect".into(), Member { capability: Some(crate::display_service::WATCH.into()), description: "Local display rectangles and the paired desktops this extension may see (peers.watch for each), identified only by caller-scoped aliases.".into() }),
                ("arrange".into(), Member { capability: Some(crate::display_service::ARRANGE.into()), description: crate::display_service::DESCRIPTION.into() }),
            ]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), PLATFORM_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: PLATFORM_SERVICE.into(),
            version: 1,
            description:
                "Portable operating system and architecture; not facilities or permission grants."
                    .into(),
            consumers,
            methods: BTreeMap::from([(
                "inspect".into(),
                Member {
                    capability: Some(PLATFORM_READ_CAPABILITY.into()),
                    description: "Reports conservative platform facts only.".into(),
                },
            )]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), EXTENSION_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: EXTENSION_SERVICE.into(),
            version: 1,
            description: "Whether the person accepted this extension's own licences and notice (`extension.tos.accepted`); nothing more."
                .into(),
            consumers,
            methods: BTreeMap::from([
                (
                    "tos".into(),
                    Member {
                        capability: None,
                        description: "Answers `{accepted: true|false}` for the caller only; it cannot read, change or give an acceptance."
                            .into(),
                    },
                ),
                (
                    "outside_store".into(),
                    Member {
                        capability: None,
                        description: "Answers `{accepted: true|false}`: whether the person accepted, once, the application's warning about installing an extension from outside the store (`extension.tos.outside_store`); it cannot change or give it."
                            .into(),
                    },
                ),
            ]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), LANGUAGE_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: LANGUAGE_SERVICE.into(),
            version: 1,
            description: "The person's language, as a tag (`ro`, `pt-br`), for an extension's own words.".into(),
            consumers,
            methods: BTreeMap::from([(
                "get".into(),
                Member {
                    capability: Some(LANGUAGE_READ_CAPABILITY.into()),
                    description: "Answers `{language: TAG}`: the language the person chose, else the device's, else `en`."
                        .into(),
                },
            )]),
            events: BTreeMap::from([(
                LANGUAGE_CHANGED.to_owned(),
                Member {
                    capability: Some(LANGUAGE_READ_CAPABILITY.into()),
                    description: "The person's language changed; carries `{language: TAG}`.".into(),
                },
            )]),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), crate::system_readings::SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: crate::system_readings::SERVICE.into(),
            version: 1,
            description: crate::system_readings::DESCRIPTION.into(),
            consumers,
            // One method per part of the device, each behind the permission for that part alone.
            methods: crate::system_readings::PARTS
                .iter()
                .map(|part| {
                    (
                        part.method.to_owned(),
                        Member {
                            capability: Some(part.capability.to_owned()),
                            description: part.answers.to_owned(),
                        },
                    )
                })
                .collect(),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), CONFIG_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: CONFIG_SERVICE.into(),
            version: 1,
            description: "Reads only the caller's declared effective settings and peer aliases."
                .into(),
            consumers,
            methods: BTreeMap::from([(
                "get".into(),
                Member {
                    capability: None,
                    description: "Returns validated settings for this caller only.".into(),
                },
            )]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), crate::page_offers::SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: crate::page_offers::SERVICE.into(),
            version: 1,
            description: "Offers the caller's declared pages in additional navigation locations."
                .into(),
            consumers,
            methods: BTreeMap::from([(
                "set".into(),
                Member {
                    capability: None,
                    description: "Replaces only this caller's additional page placements.".into(),
                },
            )]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), crate::page_offers::ACTIONS_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: crate::page_offers::ACTIONS_SERVICE.into(),
            version: 1,
            description: "Lets the caller take back, for now, actions its own manifest declares."
                .into(),
            consumers,
            methods: BTreeMap::from([(
                "withhold".into(),
                Member {
                    capability: None,
                    description: "Replaces the set of this caller's own actions that are not offered now; an empty set offers them all.".into(),
                },
            )]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), PEERS_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: PEERS_SERVICE.to_owned(),
            version: 1,
            description: "Lets a plugin see which paired devices exist and send them data. It \
                          never shows a device's key."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (
                    "list".to_owned(),
                    Member {
                        capability: Some(PEERS_WATCH_CAPABILITY.to_owned()),
                        description:
                            "Lists the paired devices the person allowed this plugin for, by name, whether each is online, asleep or \
                                      away, and whether this plugin is allowed for it."
                                .to_owned(),
                    },
                ),
                (
                    "send".to_owned(),
                    Member {
                        capability: Some(PEERS_SEND_CAPABILITY.to_owned()),
                        description: "Sends data to one paired device the user chose.".to_owned(),
                    },
                ),
            ]),
            events: BTreeMap::new(),
        });
    }
    let consumers = consumers_among(plugins.iter().copied(), PLUGINS_SERVICE);
    if !consumers.is_empty() {
        exports.push(Export {
            service: PLUGINS_SERVICE.to_owned(),
            version: 1,
            description: "Lets a plugin see which plugins exist and what state each is in, and \
                          restart one. It never shows what a plugin declares or is allowed."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (
                    "list".to_owned(),
                    Member {
                        capability: Some(PLUGINS_WATCH_CAPABILITY.to_owned()),
                        description: "Lists plugins by id and state.".to_owned(),
                    },
                ),
                (
                    "restart".to_owned(),
                    Member {
                        capability: Some(PLUGINS_RESTART_CAPABILITY.to_owned()),
                        description: "Restarts one plugin the caller names.".to_owned(),
                    },
                ),
            ]),
            events: BTreeMap::from([(
                PLUGINS_STATE_CHANGED.to_owned(),
                Member {
                    capability: Some(PLUGINS_WATCH_CAPABILITY.to_owned()),
                    description: "A plugin's state changed; carries its id and new state."
                        .to_owned(),
                },
            )]),
        });
    }
    let consumers = consumers_among(
        plugins.iter().copied(),
        crate::transfer_events::TRANSFERS_SERVICE,
    );
    if !consumers.is_empty() {
        exports.push(Export {
            service: crate::transfer_events::TRANSFERS_SERVICE.to_owned(),
            version: 1,
            description: "Tells a plugin when a transfer changes, and what is going now: which one and what happened. A plugin only reads and listens: it \
                          cannot accept, refuse or cancel anything, and never sees where a file is saved, its contents, a device's key or history."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (
                    crate::transfer_events::TRANSFERS_ACTIVE.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_WATCH_CAPABILITY.to_owned()),
                        description: "The transfers going now, as `changed` says them: no name. For a plugin that starts late or restarts.".to_owned(),
                    },
                ),
                (
                    crate::transfer_events::TRANSFERS_ACTIVE_NAMED.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_NAMES_CAPABILITY.to_owned()),
                        description: "The transfers going now, as `changed-named` says them, with names; only for plugins allowed to see names.".to_owned(),
                    },
                ),
            ]),
            events: BTreeMap::from([
                (
                    crate::transfer_events::TRANSFERS_CHANGED.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_WATCH_CAPABILITY.to_owned()),
                        description: "A transfer started, moved, finished, was cancelled or failed; carries its id, direction, state and progress, and no name."
                            .to_owned(),
                    },
                ),
                (
                    crate::transfer_events::TRANSFERS_CHANGED_NAMED.to_owned(),
                    Member {
                        capability: Some(crate::transfer_events::TRANSFERS_NAMES_CAPABILITY.to_owned()),
                        description: "The same, with the name of the file and the name of the device; for the plugins the person allowed to see names."
                            .to_owned(),
                    },
                ),
            ]),
        });
    }
    let consumers = consumers_among(
        plugins.iter().copied(),
        crate::status_service::STATUS_SERVICE,
    );
    if !consumers.is_empty() {
        use crate::status_service as status;
        let guarded = |capability: &str, description: &str| Member {
            capability: Some(capability.to_owned()),
            description: description.to_owned(),
        };
        exports.push(Export {
            service: status::STATUS_SERVICE.to_owned(),
            version: 1,
            // At most `manifest::MAX_DESCRIPTION` characters, as any export: a longer one makes the application's own manifest one the hub refuses.
            description: "Lets a plugin make statuses of its own for this device, turn them on and off, and read what paired devices say of \
                          themselves. Online, sleeping and offline are the application's words. A status says whose it is; a plugin turns on \
                          only what it made and never sees a device's key."
                .to_owned(),
            consumers,
            methods: BTreeMap::from([
                (status::STATUS_DEFINE.to_owned(), guarded(status::STATUS_DEFINE_CAPABILITY, "Creates or rewords one of the caller's own statuses. It says nothing about the device until it is turned on.")),
                (status::STATUS_SET.to_owned(), guarded(status::STATUS_SET_CAPABILITY, "Turns one of the caller's own statuses on: every paired device then shows it.")),
                (status::STATUS_CLEAR.to_owned(), guarded(status::STATUS_SET_CAPABILITY, "Turns one of the caller's own statuses off.")),
                (status::STATUS_LIST.to_owned(), guarded(status::STATUS_WATCH_CAPABILITY, "What every paired device says of itself now: how it is reachable and the statuses it shows, each with whose it is.")),
                (status::STATUS_OWN.to_owned(), Member { capability: None, description: "The statuses the caller made itself and which are on.".to_owned() }),
            ]),
            events: BTreeMap::from([(
                status::STATUS_CHANGED.to_owned(),
                guarded(status::STATUS_WATCH_CAPABILITY, "What a paired device says of itself changed. It carries nothing: `list` says what is true now, of the devices the caller may see."),
            )]),
        });
    }
    for (service, description) in [
        (
            crate::platform_services::FILES_SERVICE,
            crate::platform_services::FILES_DESCRIPTION,
        ),
        (
            crate::platform_services::HTTP_SERVICE,
            crate::platform_services::HTTP_DESCRIPTION,
        ),
        (
            crate::platform_services::CLIPBOARD_SERVICE,
            crate::platform_services::CLIPBOARD_DESCRIPTION,
        ),
        (
            crate::platform_services::INPUT_SERVICE,
            crate::platform_services::INPUT_DESCRIPTION,
        ),
        (
            crate::platform_services::NOTIFY_SERVICE,
            crate::platform_services::NOTIFY_DESCRIPTION,
        ),
        (
            crate::platform_services::location::SERVICE,
            crate::platform_services::location::DESCRIPTION,
        ),
    ] {
        let consumers = consumers_among(plugins.iter().copied(), service);
        if consumers.is_empty() {
            continue;
        }
        let methods = crate::platform_services::METHODS
            .iter()
            .filter(|(offered, _, _, _)| *offered == service)
            .map(|(_, method, capability, description)| {
                (
                    (*method).to_owned(),
                    Member {
                        capability: Some((*capability).to_owned()),
                        description: (*description).to_owned(),
                    },
                )
            })
            .collect();
        let events = crate::platform_services::EVENTS
            .iter()
            .filter(|(offered, _, _, _)| *offered == service)
            .map(|(_, event, capability, description)| {
                (
                    (*event).to_owned(),
                    Member {
                        capability: Some((*capability).to_owned()),
                        description: (*description).to_owned(),
                    },
                )
            })
            .collect();
        exports.push(Export {
            service: service.to_owned(),
            version: 1,
            description: description.to_owned(),
            consumers,
            methods,
            events,
        });
    }
    exports
}
