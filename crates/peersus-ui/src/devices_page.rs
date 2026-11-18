//! The Devices page: paired devices, the ones nearby, and testing a connection.
use super::*;

#[derive(PartialEq)]
pub(crate) struct DevicesPage {
    pub(crate) link: Link,
    pub(crate) view: View,
    /// The pictures this window holds, by digest: what a device's avatar is drawn from.
    pub(crate) pictures: shelf::Held,
    pub(crate) palette: Palette,
    pub(crate) form: view::Form,
    /// The width the window has, in pixels, from which the size of the pairing portal is decided.
    pub(crate) room: f32,
    /// The platform's camera, for reading the code of the device that is pairing; absent where there is none.
    pub(crate) camera: Option<link::CameraControl>,
    /// The platform's own file picker where there is one (Android), for the quick Send on a device's card.
    pub(crate) file_picker: Option<link::FilePicker>,
}

impl Component for DevicesPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut list = rect().width(Size::fill()).spacing(10.);
        for device in &self.view.devices {
            let link = self.link.clone();
            let id = device.id.clone();
            let reachable = device.presence.reachable();
            let (word, word_colour) = palette.of_presence(&device.presence);
            let stack = self.form.device_card_buttons_stack();
            let send = SendToDevice {
                link: link.clone(),
                peer: id.clone(),
                online: reachable,
                file_picker: self.file_picker.clone(),
            };
            let forget = Button::new()
                .on_press(move |_| {
                    link.send(Command::ForgetPeer {
                        peer: PeerId(id.clone()),
                    })
                })
                .child(crate::i18n::tr("Forget"));
            let mut head = rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .spacing(14.)
                .child(avatar_view::Avatar {
                    picture: device
                        .picture
                        .and_then(|digest| self.pictures.get(&digest))
                        .map(avatar_view::Shared)
                        .map(|shared| shared.0.clone()),
                    name: device.name.clone(),
                    size: 40.,
                    palette,
                })
                .child(
                    rect()
                        .width(Size::flex(1.))
                        .child(
                            label()
                                .text(device.name.clone())
                                .color(palette.text)
                                .font_size(17.)
                                .font_weight(FontWeight::BOLD),
                        )
                        .child(mono(i18n::tr(word), word_colour, 11.))
                        // What its extensions say of it, each a row under the presence and never instead of it: the presence is
                        // the application's own word and decides what can be sent; a status is an extension's words, with whose.
                        .children(device.statuses.iter().map(|status| {
                            label()
                                .text(format!(
                                    "{} · {}",
                                    status.label.as_str(),
                                    status.key.plugin.as_str()
                                ))
                                .color(palette.muted)
                                .font_size(13.)
                                .max_lines(1)
                                .text_overflow(TextOverflow::Ellipsis)
                        })),
                );
            let mut card = rect()
                .key(device.id.clone())
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
                );
            if stack {
                // On a phone the buttons go under the name, one below the other, so the name keeps the whole row and is read across.
                card = card.child(head).child(
                    rect()
                        .width(Size::fill())
                        .spacing(8.)
                        .child(send)
                        .child(forget),
                );
            } else {
                // On the desktop they stay in the row, beside the name, as they always were.
                head = head.child(send).child(forget);
                card = card.child(head);
            }
            // The test of the connection: a button, and what it found under it across the whole card, so that the words of an answer are read
            // as a paragraph and never squeezed into the row of buttons. Told from the snapshot; the advice follows the relay setting.
            card = card.child(TestConnection {
                link: self.link.clone(),
                peer: device.id.clone(),
                found: device.link_test,
                saved: device.typed_address.clone(),
                relays_on: self.view.settings.network.allow_relay,
                palette,
            });
            // What was sent to and received from THIS device, told apart from every other by its identity and not by the name it
            // gave itself.
            let (history, more) = view::history_shown(&device.id, &self.view.transfers);
            if !history.is_empty() {
                card = card.child(mono(crate::i18n::tr("HISTORY"), palette.muted, 11.));
            }
            for item in history {
                card = card.child(
                    label()
                        .text(view::history_line_in(i18n::language(), item))
                        .color(palette.muted)
                        .font_size(13.),
                );
            }
            if more > 0 {
                card = card.child(
                    label()
                        .text(i18n::format(
                            i18n::language(),
                            "+ {count} more",
                            &[("count", &more.to_string())],
                        ))
                        .color(palette.muted)
                        .font_size(13.),
                );
            }
            list = list.child(card);
        }
        if self.view.devices.is_empty() {
            list = list.child(
                label()
                    .text(crate::i18n::tr("No paired peers yet. Pair a device above."))
                    .color(palette.muted)
                    .font_size(16.),
            );
        }
        // Devices met by a code and never paired have no saved entry, but what was sent to and received from them is a history
        // all the same: one card for each identity, with the name it gave itself as a label. Nothing here pairs or allows anything.
        let mut met = rect().width(Size::fill()).spacing(10.);
        let unpaired = view::unpaired_with_history(&self.view.devices, &self.view.transfers);
        for (peer, name) in &unpaired {
            let mut card = rect()
                .key(format!("met-{peer}"))
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
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::center())
                        .content(Content::flex())
                        .spacing(14.)
                        // A device met by a code never shows a picture (none crosses a link made for files), so this is its initial.
                        .child(avatar_view::Avatar {
                            picture: None,
                            name: name.map_or_else(
                                || i18n::tr("Another device").to_owned(),
                                str::to_owned,
                            ),
                            size: 40.,
                            palette,
                        })
                        .child(
                            rect()
                                .width(Size::flex(1.))
                                .child(
                                    label()
                                        .text(name.map_or_else(
                                            || i18n::tr("Another device").to_owned(),
                                            str::to_owned,
                                        ))
                                        .color(palette.text)
                                        .font_size(17.)
                                        .font_weight(FontWeight::BOLD),
                                )
                                .child(mono(crate::i18n::tr("NOT PAIRED"), palette.muted, 11.)),
                        ),
                )
                .child(mono(crate::i18n::tr("HISTORY"), palette.muted, 11.));
            let (shown, more) = view::history_shown(peer, &self.view.transfers);
            for item in shown {
                card = card.child(
                    label()
                        .text(view::history_line_in(i18n::language(), item))
                        .color(palette.muted)
                        .font_size(13.),
                );
            }
            if more > 0 {
                card = card.child(
                    label()
                        .text(i18n::format(
                            i18n::language(),
                            "+ {count} more",
                            &[("count", &more.to_string())],
                        ))
                        .color(palette.muted)
                        .font_size(13.),
                );
            }
            met = met.child(card);
        }
        // The devices nearby that are not paired (experimental): only while the person's switch for nearness is on.
        let nearby = self.view.settings.nearby_send.then(|| NearbySection {
            link: self.link.clone(),
            strangers: self.view.pairable(),
            trouble: self.view.nearby_trouble,
            palette,
        });
        // Where devices nearby can be found (the radio is Linux's and Android's) but the person's switch is off, Peers says so and has the
        // button, instead of showing only the code and paste: a list that is not there looks like a list of nobody.
        let nearby_off = (!self.view.settings.nearby_send
            && cfg!(any(target_os = "linux", target_os = "android")))
        .then(|| {
            let client = self.link.client().clone();
            rect()
                .width(Size::fill())
                .spacing(10.)
                .child(section("NEARBY", palette))
                .child(mono(
                    i18n::tr("Devices nearby are off. Turn them on to see devices close to you that can be paired."),
                    palette.muted,
                    12.,
                ))
                .child(Solid {
                    text: "Turn on devices nearby",
                    accent: palette.receive,
                    palette,
                    enabled: true,
                    on_press: EventHandler::new(move |()| {
                        let client = client.clone();
                        spawn(async move {
                            let _ = client
                                .command(Command::PatchSettings {
                                    patch: Box::new(SettingsPatch {
                                        nearby_send: Some(true),
                                        ..Default::default()
                                    }),
                                })
                                .await;
                        });
                    }),
                })
        });
        let beside = self.form.nearby_list_is_beside() && nearby.is_some();
        let mut main = rect()
            .width(if beside { Size::flex(1.) } else { Size::fill() })
            .spacing(18.)
            .child(PairingSection {
                link: self.link.clone(),
                palette,
                // The column at the right takes its width and the gap from what the portal with the code has.
                room: if beside {
                    self.room - NEARBY_COLUMN - NEARBY_GAP
                } else {
                    self.room
                },
                form: self.form,
                camera: self.camera.clone(),
            });
        if !beside {
            main = main.maybe_child(nearby.clone());
        }
        main = main.maybe_child(nearby_off);
        main = main.child(section("PAIRED PEERS", palette)).child(list);
        if !unpaired.is_empty() {
            main = main.child(section("MET BY CODE", palette)).child(met);
        }
        let title = label()
            .text(crate::i18n::tr("Peers"))
            .color(palette.text)
            .font_size(44.)
            .font_weight(FontWeight::BOLD);
        let body = rect().width(Size::fill()).spacing(18.).child(title);
        let body = if beside {
            body.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(NEARBY_GAP)
                    .child(main)
                    .child(rect().width(Size::px(NEARBY_COLUMN)).maybe_child(nearby)),
            )
        } else {
            body.child(main)
        };
        frame(palette, self.form, body)
    }
}

