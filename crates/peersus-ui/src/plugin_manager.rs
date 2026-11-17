//! The extensions in Settings: each with its pages, settings, actions and the permissions the owner offers, per device.
use super::*;

/// The switches of one plugin for one scope, either one paired device (`peer`) or no device at all.
/// One row for each permission the OWNER offers in that scope, on if it is held. A switch names ONE
/// permission and what it should become; the owner applies that to what it holds at that moment, so
/// two quick flips cannot undo each other and a flip never takes away something it did not touch.
pub(crate) fn permission_block(
    link: &Link,
    plugin: &PluginInfo,
    offered: &[peersus_api::permission::Permission],
    held: &peersus_core::plugins::Grants,
    peer: Option<peersus_core::model::PeerId>,
    palette: Palette,
) -> Element {
    let mut block = rect().width(Size::fill()).spacing(8.);
    for row in view::PermissionRows::of_in(i18n::language(), plugin, offered, held) {
        let (link, id, permission, peer) = (
            link.clone(),
            plugin.id.clone(),
            row.permission.clone(),
            peer.clone(),
        );
        let target = settings_search::Target::Permission {
            plugin: id.clone(),
            permission: permission.clone(),
            peer: peer.clone(),
        };
        block = block.child(settings_navigation::anchor(
            target,
            rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .spacing(12.)
                .child(
                    rect()
                        .width(Size::flex(1.))
                        .child(mono(row.permission.name.clone(), palette.send, 11.))
                        .child(
                            label()
                                .text(row.description.clone())
                                .color(palette.muted)
                                .font_size(13.),
                        ),
                )
                .child(Switch::new(WritableSetter::new(
                    row.allowed,
                    move |value: bool| {
                        link.send(Command::Allow {
                            plugin: id.clone(),
                            peer: peer.clone(),
                            permission: permission.clone(),
                            on: value,
                        });
                    },
                ))),
            palette,
        ));
    }
    block.into()
}

/// The extension a store link asked for (`peersus://store/<id>`), from the owner's `StoreState::asked`: the window opens the
/// Extensions tab of Settings, and this page, once the store has answered, goes to that extension's row and, when it is listed
/// and neither installed nor waiting, starts its Install (which still asks for its licence and notice). Taken once.
#[derive(Clone, Copy)]
pub(crate) struct StoreLink(pub(crate) State<Option<peersus_core::plugins::PluginId>>);

/// The installed plugins as cards with what the person needs to manage them: the state, a switch
/// that turns a plugin off or on, a restart, a removal that asks twice, and what each one can do.
/// The one place plugins are listed is the Extensions tab of Settings.
#[derive(PartialEq)]
pub(crate) struct PluginManager {
    pub(crate) link: Link,
    pub(crate) view: View,
    pub(crate) pictures: shelf::Held,
    pub(crate) palette: Palette,
    pub(crate) compact: bool,
}

