//! The Settings page: its groups, the rows the window itself draws, and the search over them.
use super::*;

#[derive(PartialEq)]
pub(crate) struct SettingsPage {
    pub(crate) link: Link,
    pub(crate) view: View,
    pub(crate) palette: Palette,
    pub(crate) form: view::Form,
    pub(crate) tab: State<SettingsTab>,
    /// The compact sheet's close action; desktop navigation stays outside Settings.
    pub(crate) on_close: Option<EventHandler<()>>,
    /// This device's own picture, if it has one: drawn large on the Device tab.
    pub(crate) picture: Option<avatar_view::Shared>,
    /// Peer pictures already fetched by the shared shelf; no additional requests or pixel copies.
    pub(crate) pictures: shelf::Held,
    /// The platform's own file picker, when the toolkit has no dialog (Android).
    pub(crate) file_picker: Option<link::FilePicker>,
    /// The platform's notifications, when the person can block them outside the program (Android): shown as a row of the Device tab.
    pub(crate) notifications: Option<link::NotificationsControl>,
    /// The platform's choice of which camera reads a code (Android): shown as a row of the Device tab.
    pub(crate) camera_choice: Option<link::CameraChoice>,
    /// The platform's camera and the window's width, for the test of the camera in its row (Android).
    pub(crate) camera: Option<link::CameraControl>,
    pub(crate) room: f32,
    /// Bounds for floating controls, excluding the system bars and main navigation.
    pub(crate) inset_top: f32,
    pub(crate) inset_bottom: f32,
}

/// The folder incoming files are saved in on their own, said in full so that nothing about where a file goes is hidden,
/// with the two ways to change it: choose one, or have none (then every file asks). There is no default folder.
pub(crate) fn save_folder_row(
    link: &Link,
    folder: Option<&std::path::Path>,
    palette: Palette,
) -> Element {
    let shown = folder.map_or_else(
        || i18n::tr("No folder chosen. Every file asks where it goes.").to_owned(),
        |path| path.display().to_string(),
    );
    let choose_link = link.clone();
    let choose = move |_| {
        if choose_link.read_only() {
            return;
        }
        #[cfg(not(target_os = "android"))]
        {
            let link = choose_link.clone();
            spawn(async move {
                if let Some(folder) = rfd::AsyncFileDialog::new()
                    .set_title(i18n::tr("Folder for incoming files"))
                    .pick_folder()
                    .await
                {
                    link.send(Command::PatchSettings {
                        patch: Box::new(SettingsPatch {
                            save_folder: Some(peersus_core::config::SaveFolderChange::Folder(
                                folder.path().to_path_buf(),
                            )),
                            ..SettingsPatch::default()
                        }),
                    });
                }
            });
        }
        #[cfg(target_os = "android")]
        let _ = &choose_link;
    };
    let clear_link = link.clone();
    let clear = move |_| {
        clear_link.send(Command::PatchSettings {
            patch: Box::new(SettingsPatch {
                save_folder: Some(peersus_core::config::SaveFolderChange::Default),
                ..SettingsPatch::default()
            }),
        });
    };
    let mut row = rect()
        .width(Size::fill())
        .spacing(8.)
        .child(mono(shown, palette.text, 12.));
    row = row.child(
        rect()
            .horizontal()
            .spacing(10.)
            .child(
                Button::new()
                    .on_press(choose)
                    .child(crate::i18n::tr("Choose folder")),
            )
            .maybe_child(folder.map(|_| {
                Button::new()
                    .on_press(clear)
                    .child(crate::i18n::tr("Use none"))
            })),
    );
    row.into()
}

/// An on/off preference that can be set from the Settings screen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preference {
    ReducedMotion,
    Background,
    CollapseToIcons,
    AutoSave,
    SendInstantly,
    TwoFingerSettings,
    UpdateAutomatically,
    RestartAfterUpdate,
    NearbySend,
    ExtensionPagesTogether,
    PrivateMode,
}

impl Preference {
    /// The edit that sets this preference to `on`, and changes nothing else.
    pub(crate) fn patch(self, on: bool) -> SettingsPatch {
        let mut patch = SettingsPatch::default();
        match self {
            Preference::ReducedMotion => patch.reduced_motion = Some(on),
            Preference::Background => patch.background = Some(on),
            Preference::CollapseToIcons => patch.collapse_to_icons = Some(on),
            Preference::AutoSave => patch.auto_save = Some(on),
            Preference::SendInstantly => patch.send_instantly = Some(on),
            Preference::TwoFingerSettings => patch.two_finger_settings = Some(on),
            Preference::UpdateAutomatically => patch.update_automatically = Some(on),
            Preference::RestartAfterUpdate => patch.restart_after_update = Some(on),
            Preference::NearbySend => patch.nearby_send = Some(on),
            Preference::ExtensionPagesTogether => patch.extension_pages_together = Some(on),
            Preference::PrivateMode => patch.private_mode = Some(on),
        }
        patch
    }
}

/// The system's own way of starting Peersus with the person's session, where the platform has one.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn startup_row(link: &Link, palette: Palette) -> Option<Element> {
    (!link.read_only()).then(|| {
        crate::startup_row::StartupRow {
            link: link.clone(),
            palette,
        }
        .into()
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn startup_row(_link: &Link, _palette: Palette) -> Option<Element> {
    None
}

/// One on/off preference. The switch is its own state; when the person flips it the change is
/// sent to the owner, and the saved value that comes back is what is shown afterwards.
#[derive(PartialEq)]
pub(crate) struct ToggleRow {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
    pub(crate) text: &'static str,
    pub(crate) note: &'static str,
    pub(crate) on: bool,
    pub(crate) which: Preference,
}

impl Component for ToggleRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // Controlled: the switch always shows the value the owner holds, and flipping it only
        // asks the owner to change it. Nothing is remembered here, so flipping it back and
        // forth always sends each change, and a refusal leaves the switch where it was.
        let (link, which) = (self.link.clone(), self.which);
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, self.on))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(
                        label()
                            .text(i18n::tr(self.text))
                            .color(palette.text)
                            .font_size(16.)
                            .font_weight(FontWeight::BOLD),
                    )
                    .child(
                        label()
                            .text(i18n::tr(self.note))
                            .color(palette.muted)
                            .font_size(13.),
                    ),
            )
            .child(Switch::new(WritableSetter::new(
                self.on,
                move |value: bool| {
                    link.send(Command::PatchSettings {
                        patch: Box::new(which.patch(value)),
                    });
                },
            )))
    }
}