/// The width of the column of devices nearby where it stands beside the rest of Peers, and the gap before it, in pixels.
pub(crate) const NEARBY_COLUMN: f32 = 300.;
pub(crate) const NEARBY_GAP: f32 = 28.;
/// How often Peers tells the owner that it shows the devices nearby, in seconds: well inside the while the owner goes on
/// looking for them after the last word.
pub(crate) const NEARBY_SOUGHT_EVERY: u64 = 4;

pub(crate) fn nearby_beat() -> u64 {
    unix_now() / NEARBY_SOUGHT_EVERY
}

/// What the platform said of why nothing is found by nearness, as words for `i18n::tr`.
pub(crate) fn nearby_trouble_words(trouble: peersus_core::nearby::Trouble) -> &'static str {
    use peersus_core::nearby::Trouble;
    match trouble {
        Trouble::NoRadio => {
            "This device has no Bluetooth that can be used to find devices brought near."
        }
        Trouble::Off => "Bluetooth is off. Turn it on to find devices brought near.",
        Trouble::NotAllowed => {
            "The system does not let this program use Bluetooth to find devices brought near."
        }
        Trouble::Failed => "Bluetooth refused. It is asked again by itself.",
        Trouble::ListenOnly => {
            "This system lets this device find devices brought near but not be found by them."
        }
    }
}