impl Component for PluginManager {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut asking = use_state(view::RemoveAsk::default);
        let mut opened = use_state(|| None::<peersus_core::plugins::PluginId>);
        let mut store_query = use_state(String::new);
        // Extensions installed while this page is open, newest first. The first time one appears it is put on top of the list and the
        // page is moved to it, so the person sees at once what they installed.
        let mut recent = use_state(Vec::<peersus_core::plugins::PluginId>::new);
        let mut seen = use_state(|| None::<Vec<peersus_core::plugins::PluginId>>);
        // The store row's Install or Update asked for this package: once the owner has read it, its licence and notice open at
        // once in the same place, so the person presses Install once. An update the owner staged by itself waits for them.
        let mut wanted = use_state(|| None::<peersus_core::plugins::PluginId>);
        let ask = try_consume_context::<agreement_overlay::Ask>();
        let staged_from_store: Vec<_> = self
            .view
            .staged
            .iter()
            .filter(|staged| staged.from_store)
            .map(|staged| staged.id.clone())
            .collect();
        use_side_effect_with_deps(&staged_from_store, move |staged| {
            let Some(plugin) = wanted.peek().clone() else {
                return;
            };
            if staged.contains(&plugin) {
                wanted.set(None);
                if let Some(agreement_overlay::Ask(mut asked)) = ask {
                    asked.set(Some(plugin));
                }
            }
        });
        let navigation = try_consume_context::<settings_navigation::Navigation>();
        // A store link waits until the store has answered, so that whether its extension is listed is known.
        let store_link = try_consume_context::<StoreLink>();
        let answered = !matches!(
            self.view.store.status,
            peersus_core::model::StoreStatus::Idle | peersus_core::model::StoreStatus::Loading
        );
        let linked = store_link
            .and_then(|StoreLink(asked)| asked.read().clone())
            .filter(|_| answered)
            .map(|plugin| {
                let listed = self
                    .view
                    .store
                    .listings
                    .iter()
                    .any(|listing| listing.plugin == plugin);
                let here = self
                    .view
                    .plugins
                    .iter()
                    .chain(&self.view.staged)
                    .any(|info| info.id == plugin);
                (plugin, listed && !here)
            });
        let link_for_store = self.link.clone();
        use_side_effect_with_deps(&linked, move |linked| {
            let Some((plugin, install)) = linked.clone() else {
                return;
            };
            if let Some(StoreLink(mut asked)) = store_link {
                asked.set(None);
            }
            // A search typed earlier could hide the row the link is for.
            store_query.set(String::new());
            if let Some(mut navigation) = navigation {
                navigation
                    .requested
                    .set(Some(settings_search::Target::Plugin(plugin.clone())));
            }
            if install {
                wanted.set(Some(plugin.clone()));
                link_for_store.send(Command::StoreInstall { plugin });
            }
        });
        // Installed extensions and packages waiting for confirmation: a package chosen from a file is first shown for review, and the
        // page moves to it then; once confirmed it keeps its place at the top.
        let installed_ids: Vec<_> = self
            .view
            .staged
            .iter()
            .chain(&self.view.plugins)
            .map(|plugin| plugin.id.clone())
            .fold(Vec::new(), |mut ids, id| {
                if !ids.contains(&id) {
                    ids.push(id);
                }
                ids
            });
        use_side_effect_with_deps(&installed_ids, move |installed| {
            let appeared: Vec<_> = match seen.peek().as_ref() {
                Some(before) => installed
                    .iter()
                    .filter(|id| !before.contains(id))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            if let Some(newest) = appeared.first().cloned() {
                let mut list = recent.write();
                for id in appeared.iter().rev() {
                    list.retain(|known| known != id);
                    list.insert(0, id.clone());
                }
                drop(list);
                if let Some(mut navigation) = navigation {
                    navigation
                        .requested
                        .set(Some(settings_search::Target::Plugin(newest)));
                }
            }
            seen.set(Some(installed.clone()));
        });
        let mut list = rect().width(Size::fill()).spacing(10.);
        #[cfg(not(target_os = "android"))]
        let mut bundles = use_state(Vec::<peersus_app::discovery::Bundle>::new);
        #[cfg(not(target_os = "android"))]
        use_hook(move || {
            let (send, receive) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let found = std::env::current_exe()
                    .ok()
                    .map(|path| peersus_app::discovery::bundles_near(&path))
                    .unwrap_or_default();
                let _ = send.send(found);
            });
            spawn(async move {
                if let Ok(found) = receive.await {
                    bundles.set(found);
                }
            });
        });
        // Catalog membership enriches the extension's existing card; it is not a second list.
        let typed = store_query.read().clone();
        let rows =
            view::extension_rows(&self.view.plugins, &self.view.store, &typed, &recent.read());
        let acting = view::store_can_act(&self.view.store);
        let refresh_link = self.link.clone();
        let install_link = self.link.clone();
        let drop_link = self.link.clone();
        let mut refused = use_state(|| None::<&'static str>);
        // Whether the notice before the file dialog is on show.
        let mut file_notice = use_state(|| false);
        #[cfg(target_os = "android")]
        let plugin_picker_for_notice = try_consume_context::<link::PluginPicker>();
        let install_link_for_notice = self.link.clone();
        let choose_file = EventHandler::new(move |()| {
            file_notice.set(false);
            let link = install_link_for_notice.clone();
            #[cfg(target_os = "android")]
            let plugin_picker = plugin_picker_for_notice.clone();
            spawn(async move {
                #[cfg(not(target_os = "android"))]
                // No filter: every file is shown. A filter would hide a package
                // whose name does not end as expected, with no word about why, and a
                // file that is not an archive is refused by the owner in plain words.
                if let Some(file) = rfd::AsyncFileDialog::new()
                    .set_title(i18n::tr("Choose a plugin archive (.tar, .tar.gz, .tgz)"))
                    .pick_file()
                    .await
                    && let Some(source) = peersus_core::model::SourceDir::new(file.path())
                {
                    link.send(Command::StageInstall { source });
                }
                #[cfg(target_os = "android")]
                {
                    let _ = &link;
                    match plugin_picker
                        .as_ref()
                        .ok_or("The plugin picker is unavailable")
                        .and_then(|picker| (picker.0)())
                    {
                        Ok(()) => refused.set(None),
                        Err(words) => refused.set(Some(words)),
                    }
                }
            });
        });
        let accepted_before = self.view.settings.outside_store_risk_accepted;
        let accept_link = self.link.clone();
        list = list.child(
            rect()
                .width(Size::fill())
                .spacing(8.)
                .maybe(!self.compact, |row| {
                    row.horizontal()
                        .cross_align(Alignment::center())
                        .content(Content::flex())
                })
                .child(
                    rect()
                        .width(if self.compact {
                            Size::fill()
                        } else {
                            Size::flex(1.)
                        })
                        .child(
                            field::field(field::Profile::SEARCH, store_query)
                                .placeholder(i18n::tr("Search extensions"))
                                .width(Size::fill()),
                        ),
                )
                .child(
                    Button::new()
                        .filled()
                        .on_press({
                            let choose_file = choose_file.clone();
                            move |_| {
                                if install_link.read_only() {
                                    refused.set(Some(link::READ_ONLY));
                                    return;
                                }
                                // What it means to install from a file is said before the file dialog, and once: when it was accepted the
                                // dialog opens at once.
                                if accepted_before {
                                    choose_file.call(());
                                } else {
                                    file_notice.set(true);
                                }
                            }
                        })
                        .child(format!("+ {}", i18n::tr("Install from file"))),
                )
                .child(
                    Button::new()
                        .enabled(acting)
                        .on_press(move |_| refresh_link.send(Command::StoreRefresh {}))
                        .child(i18n::tr("Refresh")),
                ),
        );
        if *file_notice.read() {
            let palette = self.palette;
            list = list.child(
                rect()
                    .width(Size::fill())
                    .padding(16.)
                    .spacing(10.)
                    .corner_radius(12.)
                    .background(palette.surface)
                    .border(
                        Border::new()
                            .fill(palette.border)
                            .width(1.)
                            .alignment(BorderAlignment::Inner),
                    )
                    .child(
                        label()
                            .text(i18n::tr("Before you choose a file"))
                            .color(palette.text)
                            .font_size(17.)
                            .font_weight(FontWeight::BOLD),
                    )
                    .child(
                        label()
                            .text(i18n::tr("Nobody at Peersus has reviewed it, signed it or promised that it is safe. It runs with your access on this device and can read and send whatever you let it reach."))
                            .color(palette.text)
                            .font_size(14.),
                    )
                    .child(
                        label()
                            .text(i18n::tr("The Peersus project cannot take on the risk of an extension from outside its store, and is not responsible for what it does to you, your files or your devices. The risk is yours."))
                            .color(palette.text)
                            .font_size(14.),
                    )
                    .child(
                        label()
                            .text(i18n::tr("Prefer the store: what is listed there was reviewed before it was offered. If you do not know who made this package, do not install it."))
                            .color(palette.text)
                            .font_size(14.),
                    )
                    .child(
                        rect()
                            .horizontal()
                            .spacing(10.)
                            .child(
                                Button::new()
                                    .filled()
                                    .on_press(move |_| {
                                        // Said once, and kept: from now on installing from a file shows the extension's licence and notice and
                                        // no more of this.
                                        accept_link.send(Command::PatchSettings {
                                            patch: Box::new(SettingsPatch {
                                                outside_store_risk_accepted: Some(true),
                                                ..Default::default()
                                            }),
                                        });
                                        choose_file.call(());
                                    })
                                    .child(i18n::tr("I understand and accept the risk")),
                            )
                            .child(
                                Button::new()
                                    .on_press(move |_| file_notice.set(false))
                                    .child(i18n::tr("Cancel")),
                            ),
                    ),
            );
        }
        #[cfg(feature = "private-store")]
        {
            list = list.child(private_store::Configuration {
                link: self.link.clone(),
                palette,
            });
        }
        if let Some(words) = view::store_notice(&self.view.store) {
            list = list.child(
                label()
                    .text(i18n::tr(words))
                    .color(palette.muted)
                    .font_size(13.),
            );
        }
        if rows.is_empty() && !typed.trim().is_empty() {
            list = list.child(
                label()
                    .text(i18n::tr("No extension matches your search."))
                    .color(palette.muted)
                    .font_size(13.),
            );
        }
        // One way to install. A package is a folder or one archive of one; the person does not say
        // which, the owner looks at the path. The button opens the system's file dialog, which is
        // where an archive is found; a folder is dropped on this page, because the system's dialogs
        // cannot pick a folder and a file in the same window. Both send the same command and nothing
        // is installed by either: the owner reads the package and shows it below first.
        list = list.child(
            rect()
                .width(Size::fill())
                .spacing(6.)
                .on_file_drop(move |e: Event<FileEventData>| {
                    if drop_link.read_only() {
                        refused.set(Some(link::READ_ONLY));
                        return;
                    }
                    match view::Install::of_drop(e.file_paths.clone(), view::DropKind::of) {
                        view::Install::Take(path) => {
                            refused.set(None);
                            if let Some(source) = peersus_core::model::SourceDir::new(path) {
                                drop_link.send(Command::StageInstall { source });
                            }
                        }
                        view::Install::Refuse(words) => refused.set(Some(words)),
                        view::Install::Nothing => {}
                    }
                })
                .child(
                    label().text(crate::i18n::tr("Choose a plugin archive, or drop a folder or an archive here."))
                        .color(palette.muted)
                        .font_size(13.),
                )
                .child(label().text(i18n::tr("Manual installation does not imply official approval. Review the extension and its source; you are responsible for code installed outside the Store."))
                    .color(palette.muted).font_size(13.))
                .children(refused.read().iter().map(|words| {
                    label().text(i18n::tr(words)).color(palette.muted).font_size(13.)
                })),
        );
        #[cfg(not(target_os = "android"))]
        for bundle in bundles.read().iter().filter(|bundle| {
            !self
                .view
                .plugins
                .iter()
                .any(|installed| installed.id.as_str() == bundle.id)
        }) {
            let (link, source) = (self.link.clone(), bundle.source.clone());
            list = list.child(
                Button::new()
                    .child(i18n::format(
                        i18n::language(),
                        "Install bundled {name}",
                        &[("name", &bundle.id)],
                    ))
                    .on_press(move |_| {
                        if let Some(source) = peersus_core::model::SourceDir::new(source.clone()) {
                            link.send(Command::StageInstall { source });
                        }
                    }),
            );
        }
        let mut cards: Vec<Element> = Vec::new();
        for entry in &rows {
            // A package of this name read from the store or a file and waiting for the person stands in this row's place,
            // not beside it: one name, one card.
            if let Some(staged) = self
                .view
                .staged
                .iter()
                .find(|staged| staged.id == *entry.id)
            {
                cards.push(self.staged_card(staged));
                continue;
            }
            let expanded = opened.read().as_ref() == Some(entry.id);
            // The catalog owns the stable header, whether this package is installed or not.
            let package = entry.installed.and_then(|info| info.package.as_ref());
            let (author, version, description) = if let Some(listing) = entry.listing {
                (
                    listing.author.as_str(),
                    listing.version.as_str(),
                    listing.description.as_str(),
                )
            } else if let Some(package) = package {
                (
                    package.author.as_str(),
                    package.version.as_str(),
                    package.description.as_str(),
                )
            } else {
                ("", "", "")
            };
            let mut heading = rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .spacing(10.)
                .child(
                    rect().width(Size::flex(1.)).child(
                        label()
                            .text(entry.id.as_str().to_owned())
                            .color(palette.text)
                            .font_size(18.)
                            .font_weight(FontWeight::BOLD),
                    ),
                );
            if entry.listing.is_some() {
                heading =
                    heading.child(label().text("🛒 Store").color(palette.good).font_size(12.));
            }
            let mut card = rect()
                .key(entry.id.as_str().to_owned())
                .width(Size::fill())
                .padding(18.)
                .spacing(12.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(
                    Border::new()
                        .fill(palette.border)
                        .width(1.)
                        .alignment(BorderAlignment::Inner),
                )
                .child(heading)
                .maybe(!author.is_empty() || !version.is_empty(), |card| {
                    let language = i18n::language();
                    let credit = i18n::format(
                        language,
                        "by {author} · version {version}",
                        &[("author", author), ("version", version)],
                    );
                    // What the store's archive for this device weighs, before Install or Update is pressed.
                    let credit = match entry.listing.map(|listing| listing.bytes) {
                        Some(bytes) if bytes > 0 => i18n::format(
                            language,
                            "{first} · {second}",
                            &[("first", &credit), ("second", &view::size_in_words(bytes))],
                        ),
                        _ => credit,
                    };
                    card.child(
                        rect()
                            .height(Size::px(22.))
                            .child(mono(credit, palette.muted, 11.)),
                    )
                })
                .maybe(!description.is_empty(), |card| {
                    card.child(
                        rect().height(Size::px(54.)).overflow(Overflow::Clip).child(
                            label()
                                .text(description.to_owned())
                                .color(palette.muted)
                                .font_size(13.),
                        ),
                    )
                });
            let Some(info) = entry.installed else {
                let (link, plugin) = (self.link.clone(), entry.id.clone());
                card = card.child(
                    rect()
                        .height(Size::px(92.))
                        .spacing(8.)
                        .child(
                            label()
                                .text(i18n::tr("Not installed"))
                                .color(palette.muted)
                                .font_size(12.),
                        )
                        .child(
                            rect()
                                .horizontal()
                                .spacing(8.)
                                .child({
                                    let plugin = entry.id.clone();
                                    Button::new()
                                        .on_press(move |_| {
                                            let close = opened.read().as_ref() == Some(&plugin);
                                            opened.set(if close {
                                                None
                                            } else {
                                                Some(plugin.clone())
                                            });
                                        })
                                        .child(i18n::tr(if expanded {
                                            "Hide details"
                                        } else {
                                            "Details"
                                        }))
                                })
                                .child(
                                    Button::new()
                                        .filled()
                                        .enabled(acting)
                                        .on_press(move |_| {
                                            wanted.set(Some(plugin.clone()));
                                            link.send(Command::StoreInstall {
                                                plugin: plugin.clone(),
                                            });
                                        })
                                        .child(i18n::tr("Install")),
                                ),
                        ),
                );
                if expanded && let Some(listing) = entry.listing {
                    card = card
                        .child(
                            label()
                                .text(listing.description.clone())
                                .color(palette.muted)
                                .font_size(13.),
                        )
                        .child(mono(listing.link.clone(), palette.muted, 11.));
                }
                cards.push(settings_navigation::anchor(
                    settings_search::Target::Plugin(entry.id.clone()),
                    card,
                    palette,
                ));
                continue;
            };
            let PluginInfo { id, manifest, .. } = info;
            let (word, tone) = view::plugin_word(info);
            let actions = view::PluginActions::for_plugin(info);
            let confirming = *asking.read() == view::RemoveAsk::Asking(id.clone());
            // The switch stands beside the word that says how the extension is; Details and Remove are a row of two, which
            // fits the narrowest phone in every language. Whatever else a card has (approving it, its automatic updates,
            // an update that is waiting) is under them, each on a line of its own: a row of more squeezes its last
            // control before it moves it.
            let switch = matches!(
                actions.main,
                view::PluginMain::Enable | view::PluginMain::Disable
            )
            .then(|| {
                let (link, plugin) = (self.link.clone(), id.clone());
                Switch::new(WritableSetter::new(
                    // Enabled permission is independent of a stopped or restarting process.
                    matches!(info.standing, peersus_core::model::Standing::Ready),
                    move |enabled: bool| {
                        link.send(if enabled {
                            Command::EnablePlugin {
                                plugin: plugin.clone(),
                            }
                        } else {
                            Command::DisablePlugin {
                                plugin: plugin.clone(),
                            }
                        });
                    },
                ))
            });
            let mut buttons = rect()
                .maybe(!confirming, |row| row.horizontal())
                .spacing(8.)
                .cross_align(Alignment::center());
            let plugin = id.clone();
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        let close = opened.read().as_ref() == Some(&plugin);
                        opened.set(if close { None } else { Some(plugin.clone()) });
                    })
                    .child(i18n::tr(if expanded { "Hide details" } else { "Details" })),
            );
            let approve = match actions.main.clone() {
                // Given back exactly as the owner reported it: what is approved is what was displayed.
                view::PluginMain::Approve { program } => {
                    let (link, plugin) = (self.link.clone(), id.clone());
                    let command = Command::ApprovePlugin { plugin, program };
                    Some(
                        Button::new()
                            .on_press(move |_| link.send(command.clone()))
                            .child(i18n::tr("Approve")),
                    )
                }
                view::PluginMain::Disable | view::PluginMain::Enable | view::PluginMain::None => {
                    None
                }
            };
            let (link, plugin) = (self.link.clone(), id.clone());
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        let (next, remove) = asking.read().clone().pressed(&plugin);
                        asking.set(next);
                        if remove {
                            opened.set(None);
                            link.send(Command::RemovePackage {
                                plugin: plugin.clone(),
                            });
                        } else {
                            opened.set(Some(plugin.clone()));
                        }
                    })
                    .child(i18n::tr(if confirming {
                        "Remove for good?"
                    } else {
                        "Remove"
                    })),
            );
            if confirming {
                buttons = buttons.child(
                    Button::new()
                        .on_press(move |_| asking.set(view::RemoveAsk::None.cancelled()))
                        .child(i18n::tr("Keep")),
                );
            }
            // A switch, like every other yes or no of Settings; its words wrap at their spaces beside it. An update made by it
            // forgets what the installed version was allowed, as one pressed by hand does, so it says so under its name.
            let automatic_updates = entry.listing.is_some().then(|| {
                let (link, plugin) = (self.link.clone(), id.clone());
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(10.)
                    .cross_align(Alignment::center())
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(2.)
                            .child(
                                label()
                                    .text(i18n::tr("Automatic updates"))
                                    .color(palette.text)
                                    .font_size(13.),
                            )
                            .maybe_child(view::StoreAction::Update.note().map(|note| {
                                label()
                                    .text(i18n::tr(note))
                                    .color(palette.muted)
                                    .font_size(12.)
                            })),
                    )
                    .child(Switch::new(WritableSetter::new(
                        self.view.settings.auto_update.contains(id),
                        move |on: bool| {
                            link.send(Command::PatchSettings {
                                patch: Box::new(peersus_core::config::SettingsPatch {
                                    auto_update: Some(peersus_core::config::AutoUpdateChange {
                                        plugin: plugin.clone(),
                                        on,
                                    }),
                                    ..Default::default()
                                }),
                            });
                        },
                    )))
            });
            let store_row = entry
                .listing
                .map(|listing| view::StoreRow::of_installed(listing, info));
            let update = store_row
                .as_ref()
                .filter(|row| matches!(row.action, view::StoreAction::Update))
                .map(|_| {
                    let (link, plugin) = (self.link.clone(), id.clone());
                    Button::new()
                        .filled()
                        .enabled(acting)
                        .on_press(move |_| {
                            wanted.set(Some(plugin.clone()));
                            link.send(Command::StoreInstall {
                                plugin: plugin.clone(),
                            })
                        })
                        .child(i18n::tr(
                            if matches!(
                                info.standing,
                                peersus_core::model::Standing::Broken
                                    | peersus_core::model::Standing::Changed
                            ) {
                                "Reinstall"
                            } else {
                                "Update"
                            },
                        ))
                });
            // What Details shows, right under the buttons that opened it: the words of a removal being confirmed, what the
            // extension is, who made it, what it may ask for, and its licence and notice to read again.
            let details = expanded.then(|| {
                let mut details = rect().width(Size::fill()).spacing(8.);
                // The note of an update is already under Automatic updates, which every store extension has.
                if let Some(note) = store_row
                    .as_ref()
                    .and_then(|row| row.action.note())
                    .filter(|note| Some(*note) != view::StoreAction::Update.note())
                {
                    details = details.child(label().text(i18n::tr(note)).color(palette.muted).font_size(13.));
                }
                if confirming {
                    details = details.child(
                        label()
                            .text(crate::i18n::tr("This also forgets everything it was allowed to do, on every device. Press again to remove it."))
                            .color(palette.bad)
                            .font_size(13.),
                    );
                }
                if let Some(note) = view::plugin_note(info) {
                    details = details.child(label().text(i18n::tr(note)).color(palette.muted).font_size(13.));
                }
                if let Some(package) = &info.package {
                    details = package_lines(details, package, palette);
                }
                for (name, description) in &manifest.capabilities {
                    details = details.child(
                        rect()
                            .width(Size::fill())
                            .spacing(6.)
                            .child(mono(name.clone(), palette.send, 11.))
                            .child(label().text(description.clone()).color(palette.muted).font_size(13.)),
                    );
                }
                // The agreement accepted at installation, to read again: the same overlay, with nothing to accept.
                if manifest.agreements.is_some() {
                    let plugin = id.clone();
                    details = details.child(
                        Button::new()
                            .on_press(move |_| {
                                if let Some(agreement_overlay::Ask(mut asked)) = ask {
                                    asked.set(Some(plugin.clone()));
                                }
                            })
                            .child(i18n::tr("Read the licence and notice")),
                    );
                }
                details
            });
            card = card.child(
                rect()
                    .width(Size::fill())
                    .spacing(8.)
                    .child(
                        rect()
                            .width(Size::fill())
                            .horizontal()
                            .content(Content::flex())
                            .spacing(10.)
                            .cross_align(Alignment::center())
                            .child(rect().width(Size::flex(1.)).child(mono(
                                i18n::tr(word),
                                palette.of_tone(tone),
                                11.,
                            )))
                            .maybe_child(switch),
                    )
                    .child(buttons)
                    .maybe_child(details)
                    .maybe_child(approve)
                    .maybe_child(automatic_updates)
                    .maybe_child(update),
            );
            if actions.can_restart {
                let (link, plugin) = (self.link.clone(), id.clone());
                card = card.child(
                    Button::new()
                        .on_press(move |_| {
                            link.send(Command::RestartPlugin {
                                plugin: plugin.clone(),
                            });
                        })
                        .child(i18n::tr("Restart")),
                );
            }
            card = card.child(plugin_controls::Controls {
                link: self.link.clone(),
                view: self.view.clone(),
                plugin: info.clone(),
                pictures: self.pictures.clone(),
                palette,
            });
            cards.push(settings_navigation::anchor(
                settings_search::Target::Plugin(id.clone()),
                card,
                palette,
            ));
        }
        // Packages the owner has read and is holding for confirmation whose name has no row here (a new one chosen from a file,
        // or one the search leaves out): shown first, BEFORE anything is installed.
        for staged in view::staged_apart(&self.view.staged, &rows) {
            list = list.child(self.staged_card(staged));
        }
        if self.compact {
            for card in cards {
                list = list.child(card);
            }
        } else {
            // Two columns that each stack their own cards, so a tall card never leaves a hole beside a short one: cards
            // alternate between them in order, and each column is only as tall as what is in it.
            let column = || rect().width(Size::flex(1.)).spacing(14.);
            let (mut left, mut right) = (column(), column());
            for (index, card) in cards.into_iter().enumerate() {
                if index % 2 == 0 {
                    left = left.child(card);
                } else {
                    right = right.child(card);
                }
            }
            list = list.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(14.)
                    .child(left)
                    .child(right),
            );
        }
        if rows.is_empty() && typed.trim().is_empty() && self.view.store.listings.is_empty() {
            list = list.child(
                label()
                    .text(crate::i18n::tr("No plugins installed."))
                    .color(palette.muted)
                    .font_size(16.),
            );
        }
        list
    }
}