/// The state of the platform's notifications and the way to allow them. Asked of the platform each time it is drawn, so that coming back
/// from the system's settings shows what the person chose there.
#[derive(PartialEq)]
pub(crate) struct NotificationsRow {
    pub(crate) control: link::NotificationsControl,
    pub(crate) palette: Palette,
}

impl Component for NotificationsRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let allowed = (self.control.allowed)();
        let open = self.control.open.clone();
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, allowed))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(label().text(i18n::tr("Notifications")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                    .child(label().text(i18n::tr(if allowed {
                        "On: an offer of a file is shown even when the program is closed."
                    } else {
                        "Blocked: an offer of a file is not shown until you allow notifications."
                    })).color(palette.muted).font_size(13.)),
            )
            .child(Button::new().on_press(move |_| open()).child(i18n::tr(if allowed { "Settings" } else { "Allow" })))
    }
}

/// The permission to draw the panel of an offer over other applications, and the way to the screen of the system where the person gives it. Read
/// again while the row is on the screen, because the person gives it on a screen of the system and comes back to the answer already given.
#[derive(PartialEq)]
pub(crate) struct OverlayRow {
    pub(crate) control: link::OverlayControl,
    pub(crate) palette: Palette,
}

impl Component for OverlayRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let control = self.control.clone();
        let mut allowed: State<bool> = use_state(|| (control.allowed)());
        use_hook({
            let control = control.clone();
            move || {
                let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
                spawn(async move {
                    loop {
                        allowed.set_if_modified((control.allowed)());
                        if ticks.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let on = *allowed.read();
        let open = self.control.open.clone();
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, on))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(label().text(i18n::tr("Offers over other apps")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                    .child(label().text(i18n::tr(if on {
                        "On: the list of the files of an offer opens over what you are doing, and Accept does not open Peersus."
                    } else {
                        "Off: the list of the files of an offer opens in Peersus. Allow it to see the list over other apps."
                    })).color(palette.muted).font_size(13.)),
            )
            .child(Button::new().on_press(move |_| open()).child(i18n::tr(if on { "Settings" } else { "Allow" })))
    }
}

/// Where the panel of an offer is drawn over other applications: one button for each edge this screen has, the chosen one filled.
/// Controlled, as the switches are: a press asks the owner, and what the owner saved is what is marked.
#[derive(PartialEq)]
pub(crate) struct NoticePlaceRow {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
    pub(crate) saved: peersus_core::config::NoticePlace,
    /// Whether this screen has a left and a right to choose as well.
    pub(crate) sides: bool,
}

impl Component for NoticePlaceRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let shown = view::notice_place_shown(self.saved, self.sides);
        let buttons = view::notice_places(self.sides)
            .iter()
            .map(|&(place, word)| -> Element {
                let link = self.link.clone();
                let button = Button::new()
                    .on_press(move |_| {
                        link.send(Command::PatchSettings {
                            patch: Box::new(SettingsPatch {
                                notice_place: Some(place),
                                ..Default::default()
                            }),
                        });
                    })
                    .child(text::caption(i18n::tr(word)));
                if place == shown {
                    button.filled().into()
                } else {
                    button.into()
                }
            });
        rect()
            .width(Size::fill())
            .padding(16.)
            .spacing(10.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, false))
            .child(label().text(i18n::tr("Where an offer appears")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
            .child(
                label()
                    .text(i18n::tr("The edge of the screen where an offer of a file is shown over what you are doing."))
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::wrap_spacing(10.))
                    .spacing(10.)
                    .children(buttons),
            )
    }
}

/// Whether the system leaves this program out of its battery optimisation, and the way to the screen of the system where the person chooses.
/// Read again while the row is on the screen, because the person decides it there and comes back to the answer already given. It states
/// what the system says and does not ask for the exemption itself.
#[derive(PartialEq)]
pub(crate) struct BatteryRow {
    pub(crate) control: link::BatteryControl,
    pub(crate) palette: Palette,
}

impl Component for BatteryRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let control = self.control.clone();
        let mut unrestricted: State<bool> = use_state(|| (control.unrestricted)());
        use_hook({
            let control = control.clone();
            move || {
                let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
                spawn(async move {
                    loop {
                        unrestricted.set_if_modified((control.unrestricted)());
                        if ticks.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let on = *unrestricted.read();
        let open = self.control.open.clone();
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, on))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(label().text(i18n::tr("Battery")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                    .child(label().text(i18n::tr(if on {
                        "On: the network of the background service keeps working while the phone is idle."
                    } else {
                        "Limited: while the phone is idle the system may cut the network of the background service. Allow it in the battery settings."
                    })).color(palette.muted).font_size(13.)),
            )
            .child(Button::new().on_press(move |_| open()).child(i18n::tr("Settings")))
    }
}

/// A platform consent with a live status and its own dialog (a `link::ServiceSetting`). A row that can be turned on and off
/// shows a switch whose position is the platform's real state, read again while it is on screen.
pub(crate) struct ProviderRow {
    pub(crate) title: &'static str,
    pub(crate) status: std::sync::Arc<dyn Fn() -> String + Send + Sync>,
    pub(crate) open: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub(crate) switch: Option<link::PlatformSwitch>,
    pub(crate) palette: Palette,
}

impl PartialEq for ProviderRow {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
            && std::sync::Arc::ptr_eq(&self.open, &other.open)
            && self.switch == other.switch
            && self.palette == other.palette
    }
}

impl Component for ProviderRow {
    fn render(&self) -> impl IntoElement {
        let read = self.status.clone();
        let read_on = self.switch.as_ref().map(|switch| switch.on.clone());
        let mut status = use_state(|| read());
        let mut on = use_state(|| read_on.as_ref().is_some_and(|on| on()));
        use_hook(move || {
            let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
            spawn(async move {
                loop {
                    status.set_if_modified(read());
                    if let Some(read_on) = &read_on {
                        on.set_if_modified(read_on());
                    }
                    if ticks.changed().await.is_err() {
                        break;
                    }
                }
            });
        });
        let palette = self.palette;
        let open = self.open.clone();
        let enabled = *on.read();
        let header = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .spacing(12.)
            .child(
                rect()
                    .width(Size::flex(1.))
                    .spacing(2.)
                    .child(
                        label()
                            .text(i18n::tr(self.title))
                            .color(palette.text)
                            .font_size(16.)
                            .font_weight(FontWeight::BOLD),
                    )
                    .maybe_child(self.switch.is_some().then(|| {
                        label()
                            .text(i18n::tr(if enabled { "On" } else { "Off" }))
                            .color(if enabled { palette.send } else { palette.muted })
                            .font_size(13.)
                            .font_weight(FontWeight::BOLD)
                    })),
            )
            .maybe_child(self.switch.clone().map(|switch| {
                Switch::new(WritableSetter::new(enabled, move |value: bool| {
                    (switch.set)(value)
                }))
            }));
        rect()
            .width(Size::fill())
            .padding(16.)
            .spacing(8.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, enabled))
            .child(header)
            .child(
                label()
                    .text(status.read().clone())
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(
                Button::new()
                    .on_press(move |_| open())
                    .child(i18n::tr("Settings")),
            )
    }
}

/// The folder that incoming files are saved in on their own, on a phone: where the folder is not a path, so it is not a setting of the program but
/// of the platform, which keeps it and is asked. Read again while the row is on the screen, because the system's picker answers to the platform
/// and not to the program, and the person comes back to the row with the answer already given.
#[derive(PartialEq)]
pub(crate) struct SavedFolderRow {
    pub(crate) sink: link::FolderSink,
    pub(crate) palette: Palette,
}

impl Component for SavedFolderRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let sink = self.sink.clone();
        let mut tree: State<Option<String>> = use_state(|| (sink.saved)());
        use_hook({
            let sink = sink.clone();
            move || {
                let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
                spawn(async move {
                    loop {
                        tree.set_if_modified((sink.saved)());
                        if ticks.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        // The address is the system's own text and says nothing a person can read, so what is shown is whether a folder is chosen; the system's
        // picker shows its name when it is opened.
        let chosen = tree.read().is_some();
        let (choose, forget) = (sink.choose_saved.clone(), sink.forget_saved.clone());
        let mut row = rect().width(Size::fill()).spacing(8.).child(mono(
            i18n::tr(if chosen {
                "A folder is chosen. Files go straight into it."
            } else {
                "No folder chosen. Every file asks where it goes."
            }),
            palette.text,
            12.,
        ));
        row = row.child(
            rect()
                .horizontal()
                .spacing(10.)
                .child(
                    Button::new()
                        .on_press(move |_| choose())
                        .child(i18n::tr("Choose folder")),
                )
                .maybe_child(chosen.then(|| {
                    let mut tree = tree;
                    Button::new()
                        .on_press(move |_| {
                            forget();
                            tree.set(None);
                        })
                        .child(i18n::tr("Use none"))
                })),
        );
        row
    }
}

/// What a line of the list of back cameras says, said in words: a wide lens (short focal length), the main one, or a long one, with the
/// nearest it can focus on, because those two numbers are what tells which camera sees a screen held at a hand's length.
pub(crate) fn camera_words(line: &str) -> Option<(String, String, bool)> {
    let mut parts = line.split('\t');
    let id = parts.next()?.to_owned();
    let tenths: u32 = parts.next()?.parse().ok()?;
    let centimetres: u32 = parts.next()?.parse().ok()?;
    let chosen = parts.next()? == "1";
    let kind = match tenths {
        0 => "Camera",
        1..=39 => "Wide",
        40..=79 => "Main",
        _ => "Zoom",
    };
    let near = if centimetres == 0 {
        String::new()
    } else {
        format!(" · {centimetres} cm")
    };
    Some((id.clone(), format!("{} {id}{near}", i18n::tr(kind)), chosen))
}

/// The choice of the camera that reads a code: one button for each back camera of the phone, and one that leaves the choice to the program.
/// The camera that is open starts again on the one chosen, so the circle of scanning, in Peers, shows what that camera makes of a screen.
#[derive(PartialEq)]
pub(crate) struct CameraRow {
    pub(crate) link: Link,
    pub(crate) choice: link::CameraChoice,
    pub(crate) camera: Option<link::CameraControl>,
    pub(crate) room: f32,
    pub(crate) palette: Palette,
}

impl Component for CameraRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // The list is asked once for each drawing, and a press is remembered here at once, so the buttons answer before the phone does.
        let mut picked: State<Option<String>> = use_state(|| None);
        let lines = (self.choice.list)();
        let cameras: Vec<(String, String, bool)> = lines.lines().filter_map(camera_words).collect();
        let current = picked.read().clone().unwrap_or_else(|| {
            cameras
                .iter()
                .find(|camera| camera.2)
                .map(|camera| camera.0.clone())
                .unwrap_or_default()
        });
        let button = |id: String, words: String| -> Element {
            let choose = self.choice.choose.clone();
            let selected = current == id;
            let target = id.clone();
            let button = Button::new()
                .width(Size::fill())
                .on_press(move |_| {
                    picked.set(Some(target.clone()));
                    choose(&target);
                })
                .child(words);
            if selected {
                button.filled().into()
            } else {
                button.into()
            }
        };
        // A grid of two columns: every button fills a cell of the same width, and a last row of one leaves its second cell empty, so that
        // the buttons line up in columns and none runs off the edge of the card or breaks its words.
        let cell = |content: Option<Element>| -> Element {
            let mut cell = rect().width(Size::flex(1.));
            if let Some(content) = content {
                cell = cell.child(content);
            }
            cell.into()
        };
        let mut buttons: Vec<Element> =
            vec![button(String::new(), i18n::tr("Automatic").to_owned())];
        for (id, words, _) in cameras {
            buttons.push(button(id, words));
        }
        let mut row = rect().width(Size::fill()).spacing(8.);
        for pair in buttons.chunks(2) {
            row = row.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(8.)
                    .child(cell(Some(pair[0].clone())))
                    .child(cell(pair.get(1).cloned())),
            );
        }
        // The test: the camera is started here, in the row, and its picture is shown under the buttons, so that each camera is chosen by
        // looking at what it shows of a screen. It is the same scan as the circle of Peers (a code read here is joined all the same), and
        // it is closed when this page is left.
        let owner_said: State<Option<String>> = use_state(|| None);
        let CodeScan {
            looking,
            picture,
            open,
            stop,
        } = use_code_scan(&self.link, self.camera.clone(), self.room, owner_said);
        let shown = picture.read().clone();
        let scanning = looking() == view::Looking::Scanning;
        let test: Element = match shown {
            Some(picture) => CameraCircle {
                picture,
                room: self.room,
                palette,
                on_stop: stop.clone(),
            }
            .into(),
            None if scanning => label()
                .text(i18n::tr("starting the camera"))
                .color(palette.muted)
                .font_size(14.)
                .into(),
            None => {
                let open = open.clone();
                Button::new()
                    .width(Size::fill())
                    .on_press(move |_| open.call(()))
                    .child(i18n::tr("Test the camera"))
                    .into()
            }
        };
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
                    .text(i18n::tr("Camera"))
                    .color(palette.text)
                    .font_size(16.)
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                label()
                    .text(i18n::tr(
                        "Choose the camera that reads a code, and press Test to see what it shows.",
                    ))
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(row)
            .child(
                rect()
                    .width(Size::fill())
                    .cross_align(Alignment::center())
                    .child(test),
            )
    }
}

impl Component for SettingsPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let settings = &self.view.settings;
        let mut query = use_state(String::new);
        let typed = query.read().clone();
        let searching = !typed.trim().is_empty();
        let origin = use_state(|| None::<f32>);
        let requested = use_state(|| None::<settings_search::Target>);
        let selected = use_state(|| None::<settings_search::Target>);
        let landed: State<Option<f32>> = use_state(|| None);
        let go_to: State<Option<f32>> = use_state(|| None);
        let mut tap = use_state(view::Tap::default);
        let navigation = settings_navigation::Navigation {
            requested,
            selected,
            found: landed,
            tap,
        };
        provide_context(navigation);
        let name = use_state(|| settings.device_name.clone());
        let save_link = self.link.clone();
        let save_name = move |_| {
            let value = name.read().trim().to_owned();
            if !value.is_empty() {
                save_link.send(Command::PatchSettings {
                    patch: Box::new(SettingsPatch {
                        device_name: Some(value),
                        ..Default::default()
                    }),
                });
            }
        };
        // The picture is read and decoded off the window's thread, and a refusal is said in words under
        // the buttons. The owner is given the pixels, never the file or its path.
        let mut picture_refused: State<Option<&'static str>> = use_state(|| None);
        let choose_link = self.link.clone();
        let file_picker = self.file_picker.clone();
        let choose_picture = move |_| {
            let link = choose_link.clone();
            if link.read_only() {
                picture_refused.set(Some(link::READ_ONLY));
                return;
            }
            // A platform with no dialog of the toolkit's (Android) gives its own picker: one file comes back open, with
            // no path, and is decoded from where it is, under the same limits as a file on a disk. What a provider
            // hands over beyond the one asked for is let go of, or it would stay open in the registry for good.
            if let Some(picker) = file_picker.clone() {
                picker.picked.opened();
                (picker.open)(false);
                let mut said = link::await_picked(
                    picker.picked.clone(),
                    link::LOOK_EVERY,
                    link::PICKER_PATIENCE,
                );
                let picked = picker.picked.clone();
                spawn(async move {
                    while let Some(taken) = said.recv().await {
                        if let Some((used, rest)) = view::picture_choice(&taken.files) {
                            picked.release(&rest);
                            if let Some(file) = picked.take_file(used.0) {
                                match link.blocking(move || avatar_file::read_file(file)).await {
                                    Ok(Ok(picture)) => {
                                        picture_refused.set(None);
                                        let client = link.client().clone();
                                        let _ = client
                                            .command(Command::SetProfilePicture { picture })
                                            .await;
                                    }
                                    Ok(Err(refused)) => picture_refused.set(Some(refused.words())),
                                    Err(_) => {
                                        picture_refused.set(Some("That picture could not be read."))
                                    }
                                }
                            }
                        }
                        if taken.refused > 0 {
                            picture_refused.set(Some("That picture could not be kept; try again."));
                        }
                        if !taken.still_open {
                            break;
                        }
                    }
                });
                return;
            }
            spawn(async move {
                #[cfg(not(target_os = "android"))]
                if let Some(file) = rfd::AsyncFileDialog::new()
                    .set_title(i18n::tr("Choose a profile picture (PNG or JPEG)"))
                    .pick_file()
                    .await
                {
                    let path = file.path().to_path_buf();
                    match link.blocking(move || avatar_file::read(&path)).await {
                        Ok(Ok(picture)) => {
                            picture_refused.set(None);
                            let client = link.client().clone();
                            let _ = client.command(Command::SetProfilePicture { picture }).await;
                        }
                        Ok(Err(refused)) => picture_refused.set(Some(refused.words())),
                        Err(_) => picture_refused.set(Some("That picture could not be read.")),
                    }
                }
                // Android always has a picker of its own (the branch above), so with none there is nothing to open.
                #[cfg(target_os = "android")]
                {
                    let _ = &link;
                    picture_refused.set(Some("This phone has no way to choose a picture here."));
                }
            });
        };
        let remove_link = self.link.clone();
        let remove_picture = move |_| {
            picture_refused.set(None);
            remove_link.send(Command::ClearProfilePicture {});
        };

        let appearance::Preview(mut theme_preview) = consume_context::<appearance::Preview>();
        let theme_button = |text: &'static str, value: ThemeChoice| -> Element {
            let link = self.link.clone();
            let selected = settings.theme == value;
            let button = Button::new()
                .on_press(move |_| {
                    theme_preview.set(None);
                    link.send(Command::PatchSettings {
                        patch: Box::new(SettingsPatch {
                            theme: Some(value),
                            ..Default::default()
                        }),
                    })
                })
                .child(text::caption(i18n::tr(text)));
            if selected {
                button.filled().into()
            } else {
                button.into()
            }
        };

        let toggle =
            |text: &'static str, note: &'static str, on: bool, which: Preference| -> Element {
                ToggleRow {
                    link: self.link.clone(),
                    palette,
                    text,
                    note,
                    on,
                    which,
                }
                .into()
            };

        let tab = self.tab;
        let card = |children: Vec<Element>| -> Element {
            let mut card = rect()
                .width(Size::fill())
                .padding(18.)
                .spacing(8.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(
                    Border::new()
                        .fill(palette.border)
                        .width(1.)
                        .alignment(BorderAlignment::Inner),
                );
            for child in children {
                card = card.child(child);
            }
            card.into()
        };
        let fact = |name: &'static str, value: String| -> Element {
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::flex())
                // The name takes what the value leaves and wraps at its spaces. The value is one line and never more than a share of
                // the row, cut with an ellipsis beyond that: it may be a name a person typed, of any length, and the word that says
                // what it is must keep room to be read whole.
                .child(
                    label()
                        .text(i18n::tr(name))
                        .color(palette.muted)
                        .font_size(14.)
                        .width(Size::flex(1.)),
                )
                .child(
                    text::caption(value)
                        .max_width(Size::percent(50.))
                        .text_overflow(TextOverflow::Ellipsis)
                        .color(palette.text)
                        .font_size(14.)
                        .font_weight(FontWeight::BOLD),
                )
                .into()
        };

        // The platform's settings for services that an installed extension uses; none with no such extension, and none on a desktop.
        let service_settings: Vec<link::ServiceSetting> =
            try_consume_context::<Vec<link::ServiceSetting>>()
                .unwrap_or_default()
                .into_iter()
                .filter(|setting| view::wants_service(&self.view.plugins, setting.service))
                .collect();
        let searched_settings: Vec<(&'static str, &'static str)> = service_settings
            .iter()
            .map(|setting| (setting.title, setting.search))
            .collect();
        // Nothing about updates is drawn in a build that has none.
        let update = view::update_panel(&self.view.update);
        let build = |which: SettingsTab| -> Element {
            match which {
            SettingsTab::Overview => {
                let moving = self.view.transfers.iter().filter(|t| t.stoppable()).count();
                let online = self.view.devices.iter().filter(|d| d.presence == Presence::Online).count();
                let running = self.view.plugins.iter().filter(|p| p.status == PluginStatus::Running).count();
                let totals = self.view.file_totals();
                rect()
                    .width(Size::fill())
                    .spacing(14.)
                    .child(section("HOW THINGS STAND", palette))
                    .child(card(vec![
                        fact("This device", settings.device_name.clone()),
                        fact("Appearance", i18n::tr(&format!("{:?}", settings.theme)).to_owned()),
                        fact("Internet relays", i18n::tr(if settings.network.allow_relay { "on" } else { "off" }).to_owned()),
                        fact("Runs in the background", i18n::tr(if settings.background { "yes" } else { "no" }).to_owned()),
                        fact("Version", view::running_version(&self.view.update).to_owned()),
                        fact("Updates", i18n::tr(view::update_word(self.view.update.status)).to_owned()),
                    ]))
                    .child(card(vec![
                        fact("Paired devices", i18n::format(i18n::language(), "{total} ({online} online)", &[("total", &self.view.devices.len().to_string()), ("online", &online.to_string())])),
                        fact("Transfers in progress", moving.to_string()),
                        fact("Extensions", i18n::format(i18n::language(), "{total} ({running} running)", &[("total", &self.view.plugins.len().to_string()), ("running", &running.to_string())])),
                    ]))
                    // What moved, said of the history the list still holds: Clear and forgetting a device take their lines out of it, and it keeps
                    // the thousand latest, so these are the files of that record and not of every day since the first.
                    .child(card(vec![
                        fact("Files sent", totals.sent.to_string()),
                        fact("Files received", totals.received.to_string()),
                        fact("Files in all", totals.total.to_string()),
                    ]))
                    .into()
            }
            SettingsTab::Device => rect()
                .width(Size::fill())
                .spacing(14.)
                .child(section("THIS DEVICE", palette))
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .main_align(Alignment::SpaceBetween)
                        .spacing(16.)
                        // The picture is the button: pressing it opens the picker. A separate "Choose a picture"
                        // button did nothing a press on the picture does not, and took room that a phone lacks.
                        // The press is on this wrapper and not on `Avatar`, which every list of devices also draws.
                        .child(
                            rect()
                                .on_press(choose_picture.clone())
                                .child(avatar_view::Avatar {
                                    picture: self.picture.as_ref().map(|shared| shared.0.clone()),
                                    name: settings.device_name.clone(),
                                    size: 96.,
                                    palette,
                                }),
                        )
                        // The picture at the left edge and Remove at the right, with only the one gap between them when the row is
                        // narrow (a phone, a translated "Remove"): its caption is one line and never broken inside the word.
                        .child(
                            Button::new()
                                .on_press(remove_picture.clone())
                                .enabled(self.picture.is_some())
                                .child(text::caption(crate::i18n::tr("Remove"))),
                        ),
                )
                .children(picture_refused.read().iter().map(|words| {
                    label().text(i18n::tr(words)).color(palette.muted).font_size(13.)
                }))
                .child(if self.form.ask_stacks_field_and_button() {
                    rect()
                        .width(Size::fill())
                        .spacing(10.)
                        .child(field::field(field::Profile::TEXT, name).placeholder(crate::i18n::tr("Device name")).width(Size::fill()))
                        .child(Button::new().filled().on_press(save_name.clone()).width(Size::fill()).child(crate::i18n::tr("Save")))
                } else {
                    rect()
                        .width(Size::fill())
                        .max_width(Size::px(460.))
                        .content(Content::flex())
                        .horizontal()
                        .spacing(10.)
                        .child(field::field(field::Profile::TEXT, name).placeholder(crate::i18n::tr("Device name")).width(Size::flex(1.)))
                        .child(Button::new().filled().on_press(save_name.clone()).child(crate::i18n::tr("Save")))
                })
                .child(language_picker::Picker {
                    link: self.link.clone(),
                    settings: settings.clone(),
                    choices: view::language_choices(&self.view.languages, i18n::messages()),
                    current: view::wanted_language(settings.language, &self.view.languages),
                    palette,
                    inset_top: self.inset_top,
                    inset_bottom: self.inset_bottom,
                    // On a phone this page lies on the panel of the Settings overlay, whose colour is lifted; elsewhere on the page's own.
                    ground: if self.form.settings_places_come_from_the_top() { palette.raised } else { palette.background },
                })
                .child(interface_size::Picker { link: self.link.clone(), settings: settings.clone(), palette })
                .child(toggle(
                    "Run in the background",
                    "Start without opening a window.",
                    settings.background,
                    Preference::Background,
                ))
                .maybe_child(startup_row(&self.link, palette))
                .maybe_child(cfg!(target_os = "android").then(|| {
                    toggle(
                        "Two fingers open Settings",
                        "Touch the screen with two fingers to bring Settings down from the top. It does nothing while Settings is already open.",
                        settings.two_finger_settings,
                        Preference::TwoFingerSettings,
                    )
                }))
                .maybe_child(self.notifications.clone().map(|control| -> Element { NotificationsRow { control, palette }.into() }))
                .maybe_child(try_consume_context::<link::OverlayControl>().map(|control| -> Element { OverlayRow { control, palette }.into() }))
                // The panel exists where the platform can draw over other applications, and there it has a top and a bottom; on a
                // desktop that can show one it has the sides as well.
                .maybe_child(try_consume_context::<link::OverlayControl>().map(|_| -> Element {
                    NoticePlaceRow { link: self.link.clone(), palette, saved: settings.notice_place, sides: false }.into()
                }))
                .maybe_child(desktop_panel().then(|| -> Element {
                    NoticePlaceRow { link: self.link.clone(), palette, saved: settings.notice_place, sides: true }.into()
                }))
                .maybe_child(try_consume_context::<link::BatteryControl>().map(|control| -> Element { BatteryRow { control, palette }.into() }))
                .children(service_settings.iter().cloned().map(|setting| {
                    ProviderRow { title: setting.title, status: setting.status, open: setting.open, switch: Some(setting.switch), palette }
                }))
                .maybe_child(self.camera_choice.clone().map(|choice| -> Element {
                    CameraRow { link: self.link.clone(), choice, camera: self.camera.clone(), room: self.room, palette }.into()
                }))
                .child(section("PRIVACY", palette))
                .child(toggle(
                    "Private mode",
                    "Off: your paired devices see this device online, with its name, picture and statuses. On: this device stays connected and sees them, but tells them nothing about itself, so they show it as offline, and extensions send nothing on their own. Sending a file to a device, or choosing a device in an extension, shows this device to that one device until a short while after you are done.",
                    settings.private_mode,
                    Preference::PrivateMode,
                ))
                .child(section("OUTGOING FILES", palette))
                .child(toggle(
                    "Send instantly",
                    "Off: after choosing where to send, you hold the circle until it fills, so a wrong choice costs nothing. On: touching the circle sends at once.",
                    settings.send_instantly,
                    Preference::SendInstantly,
                ))
                // Experimental, and only where there is an adapter for the platform's Bluetooth: elsewhere the switch would do nothing.
                .maybe_child(cfg!(any(target_os = "linux", target_os = "android")).then(|| -> Element { section("EXPERIMENTAL", palette).into() }))
                .maybe_child(cfg!(any(target_os = "linux", target_os = "android")).then(|| {
                    toggle(
                        "Devices nearby",
                        "Off: nothing is announced and nothing is listened for. On: while this window is open, this device listens for devices nearby and can be found by them. With Receive open it says that it takes files: files listed on a paired device held right next to it are sent at once, and a device that is not paired can offer its files when it is brought near; each file is asked about here. With Peers open it says that it is there to be paired with, and Peers lists the devices nearby that say so. Uses Bluetooth.",
                        settings.nearby_send,
                        Preference::NearbySend,
                    )
                }))
                .maybe_child(
                    self.view
                        .nearby_trouble
                        .filter(|_| settings.nearby_send && cfg!(any(target_os = "linux", target_os = "android")))
                        .map(|trouble| -> Element {
                            label()
                                .text(i18n::tr(nearby_trouble_words(trouble)))
                                .color(palette.muted)
                                .font_size(14.)
                                .into()
                        }),
                )
                .maybe_child((settings.nearby_send && cfg!(any(target_os = "linux", target_os = "android"))).then(|| -> Element {
                    NearRow {
                        link: self.link.clone(),
                        palette,
                        saved: settings.nearby_near_at,
                        strongest: self.view.nearby_strongest.filter(|_| self.view.nearby_measuring),
                    }
                    .into()
                }))
                .child(section("INCOMING FILES", palette))
                .child(toggle(
                    "Save incoming files on their own",
                    "Off: you are asked where each file goes. On: files go straight to the folder below, without asking.",
                    settings.auto_save,
                    Preference::AutoSave,
                ))
                .child(match try_consume_context::<link::FolderSink>() {
                    Some(sink) => SavedFolderRow { sink, palette }.into(),
                    None => save_folder_row(&self.link, settings.save_folder.as_deref(), palette),
                })
                .maybe_child(update.as_ref().map(|_| -> Element { section("UPDATES", palette).into() }))
                .maybe_child(update.as_ref().map(|panel| update_card(&self.link, panel, palette)))
                .maybe_child(update.as_ref().filter(|panel| panel.switches).map(|_| {
                    toggle(
                        "Update automatically",
                        "On: a verified update of Peersus is downloaded and put in place without asking.",
                        settings.update_automatically,
                        Preference::UpdateAutomatically,
                    )
                }))
                .maybe_child(update.as_ref().filter(|panel| panel.switches).map(|_| {
                    toggle(
                        "Restart after an update",
                        "On: Peersus starts again by itself once an update is in place, when nothing is being sent or received.",
                        settings.restart_after_update,
                        Preference::RestartAfterUpdate,
                    )
                }))
                .into(),
            SettingsTab::Appearance => rect()
                .width(Size::fill())
                .spacing(14.)
                .child(section("THEME", palette))
                .child(
                    rect()
                        // Wrapped: four names do not fit a phone's width in every language, and the last one is the way to the editor.
                        .width(Size::fill())
                        .horizontal()
                        .content(Content::wrap_spacing(10.))
                        .spacing(10.)
                        .child(theme_button("System", ThemeChoice::System))
                        .child(theme_button("Light", ThemeChoice::Light))
                        .child(theme_button("Dark", ThemeChoice::Dark))
                        .child(theme_button("Custom", ThemeChoice::Custom)),
                )
                .child(accent_picker::Picker { link: self.link.clone(), settings: settings.clone(), palette })
                .child(renderer_settings::Controls {
                    link: self.link.clone(),
                    saved: settings.renderer,
                    palette,
                })
                .child(appearance::Editor { link: self.link.clone(), settings: settings.clone(), palette })
                // NOTE(@DarkPhilosophy): Keep-icons is disabled on Android; navigation uses the top Settings sheet.
                .maybe(!cfg!(target_os = "android"), |el| el.child(toggle(
                    "Keep icons when the bar is closed",
                    "A closed bar shows only its pictures. Off, a closed bar is not shown at all.",
                    settings.collapse_to_icons,
                    Preference::CollapseToIcons,
                )))
                .child(toggle(
                    "Extension pages in one place",
                    "The pages extensions add share one place in the navigation, Extensions, which switches between them. Off, each has a place of its own.",
                    settings.extension_pages_together,
                    Preference::ExtensionPagesTogether,
                ))
                .child(toggle(
                    "Reduce motion",
                    "Turn off decorative animation.",
                    settings.reduced_motion,
                    Preference::ReducedMotion,
                ))
                .into(),
            SettingsTab::Network => rect()
                .width(Size::fill())
                .spacing(14.)
                .child(section("CONNECTIONS", palette))
                .child(relay_choice::Picker { link: self.link.clone(), settings: settings.clone(), diagnostics: self.view.relay, probes: self.view.relay_probes.clone(), palette, compact: self.form == view::Form::Compact })
                // The port this device listens on, which another device writes after this one's address to find it again. Read only: the system
                // chooses it when the program starts, so it is different each time, and it is shown here and not kept.
                .child(card(vec![
                    fact(
                        "Listening port",
                        self.view.listening_port.map_or_else(|| "—".to_owned(), |port| port.to_string()),
                    ),
                    label()
                        .text(i18n::tr("Another device writes this port after the address of this one to find it. It changes each time the program starts."))
                        .color(palette.muted)
                        .font_size(13.)
                        .into(),
                ]))
                .into(),
            SettingsTab::Extensions => rect()
                .width(Size::fill())
                .spacing(14.)
                // The headings are drawn inside the cards component, because the store and the installed list
                // share one removal confirmation and so one component.
                .child(plugin_cards(&self.link, &self.view, &self.pictures, palette, self.form == view::Form::Compact))
                .into(),
            SettingsTab::Permissions => {
                let mut page = rect().width(Size::fill()).spacing(14.).child(section("WHAT EACH PLUGIN MAY DO", palette));
                for plugin in &self.view.plugins {
                    let name = String::from(plugin.id.clone());
                    let mut body: Vec<Element> =
                        vec![label().text(name).color(palette.text).font_size(18.).font_weight(FontWeight::BOLD).into()];
                    let nothing = plugin.grantable.per_device.is_empty() && plugin.grantable.outright.is_empty();
                    if nothing {
                        body.push(label().text(crate::i18n::tr("It cannot be granted anything.")).color(palette.muted).font_size(14.).into());
                    }
                    // What can be allowed once, for no device in particular.
                    if !plugin.grantable.outright.is_empty() {
                        let held = self.view.grants.get(&plugin.id).cloned().unwrap_or_default();
                        body.push(mono(crate::i18n::tr("NOT TIED TO ANY DEVICE"), palette.muted, 11.).into());
                        body.push(permission_block(&self.link, plugin, &plugin.grantable.outright, &held, None, palette));
                    }
                    // What is allowed for one paired device at a time: one block for each device.
                    if !plugin.grantable.per_device.is_empty() {
                        if self.view.devices.is_empty() {
                            body.push(label().text(crate::i18n::tr("Pair a device to allow this plugin anything for it.")).color(palette.muted).font_size(14.).into());
                        }
                        for device in &self.view.devices {
                            let held = device.grants.get(&plugin.id).cloned().unwrap_or_default();
                            body.push(mono(i18n::format(i18n::language(), "FOR {device}", &[("device", &device.name.to_uppercase())]), palette.muted, 11.).into());
                            body.push(permission_block(
                                &self.link,
                                plugin,
                                &plugin.grantable.per_device,
                                &held,
                                Some(peersus_core::model::PeerId(device.id.clone())),
                                palette,
                            ));
                        }
                    }
                    page = page.child(settings_navigation::anchor(settings_search::Target::PluginPermissions(plugin.id.clone()), card(body), palette));
                }
                if self.view.plugins.is_empty() {
                    page = page.child(label().text(crate::i18n::tr("No plugins installed, so nothing to allow.")).color(palette.muted).font_size(16.));
                }
                page.into()
            }
            SettingsTab::Binds => binds_page::BindsPage {
                link: self.link.clone(),
                palette,
                rows: view::bind_rows(&self.view),
                bindings: settings.bindings.clone(),
                outside: settings.bindings_outside,
            }
            .into(),
            SettingsTab::Languages => languages_page::LanguagesPage {
                link: self.link.clone(),
                palette,
                state: self.view.languages.clone(),
                chosen: settings.language,
            }
            .into(),
            SettingsTab::About => {
                let mut page = rect().width(Size::fill()).spacing(14.).child(card(vec![
                    label().text("peersus").color(palette.text).font_size(20.).font_weight(FontWeight::BOLD).into(),
                    label().text(i18n::tr("Move it. Don't store it.")).color(palette.text).font_size(15.).into(),
                    label()
                        .text(i18n::tr("Private connections between your own devices. Files go directly from one device to another, only after the receiving person approves, and no cloud folder is involved."))
                        .color(palette.muted)
                        .font_size(13.)
                        .into(),
                    mono(
                        format!("VERSION {} \u{b7} {}", env!("CARGO_PKG_VERSION"), about::APPLICATION_LICENCE),
                        palette.muted,
                        12.,
                    )
                    .into(),
                    // The notice the licence asks every redistributor to keep belongs to the licence, word for word as the file has it. Its
                    // lines were broken at the file's fixed width; shown as paragraphs, the page wraps them to its own width.
                    label().text(about::paragraphs(about::PROJECT_NOTICE)).color(palette.muted).font_size(12.).into(),
                ]));
                let mut disclaimer = rect()
                    .width(Size::fill())
                    .padding(18.)
                    .spacing(12.)
                    .corner_radius(14.)
                    .background(palette.warning_background)
                    .child(label().text(crate::i18n::tr("IMPORTANT — DATA AND SECURITY RISKS"))
                        .color(palette.warning).font_size(15.).font_weight(FontWeight::BOLD));
                for paragraph in about::DISCLAIMER {
                    disclaimer = disclaimer.child(label().text(i18n::tr(paragraph)).color(palette.warning).font_size(14.));
                }
                page = page.child(disclaimer);
                page = page.child(support::Support { palette, reduced: settings.reduced_motion });
                for section_of_credits in about::SECTIONS {
                    page = page.child(section(section_of_credits.title, palette));
                    let mut rows = Vec::new();
                    for credit in section_of_credits.credits {
                        rows.push(
                            rect()
                                .width(Size::fill())
                                .spacing(2.)
                                // Its name and the version that is built, on one line that scrolls inside the row when it is too
                                // long for it (never broken letter by letter), and its licence under it on a line of its own.
                                .child(sliding_line::SlidingLine {
                                    text: credit.title(),
                                    colour: palette.text,
                                    size: 15.,
                                    bold: true,
                                    reduced_motion: settings.reduced_motion,
                                })
                                .child(mono(credit.licence, palette.muted, 11.))
                                .child(label().text(i18n::tr(credit.purpose)).color(palette.muted).font_size(13.))
                                .child(
                                    rect()
                                        .on_press({
                                            let url = credit.url;
                                            move |_| Platform::get().open_url(url)
                                        })
                                        .child(mono(credit.url, palette.receive, 11.)),
                                )
                                .into(),
                        );
                    }
                    page = page.child(card(rows));
                }
                // Third-party licence notices, in their original wording because they are legal attributions.
                page = page.child(section("NOTICES", palette));
                let notices = about::NOTICES
                    .iter()
                    .map(|notice| label().text(*notice).color(palette.muted).font_size(13.).into())
                    .collect::<Vec<Element>>();
                page.child(card(notices)).into()
            }
        }
        };
        // Search replaces the page content; it never creates a second scroll area in the header.
        let found = settings_search::search(
            &typed,
            &self.view,
            i18n::language(),
            settings_search::Features {
                notifications: self.notifications.is_some(),
                camera: self.camera_choice.is_some(),
                overlay: try_consume_context::<link::OverlayControl>().is_some(),
                offer_place: try_consume_context::<link::OverlayControl>().is_some()
                    || desktop_panel(),
                battery: try_consume_context::<link::BatteryControl>().is_some(),
                service_settings: &searched_settings,
            },
        );
        let mut results = rect().width(Size::fill()).spacing(10.);
        for entry in &found {
            let mut tab = self.tab;
            let mut navigation = navigation;
            results = results.child(settings_navigation::ResultButton {
                entry: entry.clone(),
                palette,
                on_select: EventHandler::new(move |target: settings_search::Target| {
                    query.set(String::new());
                    tab.set(target.tab());
                    navigation.selected.set(Some(target.clone()));
                    navigation.requested.set(Some(target));
                }),
            });
        }
        if searching && found.is_empty() {
            results = results.child(
                label()
                    .text(i18n::tr("Nothing in Settings looks like that."))
                    .color(palette.muted)
                    .font_size(14.),
            );
        }
        // Where the container of the groups and each group's title are laid out, in the window's own coordinates: a group's top in what is scrolled is
        // the difference of the two, which does not change as the page moves under the window. `None` until it was laid out.
        let title_tops: State<[Option<f32>; SettingsTab::ALL.len()]> =
            use_state(|| [None; SettingsTab::ALL.len()]);
        let tops: State<Vec<f32>> = use_state(Vec::new);
        let content: Element = if searching {
            results.into()
        } else if self.form.settings_places_come_from_the_top() {
            let mut page = rect().width(Size::fill()).spacing(28.).on_sized({
                let mut origin = origin;
                move |e: Event<SizedEventData>| origin.set_if_modified(Some(e.area.min_y()))
            });
            for (index, &which) in SettingsTab::shown().iter().enumerate() {
                page = page
                    .child(settings_navigation::anchor(
                        settings_search::Target::Group(which),
                        rect()
                            .width(Size::fill())
                            .horizontal()
                            .main_align(Alignment::Center)
                            .on_sized({
                                let mut title_tops = title_tops;
                                move |e: Event<SizedEventData>| {
                                    let mut next = *title_tops.peek();
                                    next[index] = Some(e.area.min_y());
                                    title_tops.set_if_modified(next);
                                }
                            })
                            .child(
                                label()
                                    .text(i18n::tr(which.title()))
                                    .color(palette.text)
                                    .font_size(26.)
                                    .font_weight(FontWeight::BOLD),
                            ),
                        palette,
                    ))
                    .child(build(which));
            }
            page.into()
        } else {
            rect()
                .width(Size::fill())
                .on_sized({
                    let mut origin = origin;
                    move |e: Event<SizedEventData>| origin.set_if_modified(Some(e.area.min_y()))
                })
                .child(settings_navigation::anchor(
                    settings_search::Target::Group(tab()),
                    build(tab()),
                    palette,
                ))
                .into()
        };
        // The tops of the groups once the container and the title of every group shown have been laid out. Only the groups shown
        // are drawn (Binds is not on Android): waiting for all of them never ended there, and the track had no group icons.
        let shown = SettingsTab::shown().len();
        // Track layout independently of the settings tree. Scrolling moves every title in
        // window coordinates, but normally leaves all offsets within the document unchanged.
        use_side_effect_with_deps(&shown, move |shown| {
            let titles = *title_tops.read();
            let Some(first) = titles[0] else { return };
            if titles[..*shown].iter().any(Option::is_none) {
                return;
            }
            let mut tops = tops;
            let measured = titles[..*shown]
                .iter()
                .flatten()
                .map(|y| (y - first).max(0.));
            if !tops.peek().iter().copied().eq(measured.clone()) {
                tops.set(measured.collect());
            }
        });
        // A requested anchor was laid out: where to scroll is its top less the container's, both of the same layout (the events of
        // one layout are all delivered before this runs again).
        // Copied out first: a guard taken in the condition would still hold the state while it is written.
        let at = *landed.read();
        if let (Some(origin), Some(at)) = (*origin.peek(), at) {
            let (mut landed, mut go_to) = (landed, go_to);
            landed.set(None);
            go_to.set(Some((at - origin).max(0.)));
        }

        // The fixed header owns only title/input; the page has one scrolling body.
        // In the sheet of a phone, whichever way it is held: every group one under another, and a small title.
        let compact = self.form.settings_places_come_from_the_top();
        let narrow_header = compact && self.room < 320.;
        let title = label()
            .text(i18n::tr("Settings"))
            .color(palette.text)
            .font_size(if compact { 20. } else { 32. })
            .font_weight(FontWeight::BOLD)
            .max_lines(1);
        let mut controls = rect()
            .width(Size::fill())
            .horizontal()
            .content(Content::flex())
            .cross_align(Alignment::center())
            .spacing(if compact { 10. } else { 20. });
        if !narrow_header {
            controls = controls.child(title.clone());
        }
        controls = controls
            .child(
                rect().width(Size::flex(1.)).child(
                    field::field(field::Profile::SEARCH, query)
                        .placeholder(i18n::tr("Search settings"))
                        .width(Size::fill()),
                ),
            )
            .maybe_child(self.on_close.clone().map(|close| {
                Button::new()
                    .on_press(move |_| close.call(()))
                    .child(label().text(i18n::tr("Close")).max_lines(1))
            }));
        let mut header = rect()
            .width(Size::fill())
            .spacing(8.)
            .padding((12., self.form.page_side_margin()));
        if narrow_header {
            header = header.child(title);
        }
        let header = header.child(controls);
        let scroll = Scrolling {
            palette,
            form: self.form,
            content,
            see_through: compact,
            tops: (compact && !searching).then_some(tops),
            go_to: Some(go_to),
        };
        rect()
            .expanded()
            .content(Content::flex())
            .background(if compact {
                palette.raised
            } else {
                palette.background
            })
            .on_capture_global_pointer_move(move |event: Event<PointerEventData>| {
                let point = event.global_location();
                let mut next = *tap.peek();
                next.moved((point.x as f32, point.y as f32));
                tap.set_if_modified(next);
            })
            .on_touch_cancel(move |_| tap.set(view::Tap::default()))
            .child(header)
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::px(1.))
                    .background(palette.border),
            )
            .child(
                rect()
                    .key(if searching {
                        "settings-search-results"
                    } else {
                        "settings-content"
                    })
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .child(scroll),
            )
            .into_element()
    }
}