/// What a window says to the owner again and again for as long as it is so (see [`NearbyBeat`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NearbySays {
    /// It is open with the switch for nearness on: devices nearby are listened for. And what the page it shows lets this device be
    /// found for by them, if anything: only that is announced.
    Seek(Option<peersus_core::nearby::Takes>),
    /// A person is setting how near "near" is: the owner tells the strength of the device heard strongest.
    Measure,
}

/// Says one thing to the owner every few seconds for as long as it is in the tree, and draws nothing. The owner forgets what it was told a
/// little after the last time, so taking this out of the tree is all it takes to end it.
#[derive(PartialEq, Clone)]
pub(crate) struct NearbyBeat {
    pub(crate) link: Link,
    pub(crate) says: NearbySays,
}

impl NearbySays {
    pub(crate) fn command(self) -> Command {
        match self {
            Self::Seek(takes) => Command::SeekNearby { takes },
            Self::Measure => Command::MeasureNearby {},
        }
    }
}

impl Component for NearbyBeat {
    fn render(&self) -> impl IntoElement {
        // What is said now, where the task that says it again and again reads it: the page changes under a window that stays open.
        let mut says: State<NearbySays> = use_state(|| self.says);
        says.set_if_modified(self.says);
        use_hook({
            let client = self.link.client().clone();
            move || {
                let mut beats = link::clock(nearby_beat, std::time::Duration::from_millis(500));
                spawn(async move {
                    loop {
                        let said = says.peek().command();
                        let _ = client.command(said).await;
                        if beats.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        // Said at once when it changes, and not at the next beat: a person who opens Receive is taking files from that moment, and one
        // who leaves it is not.
        use_side_effect_with_deps(&self.says, {
            let client = self.link.client().clone();
            move |says: &NearbySays| {
                let (client, said) = (client.clone(), says.command());
                spawn(async move {
                    let _ = client.command(said).await;
                });
            }
        });
        rect().width(Size::px(0.)).height(Size::px(0.))
    }
}

/// How near "near" is (experimental; see `docs/nearby-send.md`): the strength a device must be heard at to count as brought near, which the
/// person sets by measuring. Nothing here guesses a distance: the person holds the two devices as near as they mean, looks at the strength
/// that is heard, and says "from here". Measuring is asked of the owner only while the person has it on.
#[derive(PartialEq)]
pub(crate) struct NearRow {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
    /// The threshold the person set, if they set one.
    pub(crate) saved: Option<i16>,
    /// The strength of the device heard strongest, while the owner measures.
    pub(crate) strongest: Option<i16>,
}

impl Component for NearRow {
    fn render(&self) -> impl IntoElement {
        use peersus_core::nearby;
        let palette = self.palette;
        let mut measuring: State<bool> = use_state(|| false);
        let on = *measuring.read();
        let language = i18n::language();
        let words = |text: String| label().text(text).color(palette.muted).font_size(13.);
        let begins = nearby::Rule::brought_near(self.saved).near_at.to_string();
        let begins = i18n::format(
            language,
            if self.saved.is_some() {
                "Near begins at {dbm} dBm, as you set it."
            } else {
                "Near begins at {dbm} dBm, as the application has it."
            },
            &[("dbm", &begins)],
        );
        let heard = on.then(|| match self.strongest {
            Some(strongest) => i18n::format(
                language,
                "Strongest device heard now: {dbm} dBm. Set from here, near would begin at {near} dBm.",
                &[
                    ("dbm", &strongest.to_string()),
                    ("near", &nearby::near_at_from(strongest).to_string()),
                ],
            ),
            None => i18n::tr("No device with this application open is heard yet.").to_owned(),
        });
        let patch = |link: &Link, near_at: i16| {
            link.send(Command::PatchSettings {
                patch: Box::new(SettingsPatch {
                    nearby_near_at: Some(near_at),
                    ..Default::default()
                }),
            });
        };
        let mut buttons = rect()
            .width(Size::fill())
            .horizontal()
            .content(Content::wrap_spacing(10.))
            .spacing(10.)
            .child(
                Button::new()
                    .on_press(move |_| measuring.set(!on))
                    .child(text::caption(i18n::tr(if on {
                        "Stop measuring"
                    } else {
                        "Measure"
                    }))),
            );
        if let Some(strongest) = self.strongest.filter(|_| on) {
            let link = self.link.clone();
            buttons = buttons.child(
                Button::new()
                    .filled()
                    .on_press(move |_| {
                        patch(&link, nearby::near_at_from(strongest));
                        measuring.set(false);
                    })
                    .child(text::caption(i18n::tr("Set near from here"))),
            );
        }
        if self.saved.is_some() {
            let link = self.link.clone();
            // Nought is how the owner is told to forget what was set.
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| patch(&link, 0))
                    .child(text::caption(i18n::tr("Reset"))),
            );
        }
        rect()
            .width(Size::fill())
            .padding(16.)
            .spacing(10.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, false))
            .child(label().text(i18n::tr("How near is near")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
            .child(words(i18n::tr("Hold the two devices as near as you will when you send, with the application open on both, then measure and set. A device counts as brought near a little under the strength measured, so that a hand that is not quite still does not lose it.").to_owned()))
            .child(words(begins))
            .maybe_child(heard.map(words))
            .child(buttons)
            .maybe_child(on.then(|| NearbyBeat { link: self.link.clone(), says: NearbySays::Measure }))
    }
}

/// The devices nearby that offer to pair and are not paired (experimental; see `docs/nearby-send.md`). Choosing one begins the pairing a code
/// begins, and what follows is the same: both people choose a circle. A name here is what that device says of itself, which nobody has
/// checked, and the page says so. They are listened for while a window is open with the switch on (see `NearbyBeat`).
#[derive(PartialEq, Clone)]
pub(crate) struct NearbySection {
    pub(crate) link: Link,
    pub(crate) strangers: Vec<peersus_core::nearby::Stranger>,
    pub(crate) trouble: Option<peersus_core::nearby::Trouble>,
    pub(crate) palette: Palette,
}

impl Component for NearbySection {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let client = self.link.client().clone();
        // What the owner said when it would not begin: shown under the list, and gone with the next choice.
        let mut owner_said: State<Option<String>> = use_state(|| None);
        let mut list = rect().width(Size::fill()).spacing(10.);
        for stranger in &self.strangers {
            list = list.child(NearbyDevice {
                key: stranger.id.0.clone(),
                name: stranger.name.clone(),
                palette,
                on_press: EventHandler::new({
                    let (client, peer) = (client.clone(), stranger.id.clone());
                    move |()| {
                        let (client, peer) = (client.clone(), peer.clone());
                        owner_said.set(None);
                        spawn(async move {
                            if let Err(reason) = client.command(Command::PairNearby { peer }).await
                            {
                                owner_said.set(Some(i18n::tr(reason).to_owned()));
                            }
                        });
                    }
                }),
            });
        }
        let words = |text: &'static str| {
            label()
                .text(i18n::tr(text))
                .color(palette.muted)
                .font_size(14.)
        };
        rect()
            .width(Size::fill())
            .spacing(12.)
            .child(section("NEARBY", palette))
            .maybe_child(self.trouble.map(|trouble| words(nearby_trouble_words(trouble))))
            .child(list)
            .maybe_child(
                (self.strangers.is_empty() && self.trouble.is_none()).then(|| {
                    words("No device nearby yet. Open Peers on the other device, with Devices nearby turned on there too.")
                }),
            )
            .maybe_child((!self.strangers.is_empty()).then(|| {
                words("A name here is what that device says of itself. Choosing one begins pairing: both of you then choose the same number.")
            }))
            .maybe_child(
                owner_said
                    .read()
                    .clone()
                    .map(|said| label().text(said).color(palette.send).font_size(14.)),
            )
    }
}

/// One device nearby in the list: its name, and the whole row is what is pressed to begin pairing with it.
#[derive(PartialEq)]
pub(crate) struct NearbyDevice {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) palette: Palette,
    pub(crate) on_press: EventHandler<()>,
}

impl Component for NearbyDevice {
    fn render(&self) -> impl IntoElement {
        let (palette, on_press) = (self.palette, self.on_press.clone());
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::fill())
            .padding((14., 16.))
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .spacing(12.)
            .corner_radius(12.)
            .background(if hovered() {
                palette.raised
            } else {
                palette.surface
            })
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| on_press.call(()))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .spacing(2.)
                    .child(
                        label()
                            .text(self.name.clone())
                            .color(palette.text)
                            .font_size(18.)
                            .font_weight(FontWeight::BOLD)
                            .max_lines(1)
                            .text_overflow(TextOverflow::Ellipsis),
                    )
                    .child(mono(crate::i18n::tr("NOT PAIRED"), palette.muted, 11.)),
            )
            .child(
                text::caption(i18n::tr("Pair"))
                    .color(palette.send)
                    .font_size(15.)
                    .font_weight(FontWeight::BOLD),
            )
    }

    fn render_key(&self) -> DiffKey {
        DiffKey::from(&self.key)
    }
}