impl PluginManager {
    /// A package the owner has read and is holding for confirmation: what it is and what it asks for, shown BEFORE anything is
    /// installed, and installed only if the person says so.
    fn staged_card(&self, staged: &PluginInfo) -> Element {
        let palette = self.palette;
        let name = String::from(staged.id.clone());
        let how = view::Staging::of(staged, &self.view.plugins);
        let (ask, plugin) = (
            try_consume_context::<agreement_overlay::Ask>(),
            staged.id.clone(),
        );
        let (cancel_link, cancelled) = (self.link.clone(), staged.id.clone());
        let mut card = rect()
            .key(format!("staged-{name}"))
            .width(Size::fill())
            .padding(18.)
            .spacing(8.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.send).width(1.5).alignment(BorderAlignment::Inner))
            .child(
                rect()
                    .width(Size::fill())
                    .spacing(8.)
                    .child(label().text(name.clone()).color(palette.text).font_size(18.).font_weight(FontWeight::BOLD))
                    .child(mono(crate::i18n::tr("WAITING FOR YOU"), palette.send, 11.))
                    .child(
                        rect()
                            .spacing(8.)
                            .child(
                                Button::new()
                                    .filled()
                                    .on_press(move |_| {
                                        if let Some(agreement_overlay::Ask(mut asked)) = ask {
                                            asked.set(Some(plugin.clone()));
                                        }
                                    })
                                    .child(i18n::tr(how.button())),
                            )
                            .child(
                                Button::new()
                                    .on_press(move |_| cancel_link.send(Command::CancelInstall { plugin: cancelled.clone() })).child(crate::i18n::tr("Not now")),
                            ),
                    ),
            )
            .child(
                label()
                    .text(i18n::tr(if how == view::Staging::Replaces {
                        "A plugin with this name is installed. This replaces it and starts with nothing allowed."
                    } else {
                        "It starts with nothing allowed. You decide what it may do after it is installed."
                    }))
                    .color(palette.muted)
                    .font_size(13.),
            );
        // Who made it and where to read about it, shown BEFORE the person decides.
        if let Some(package) = &staged.package {
            card = package_lines(card, package, palette);
        }
        for (capability, description) in &staged.manifest.capabilities {
            card = card.child(
                rect()
                    .width(Size::fill())
                    .spacing(6.)
                    .child(mono(capability.clone(), palette.send, 11.))
                    .child(
                        label()
                            .text(description.clone())
                            .color(palette.muted)
                            .font_size(13.),
                    ),
            );
        }
        settings_navigation::anchor(
            settings_search::Target::Plugin(staged.id.clone()),
            card,
            palette,
        )
    }
}

pub(crate) fn plugin_cards(
    link: &Link,
    view: &View,
    pictures: &shelf::Held,
    palette: Palette,
    compact: bool,
) -> Element {
    PluginManager {
        link: link.clone(),
        view: view.clone(),
        pictures: pictures.clone(),
        palette,
        compact,
    }
    .into()
}