/// The quick Send on a paired device's card: pick files and they go straight to that device, with no page between. Offered only while
/// the device is online (a file offered to a device that is not there would wait for nothing), and what the choice left out is said
/// under it, as the Send page says it. It keeps its own state, so it is a component and not a part of the loop that draws the cards.
#[derive(PartialEq)]
pub(crate) struct SendToDevice {
    pub(crate) link: Link,
    pub(crate) peer: String,
    pub(crate) online: bool,
    pub(crate) file_picker: Option<link::FilePicker>,
}

impl Component for SendToDevice {
    fn render(&self) -> impl IntoElement {
        let said: State<Option<SendNotice>> = use_state(|| None);
        let (link, peer, picker, online) = (
            self.link.clone(),
            self.peer.clone(),
            self.file_picker.clone(),
            self.online,
        );
        let button = Button::new()
            .enabled(online)
            .on_press(move |_| {
                if online {
                    choose_and_send(&link, picker.clone(), peer.clone(), said);
                }
            })
            .child(crate::i18n::tr("Send"));
        rect()
            .spacing(4.)
            .cross_align(Alignment::end())
            .child(button)
            .maybe_child((*said.read()).map(|notice| label().text(notice.words()).font_size(11.)))
    }
}

/// The test of the connection to a paired device, on its card, and the place to say where the device can be found when it cannot be reached
/// where it was paired. One button asks the owner to try the device now. A second opens a field for an IP address or a name (with a port or
/// without one), with what it does and does not help under it, and Save and test, which keeps the address and then tries the device: the SAME
/// test, so what it found is shown in one place, under the buttons, in words and coloured by how it stands.
///
/// The answer to a test is told in the snapshot and comes later (it can take half a minute), so this shows what the snapshot holds and nothing it
/// keeps of its own, except whether the field is open, what is typed in it, and the owner's sentence if it refused the address. An old answer is
/// cleared before a new test begins, so one is never taken for the other.
///
/// The advice depends on whether the internet relays are on, read from the settings the window already has: it is what decides whether "turn them
/// on" is the thing to say. And the field says plainly what an address cannot do: it cannot reach a device that no network lets a connection in.
#[derive(PartialEq)]
pub(crate) struct TestConnection {
    pub(crate) link: Link,
    pub(crate) peer: String,
    pub(crate) found: Option<peersus_core::model::LinkTest>,
    /// What the person typed for this device before, as they would type it again.
    pub(crate) saved: Option<String>,
    pub(crate) relays_on: bool,
    pub(crate) palette: Palette,
}

impl Component for TestConnection {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let words = view::address_words_in(i18n::language());
        // The field starts with what was saved, so that it is read and mended and not typed again from nothing.
        let saved = self.saved.clone();
        let mut text = use_state(move || saved.clone().unwrap_or_default());
        let mut open = use_state(|| false);
        let mut owner_said: State<Option<String>> = use_state(|| None);
        // A test that is going on is not started again, and says so by being a button that does nothing.
        let trying = self.found == Some(peersus_core::model::LinkTest::Trying);

        let (link, peer) = (self.link.clone(), self.peer.clone());
        let test = Button::new()
            .enabled(!trying)
            .on_press(move |_| {
                let peer = PeerId(peer.clone());
                link.send(Command::ClearLinkTest { peer: peer.clone() });
                link.send(Command::TestLink { peer });
            })
            .child(crate::i18n::tr("Test connection"));
        let toggle = Button::new()
            .on_press(move |_| {
                owner_said.set(None);
                open.set(!open());
            })
            .child(words.title.clone());
        let mut block = rect()
            .width(Size::fill())
            .spacing(6.)
            .child(test)
            .child(toggle);

        if open() {
            let (link, peer) = (self.link.clone(), self.peer.clone());
            let save = move |_| {
                owner_said.set(None);
                let typed = text.read().trim().to_owned();
                // Empty is not an error: it is asking for the address to be taken away, which the owner does when it is told none.
                let address = (!typed.is_empty()).then_some(typed);
                let (link, peer) = (link.clone(), PeerId(peer.clone()));
                spawn(async move {
                    match link.set_address(peer.clone(), address).await {
                        // Taken: the same test as the button above, so the result is told where every result is.
                        Ok(()) => link.send(Command::TestLink { peer }),
                        Err(sentence) => owner_said.set(Some(sentence)),
                    }
                });
            };
            let (link, peer) = (self.link.clone(), self.peer.clone());
            let clear = move |_| {
                owner_said.set(None);
                text.set(String::new());
                let (link, peer) = (link.clone(), PeerId(peer.clone()));
                spawn(async move {
                    if let Err(sentence) = link.set_address(peer, None).await {
                        owner_said.set(Some(sentence));
                    }
                });
            };
            let mut panel = rect()
                .width(Size::fill())
                .spacing(8.)
                .padding(12.)
                .corner_radius(10.)
                .background(palette.raised)
                .border(
                    Border::new()
                        .fill(palette.border)
                        .width(1.)
                        .alignment(BorderAlignment::Inner),
                )
                .child(
                    field::field(field::Profile::TEXT, text)
                        .placeholder(words.placeholder.clone())
                        .width(Size::fill()),
                );
            // What the owner said when it would not take the address: under the field, and gone with the next try.
            if let Some(sentence) = owner_said.read().clone() {
                panel = panel.child(
                    label()
                        .text(i18n::tr(&sentence).to_owned())
                        .color(palette.bad)
                        .font_size(13.),
                );
            }
            for line in &words.explanation {
                panel = panel.child(
                    label()
                        .text(line.clone())
                        .color(palette.muted)
                        .font_size(13.),
                );
            }
            panel = panel.child(
                rect()
                    .width(Size::fill())
                    .spacing(8.)
                    .child(Button::new().on_press(save).child(words.save.clone()))
                    .maybe_child(
                        self.saved
                            .is_some()
                            .then(|| Button::new().on_press(clear).child(words.clear.clone())),
                    ),
            );
            block = block.child(panel);
        }

        if let Some(found) = self.found {
            let (title, advice, tone) =
                view::link_test_words_in(i18n::language(), found, self.relays_on);
            let (link, peer) = (self.link.clone(), self.peer.clone());
            block = block.child(
                rect()
                    .width(Size::fill())
                    .spacing(4.)
                    .child(
                        label()
                            .text(title)
                            .color(palette.of_tone(tone))
                            .font_size(14.)
                            .font_weight(FontWeight::BOLD),
                    )
                    .child(label().text(advice).color(palette.muted).font_size(13.))
                    .maybe_child((!trying).then(|| {
                        Button::new()
                            .on_press(move |_| {
                                link.send(Command::ClearLinkTest {
                                    peer: PeerId(peer.clone()),
                                })
                            })
                            .child(crate::i18n::tr("Close"))
                    })),
            );
        }
        block
    }
}
