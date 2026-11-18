//! Named, portable palettes. Theme exchange never contains application preferences or device data.
use crate::{glyph, i18n, link::Link, theme::Palette};
use freya::prelude::*;
use peersus_core::{
    Settings,
    config::{
        CustomTheme, NamedTheme, SettingsPatch, Theme, ThemeBase, ThemeColor, valid_theme_name,
    },
    model::Command,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy)]
pub struct Preview(pub State<Option<CustomTheme>>);

const MAX_THEME_BYTES: usize = 65_536;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    name: String,
    palette: CustomTheme,
}

/// What the editor has to say after an action. A value and not a text, so that it is said in the language in use at the moment it is
/// shown, and so that what reads a file on another thread needs no language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    target_os = "android",
    expect(
        dead_code,
        reason = "a theme file is opened and exported by a desktop's dialogs only"
    )
)]
enum Note {
    Name,
    NotExported,
    TooLarge,
    NotATheme,
    Version,
    BadName,
    Hex,
    Exists,
    Saved,
    NotSaved,
    Copied,
    InField,
    Imported,
    Discarded,
    NotOpened,
    NotRead,
    Exported,
    Removed,
    NotRemoved,
}
impl Note {
    fn text(self) -> &'static str {
        match self {
            Note::Name => {
                i18n::tr("Choose a theme name of 1–64 characters, without surrounding spaces.")
            }
            Note::NotExported => i18n::tr("The theme could not be exported."),
            Note::TooLarge => i18n::tr("Theme files must be at most 64 KiB."),
            Note::NotATheme => i18n::tr("This is not a valid theme file."),
            Note::Version => i18n::tr("This theme format version is not supported."),
            Note::BadName => i18n::tr("The theme name is invalid."),
            Note::Hex => i18n::tr("Enter six hexadecimal RGB digits, such as #708090."),
            Note::Exists => i18n::tr(
                "This name already exists. Confirm replacement below, or use another name.",
            ),
            Note::Saved => i18n::tr("Theme saved in your library."),
            Note::NotSaved => i18n::tr("The theme could not be saved."),
            Note::Copied => i18n::tr("Theme copied. Paste it into a file or another installation."),
            Note::InField => i18n::tr(
                "Theme JSON is in the exchange field below; it can be selected and copied.",
            ),
            Note::Imported => {
                i18n::tr("Imported for preview. Save to register this theme in your library.")
            }
            Note::Discarded => i18n::tr("Changes discarded; saved preferences are unchanged."),
            Note::NotOpened => i18n::tr("The theme file could not be opened."),
            Note::NotRead => i18n::tr("The theme file could not be read."),
            Note::Exported => i18n::tr("Theme exported."),
            Note::Removed => i18n::tr("Theme removed from your library."),
            Note::NotRemoved => i18n::tr("The theme could not be removed."),
        }
    }
}

fn encode(name: &str, palette: &CustomTheme) -> Result<String, Note> {
    if !valid_theme_name(name) {
        return Err(Note::Name);
    }
    serde_json::to_string_pretty(&Document {
        version: 1,
        name: name.to_owned(),
        palette: palette.clone(),
    })
    .map_err(|_| Note::NotExported)
}
fn decode(bytes: &[u8]) -> Result<NamedTheme, Note> {
    if bytes.len() > MAX_THEME_BYTES {
        return Err(Note::TooLarge);
    }
    let document: Document = serde_json::from_slice(bytes).map_err(|_| Note::NotATheme)?;
    if document.version != 1 {
        return Err(Note::Version);
    }
    if !valid_theme_name(&document.name) {
        return Err(Note::BadName);
    }
    Ok(NamedTheme {
        name: document.name,
        palette: document.palette,
    })
}
fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}
/// Six hexadecimal digits, with or without the `#`. Nothing else: a sign, which the integer parser would take, is not a digit.
fn rgb(text: &str) -> Option<[u8; 3]> {
    let text = text.trim();
    let text = text.strip_prefix('#').unwrap_or(text);
    if text.len() != 6 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&text[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}
/// What the colour field shows for a role of a palette.
fn shown(theme: &CustomTheme, role: ThemeColor) -> String {
    hex(Palette::custom(theme).color(role))
}
pub(crate) const ROLES: &[(ThemeColor, &str)] = &[
    (ThemeColor::Background, "Page background"),
    (ThemeColor::Surface, "Cards and panels"),
    (ThemeColor::Raised, "Raised controls and hover"),
    (ThemeColor::Header, "Top header background"),
    (ThemeColor::Sidebar, "Sidebar background"),
    (ThemeColor::Border, "Borders and separators"),
    (ThemeColor::Text, "Primary text"),
    (ThemeColor::Muted, "Secondary text"),
    (ThemeColor::Portal, "Portal disc"),
    (ThemeColor::PortalText, "Portal text"),
    (ThemeColor::Send, "Send indicators"),
    (ThemeColor::Receive, "Receive indicators"),
    (ThemeColor::Good, "Connected and successful"),
    (ThemeColor::Bad, "Offline and failed"),
    (ThemeColor::Warning, "Warning text"),
    (ThemeColor::WarningBackground, "Warning background"),
    (ThemeColor::Button, "Primary button background"),
    (ThemeColor::ButtonText, "Primary button text"),
    (ThemeColor::Minimise, "Minimise window control"),
    (ThemeColor::Maximise, "Maximise window control"),
    (ThemeColor::Close, "Close window control"),
    (ThemeColor::Icons, "Navigation icons"),
    (ThemeColor::ActiveIcons, "Selected navigation icon"),
    (ThemeColor::ScannerOutline, "Camera detection outline"),
];
/// The English name of a role, which is its message in the catalog.
fn title(role: ThemeColor) -> &'static str {
    ROLES
        .iter()
        .find(|(candidate, _)| *candidate == role)
        .map_or("", |(_, name)| *name)
}

/// Text and what it is read on. Only pairs that are always drawn together: a palette may be as odd as its maker likes anywhere else.
const READ_ON: &[(ThemeColor, ThemeColor)] = &[
    (ThemeColor::Text, ThemeColor::Background),
    (ThemeColor::Text, ThemeColor::Surface),
    (ThemeColor::PortalText, ThemeColor::Portal),
    (ThemeColor::ButtonText, ThemeColor::Button),
    (ThemeColor::Warning, ThemeColor::WarningBackground),
];
/// The least contrast at which ordinary text is still read, as the accessibility guidelines put it.
const READABLE: f32 = 4.5;
/// The first text of a palette that could not be read on what it is drawn on, with that surface.
fn unreadable(palette: Palette) -> Option<(ThemeColor, ThemeColor)> {
    let of = |role| {
        let [r, g, b] = palette.color(role);
        (r, g, b)
    };
    READ_ON
        .iter()
        .copied()
        .find(|&(text, surface)| crate::theme::contrast_ratio(of(text), of(surface)) < READABLE)
}

/// What the editor is working on. One value, so that every way a palette arrives (the library, pasted text, a file) goes through
/// [`Work::adopt`] and every edit through [`Work::change`], and none of them can forget the field or the preview.
#[derive(Clone, Copy)]
struct Work {
    draft: State<CustomTheme>,
    selected: State<ThemeColor>,
    value: State<String>,
    name: State<String>,
    overwrite: State<Option<(String, CustomTheme, CustomTheme)>>,
    preview: State<Option<CustomTheme>>,
}
impl Work {
    /// An edit of the draft: shown in the whole interface, saved nowhere.
    fn change(mut self, next: CustomTheme) {
        let role = *self.selected.peek();
        self.value.set(shown(&next, role));
        self.preview.set(Some(next.clone()));
        self.draft.set(next);
    }
    /// A whole palette from elsewhere becomes the draft. `previewed` is whether it is only shown (an import) or already the saved
    /// palette (a theme of the library, which the caller saves as the one in use).
    fn adopt(mut self, theme: NamedTheme, previewed: bool) {
        let role = *self.selected.peek();
        self.value.set(shown(&theme.palette, role));
        self.name.set(theme.name);
        self.overwrite.set(None);
        self.preview.set(previewed.then(|| theme.palette.clone()));
        self.draft.set(theme.palette);
    }
}

#[derive(PartialEq)]
pub struct Editor {
    pub link: Link,
    pub settings: Settings,
    pub palette: Palette,
}
impl Component for Editor {
    fn render(&self) -> impl IntoElement {
        let language = i18n::language();
        let initial = self.settings.custom_theme.clone();
        let mut draft = use_state(move || initial);
        let mut selected = use_state(|| ThemeColor::Background);
        let mut value = use_state(|| shown(&draft.read().clone(), selected()));
        let name = use_state(String::new);
        let mut exchange = use_state(String::new);
        let mut note = use_state(|| None::<Note>);
        let mut overwrite = use_state(|| None::<(String, CustomTheme, CustomTheme)>);
        let mut forgetting = use_state(|| None::<String>);
        let Preview(mut preview) = consume_context::<Preview>();
        use_drop(move || preview.set(None));
        let work = Work {
            draft,
            selected,
            value,
            name,
            overwrite,
            preview,
        };
        // A colour is applied as soon as the field holds one: there is no second step to forget. The field is also filled by the
        // editor itself (another element chosen, a theme loaded), and then it holds what the draft already has, which changes nothing.
        use_side_effect_with_deps(&value.read().clone(), move |text: &String| {
            let Some(colour) = rgb(text) else {
                return;
            };
            let role = *selected.peek();
            let mut next = draft.peek().clone();
            if Palette::custom(&next).color(role) != colour {
                next.colors.insert(role, colour);
                preview.set(Some(next.clone()));
                draft.set(next);
            }
        });
        // The panel is drawn in the colours the application is in, so a light page does not get a black block with white buttons on it.
        // It falls back to the readable colours of the draft's base only when the application's own text could not be read on its page.
        let chrome =
            if crate::theme::contrast_ratio(self.palette.text, self.palette.surface) >= READABLE {
                self.palette
            } else {
                match draft.read().clone().base {
                    ThemeBase::Dark => crate::theme::DARK,
                    ThemeBase::Light => crate::theme::LIGHT,
                }
            };
        let palette = Palette::custom(&draft.read().clone());
        let base = draft.read().clone().base;
        let base_button = |text: &'static str, wanted: ThemeBase| -> Element {
            let button = Button::new()
                .on_press(move |_| {
                    let mut next = draft.read().clone();
                    next.base = wanted;
                    work.change(next);
                })
                .child(crate::text::caption(text));
            if base == wanted {
                button.filled().into()
            } else {
                button.into()
            }
        };
        let mut choices = rect().width(Size::fill()).spacing(4.);
        for &(role, title) in ROLES {
            let [r, g, b] = palette.color(role);
            let button = Button::new()
                .on_press(move |_| {
                    selected.set(role);
                    value.set(shown(&draft.read().clone(), role));
                })
                .child(
                    rect()
                        .horizontal()
                        .spacing(8.)
                        .cross_align(Alignment::center())
                        .child(
                            rect()
                                .width(Size::px(16.))
                                .height(Size::px(16.))
                                .corner_radius(4.)
                                .background((r, g, b))
                                // Ringed in the panel's own border colour, so a colour that is the button's own is still seen as a square.
                                .border(
                                    Border::new()
                                        .fill(chrome.muted)
                                        .width(1.)
                                        .alignment(BorderAlignment::Inner),
                                ),
                        )
                        .child(label().text(i18n::tr(title))),
                );
            choices = choices.child(if selected() == role {
                button.filled().into_element()
            } else {
                button.into_element()
            });
        }
        let save_link = self.link.clone();
        let library = self.settings.saved_themes.clone();
        let save = move |_| {
            let title = name.read().clone();
            if !valid_theme_name(&title) {
                note.set(Some(Note::Name));
                return;
            }
            let palette = draft.read().clone();
            let replacement = library
                .get(&title)
                .filter(|saved| **saved != palette)
                .map(|saved| (title.clone(), saved.clone(), palette.clone()));
            if replacement.is_some() && *overwrite.read() != replacement {
                note.set(Some(Note::Exists));
                overwrite.set(replacement);
                return;
            }
            let client = save_link.client().clone();
            spawn(async move {
                match client
                    .command(Command::PatchSettings {
                        patch: Box::new(SettingsPatch {
                            theme: Some(Theme::Custom),
                            custom_theme: Some(palette.clone()),
                            save_theme: Some(NamedTheme {
                                name: title,
                                palette,
                            }),
                            ..Default::default()
                        }),
                    })
                    .await
                {
                    Ok(_) => {
                        preview.set(None);
                        overwrite.set(None);
                        note.set(Some(Note::Saved));
                    }
                    Err(_) => note.set(Some(Note::NotSaved)),
                }
            });
        };
        let mut saved = rect().width(Size::fill()).spacing(6.);
        for (title, palette) in &self.settings.saved_themes {
            let load = {
                let theme = NamedTheme {
                    name: title.clone(),
                    palette: palette.clone(),
                };
                let link = self.link.clone();
                Button::new()
                    .child(i18n::format(language, "Load {name}", &[("name", title)]))
                    .on_press(move |_| {
                        work.adopt(theme.clone(), false);
                        forgetting.set(None);
                        note.set(None);
                        link.send(Command::PatchSettings {
                            patch: Box::new(SettingsPatch {
                                theme: Some(Theme::Custom),
                                custom_theme: Some(theme.palette.clone()),
                                ..Default::default()
                            }),
                        });
                    })
            };
            // Removing cannot be undone, so it is asked for twice: the first press only turns the button into its own confirmation.
            let asked = forgetting.read().as_deref() == Some(title.as_str());
            let remove = {
                let title = title.clone();
                let client = self.link.client().clone();
                Button::new()
                    .child(if asked {
                        i18n::tr("Confirm removal")
                    } else {
                        i18n::tr("Remove")
                    })
                    .on_press(move |_| {
                        if forgetting.read().as_deref() != Some(title.as_str()) {
                            forgetting.set(Some(title.clone()));
                            return;
                        }
                        forgetting.set(None);
                        let (client, title) = (client.clone(), title.clone());
                        spawn(async move {
                            let removed = client
                                .command(Command::PatchSettings {
                                    patch: Box::new(SettingsPatch {
                                        forget_theme: Some(title),
                                        ..Default::default()
                                    }),
                                })
                                .await;
                            note.set(Some(if removed.is_ok() {
                                Note::Removed
                            } else {
                                Note::NotRemoved
                            }));
                        });
                    })
            };
            saved = saved.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::wrap_spacing(6.))
                    .spacing(6.)
                    .cross_align(Alignment::center())
                    .child(load)
                    .child(if asked {
                        remove.filled().into_element()
                    } else {
                        remove.into_element()
                    }),
            );
        }
        let copy = move |_| match encode(&name.read().clone(), &draft.read().clone()) {
            Ok(json) => {
                exchange.set(json.clone());
                note.set(Some(if Clipboard::set(json).is_ok() {
                    Note::Copied
                } else {
                    Note::InField
                }));
            }
            Err(error) => note.set(Some(error)),
        };
        let import = move |_| match decode(exchange.read().clone().as_bytes()) {
            Ok(theme) => {
                work.adopt(theme, true);
                note.set(Some(Note::Imported));
            }
            Err(error) => note.set(Some(error)),
        };
        let saved_palette = self.settings.custom_theme.clone();
        let discard = move |_| {
            let role = selected();
            value.set(shown(&saved_palette, role));
            draft.set(saved_palette.clone());
            overwrite.set(None);
            preview.set(None);
            note.set(Some(Note::Discarded));
        };
        let reset = move |_| {
            let mut next = draft.read().clone();
            next.colors.remove(&selected());
            work.change(next);
        };
        let confirming = overwrite
            .read()
            .as_ref()
            .is_some_and(|(title, _, palette)| *title == *name.read() && *palette == *draft.read());
        let mut body = rect()
            .width(Size::fill())
            .spacing(10.)
            .padding(14.)
            .corner_radius(12.)
            .background(chrome.surface)
            .child(
                label()
                    .text(i18n::tr("CUSTOM THEME"))
                    .color(chrome.text)
                    .font_size(16.),
            )
            .child(
                label()
                    .text(i18n::tr("Choose an element and type its colour. The preview and the whole interface follow as you type; nothing is saved until you save the theme."))
                    .color(chrome.muted)
                    .font_size(13.),
            );
        // What the last action came to is said here, under the title, where it is seen whichever button was pressed: the panel is
        // longer than a window, and a line at its foot is off the screen for every button but the last.
        if let Some(said) = note() {
            body = body.child(
                rect()
                    .width(Size::fill())
                    .padding((8., 10.))
                    .corner_radius(8.)
                    .background(chrome.raised)
                    .child(label().text(said.text()).color(chrome.text).font_size(13.)),
            );
        }
        if let Some((text, surface)) = unreadable(palette) {
            body = body.child(
                rect()
                    .width(Size::fill())
                    .padding((8., 10.))
                    .corner_radius(8.)
                    .background(chrome.warning_background)
                    .child(
                        label()
                            .text(i18n::format(
                                language,
                                "Low contrast: “{text}” on “{surface}” may be hard to read.",
                                &[
                                    ("text", i18n::tr(title(text))),
                                    ("surface", i18n::tr(title(surface))),
                                ],
                            ))
                            .color(chrome.warning)
                            .font_size(13.),
                    ),
            );
        }
        let mut body = body
            .child(
                rect()
                    // Wrapped, here and for the library's rows: in a longer language two buttons do not fit a phone's width side
                    // by side, and a button squeezed into what is left breaks its words letter by letter.
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::wrap_spacing(8.))
                    .spacing(8.)
                    .child(base_button(i18n::tr("Dark base"), ThemeBase::Dark))
                    .child(base_button(i18n::tr("Light base"), ThemeBase::Light)),
            )
            .child(
                rect()
                    .height(Size::px(190.))
                    .width(Size::fill())
                    .child(ScrollView::new().child(choices)),
            )
            .child(
                label()
                    .text(i18n::format(
                        language,
                        "Editing: {element}",
                        &[("element", i18n::tr(title(selected())))],
                    ))
                    .color(chrome.text),
            )
            .child(
                crate::field::field(crate::field::Profile::TEXT, value)
                    .placeholder("#RRGGBB")
                    .width(Size::fill()),
            );
        if rgb(&value.read()).is_none() {
            body = body.child(
                label()
                    .text(Note::Hex.text())
                    .color(chrome.muted)
                    .font_size(13.),
            );
        }
        let body = body
            .child(sample(palette, selected()))
            // One under the other: side by side they do not fit a phone's width in most languages, and a button broken over four lines
            // is not read.
            .child(
                Button::new()
                    .child(i18n::tr("Reset selected element"))
                    .on_press(reset),
            )
            .child(
                Button::new()
                    .child(i18n::tr("Discard changes"))
                    .on_press(discard),
            )
            .child(
                crate::field::field(crate::field::Profile::TEXT, name)
                    .placeholder(i18n::tr("Your theme name"))
                    .width(Size::fill()),
            )
            .child(
                Button::new()
                    .filled()
                    .child(if confirming {
                        i18n::tr("Confirm replace existing theme")
                    } else {
                        i18n::tr("Save named theme")
                    })
                    .on_press(save),
            )
            .child(
                label()
                    .text(i18n::tr("SAVED THEME LIBRARY"))
                    .color(chrome.text),
            )
            .child(saved)
            .child(label().text(i18n::tr("IMPORT / EXPORT")).color(chrome.text))
            .child(
                Button::new()
                    .child(i18n::tr("Export / copy theme JSON"))
                    .on_press(copy),
            )
            .child(
                crate::field::field(crate::field::Profile::TEXT, exchange)
                    .placeholder(i18n::tr("Paste exported theme JSON here"))
                    .width(Size::fill()),
            )
            .child(
                Button::new()
                    .child(i18n::tr("Import JSON for preview"))
                    .on_press(import),
            );
        #[cfg(not(target_os = "android"))]
        let body = {
            let mut body = body;
            let filter = i18n::tr("Theme JSON");
            let link = self.link.clone();
            body = body.child(
                Button::new()
                    .child(i18n::tr("Import theme file…"))
                    .on_press(move |_| {
                        let link = link.clone();
                        spawn(async move {
                            if let Some(file) = rfd::AsyncFileDialog::new()
                                .add_filter(filter, &["json"])
                                .pick_file()
                                .await
                            {
                                let path = file.path().to_owned();
                                let result = link
                                    .blocking(move || {
                                        use std::io::Read;
                                        let file = std::fs::File::open(path)
                                            .map_err(|_| Note::NotOpened)?;
                                        let mut bytes = Vec::new();
                                        file.take(MAX_THEME_BYTES as u64 + 1)
                                            .read_to_end(&mut bytes)
                                            .map_err(|_| Note::NotRead)?;
                                        decode(&bytes)
                                    })
                                    .await;
                                match result {
                                    Ok(Ok(theme)) => {
                                        work.adopt(theme, true);
                                        note.set(Some(Note::Imported));
                                    }
                                    Ok(Err(error)) => note.set(Some(error)),
                                    Err(_) => note.set(Some(Note::NotRead)),
                                }
                            }
                        });
                    }),
            );
            let link = self.link.clone();
            body = body.child(
                Button::new()
                    .child(i18n::tr("Export theme file…"))
                    .on_press(move |_| {
                        let json = match encode(&name.read().clone(), &draft.read().clone()) {
                            Ok(json) => json,
                            Err(error) => {
                                note.set(Some(error));
                                return;
                            }
                        };
                        let link = link.clone();
                        spawn(async move {
                            if let Some(file) = rfd::AsyncFileDialog::new()
                                .set_file_name("theme.json")
                                .add_filter(filter, &["json"])
                                .save_file()
                                .await
                            {
                                let path = file.path().to_owned();
                                note.set(Some(
                                    match link.blocking(move || std::fs::write(path, json)).await {
                                        Ok(Ok(())) => Note::Exported,
                                        _ => Note::NotExported,
                                    },
                                ));
                            }
                        });
                    }),
            );
            body
        };
        body
    }
}

/// The selected role is shown on the SAME kind of element, not as an unrelated colour swatch.
fn sample(p: Palette, role: ThemeColor) -> Element {
    let (title, element): (&str, Element) = match role {
        ThemeColor::Header => (
            i18n::tr("Top header"),
            rect()
                .padding(14.)
                .background(p.header)
                .child(
                    label()
                        .text(i18n::tr("Header · navigation and window controls"))
                        .color(p.text),
                )
                .into(),
        ),
        ThemeColor::Sidebar => (
            i18n::tr("Sidebar"),
            rect()
                .padding(14.)
                .background(p.sidebar)
                .child(glyph::draw(
                    glyph::of_tab(crate::view::SettingsTab::Appearance),
                    glyph::LIST,
                    p.icons,
                ))
                .child(label().text(i18n::tr("Appearance")).color(p.text))
                .into(),
        ),
        ThemeColor::Icons | ThemeColor::ActiveIcons => (
            i18n::tr("Navigation icon"),
            glyph::draw(
                glyph::of_tab(crate::view::SettingsTab::Appearance),
                32.,
                if role == ThemeColor::Icons {
                    p.icons
                } else {
                    p.active_icons
                },
            ),
        ),
        ThemeColor::Minimise | ThemeColor::Maximise | ThemeColor::Close => {
            let [r, g, b] = p.color(role);
            (
                i18n::tr("Window control (preview only)"),
                rect()
                    .width(Size::px(crate::HEADER_ROUND))
                    .height(Size::px(crate::HEADER_ROUND))
                    .corner_radius(crate::HEADER_ROUND / 2.)
                    .background((r, g, b))
                    .into(),
            )
        }
        ThemeColor::Portal | ThemeColor::PortalText => (
            i18n::tr("Portal disc and its text"),
            rect()
                .width(Size::px(120.))
                .height(Size::px(120.))
                .corner_radius(60.)
                .center()
                .background(p.portal)
                .child(label().text(i18n::tr("PORTAL")).color(p.portal_text))
                .into(),
        ),
        ThemeColor::Button | ThemeColor::ButtonText => (
            i18n::tr("Primary button"),
            rect()
                .padding((12., 20.))
                .corner_radius(10.)
                .background(p.button)
                .child(
                    label()
                        .text(i18n::tr("Primary action"))
                        .color(p.button_text),
                )
                .into(),
        ),
        ThemeColor::Warning | ThemeColor::WarningBackground => (
            i18n::tr("Warning notice"),
            rect()
                .padding(14.)
                .background(p.warning_background)
                .child(
                    label()
                        .text(i18n::tr("Warning: verify received files."))
                        .color(p.warning),
                )
                .into(),
        ),
        ThemeColor::ScannerOutline => (
            i18n::tr("Camera detection marker"),
            rect()
                .width(Size::px(40.))
                .height(Size::px(40.))
                .corner_radius(20.)
                .background(p.good)
                .border(
                    Border::new()
                        .fill(p.scanner_outline)
                        .width(2.)
                        .alignment(BorderAlignment::Inner),
                )
                .into(),
        ),
        ThemeColor::Send | ThemeColor::Receive | ThemeColor::Good | ThemeColor::Bad => {
            let text = match role {
                ThemeColor::Send => i18n::tr("Sending →"),
                ThemeColor::Receive => i18n::tr("← Receiving"),
                ThemeColor::Good => i18n::tr("● Connected · Complete"),
                _ => i18n::tr("● Offline · Failed"),
            };
            let [r, g, b] = p.color(role);
            (
                i18n::tr("Transfer or connection indicator"),
                label().text(text).color((r, g, b)).font_size(18.).into(),
            )
        }
        ThemeColor::Text | ThemeColor::Muted => (
            i18n::tr("Text on the page"),
            label()
                .text(if role == ThemeColor::Text {
                    i18n::tr("Primary text — names and headings")
                } else {
                    i18n::tr("Secondary text — descriptions and details")
                })
                .color(if role == ThemeColor::Text {
                    p.text
                } else {
                    p.muted
                })
                .into(),
        ),
        _ => {
            let [r, g, b] = p.color(role);
            (
                i18n::tr("Surface and border"),
                rect()
                    .padding(18.)
                    .background(if role == ThemeColor::Border {
                        p.surface
                    } else {
                        (r, g, b)
                    })
                    .border(Border::new().fill(p.border).width(2.))
                    .child(label().text(i18n::tr("Panel / page content")).color(p.text))
                    .into(),
            )
        }
    };
    rect()
        .width(Size::fill())
        .padding(12.)
        .spacing(10.)
        .background(p.background)
        .border(Border::new().fill(p.border).width(2.))
        .child(
            label()
                .text(i18n::format(
                    i18n::language(),
                    "PREVIEW — {element}",
                    &[("element", title)],
                ))
                .color(p.text)
                .font_size(12.),
        )
        .child(element)
        .into()
}

/// What the Appearance page gives Settings search: the theme choices, the custom theme's roles, the saved library and the file
/// round trip.
pub fn search(index: &mut crate::settings_index::Index, view: &crate::view::View) {
    let choices = ["System", "Light", "Dark", "Custom", "Accent colour"]
        .map(|choice| index.tr(choice))
        .join(" · ");
    index.add("Appearance", choices);
    if !cfg!(target_os = "android") {
        let words = index
            .tr("A closed bar shows only its pictures. Off, a closed bar is not shown at all.");
        index.add("Keep icons when the bar is closed", words);
    }
    let words = index.tr(
        "The pages extensions add share one place in the navigation, Extensions, which switches between them. Off, each has a place of its own.",
    );
    index.add("Extension pages in one place", words);
    let words = index.tr("Turn off decorative animation.");
    index.add("Reduce motion", words);
    index.add("CUSTOM THEME", String::new());
    let custom = index.tr("CUSTOM THEME");
    for (_, title) in ROLES {
        index.add(title, custom.clone());
    }
    index.add("SAVED THEME LIBRARY", String::new());
    let library = index.tr("SAVED THEME LIBRARY");
    for title in view.settings.saved_themes.keys() {
        index.add(title, library.clone());
    }
    let words = index.tr("Import theme file · Export theme file · JSON");
    index.add("IMPORT / EXPORT", words);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_theme_round_trip_keeps_name_base_and_exact_role_without_app_data() {
        let mut palette = CustomTheme {
            base: ThemeBase::Light,
            ..Default::default()
        };
        palette.colors.insert(ThemeColor::Header, [12, 34, 56]);
        let encoded = encode("My theme", &palette).unwrap();
        let imported = decode(encoded.as_bytes()).unwrap();
        assert_eq!(
            imported,
            NamedTheme {
                name: "My theme".into(),
                palette
            }
        );
    }
    #[test]
    fn import_rejects_unknown_roles_versions_channels_and_extra_settings() {
        for (text, why) in [
            (r#"{"version":2,"name":"X","palette":{}}"#, Note::Version),
            (
                r#"{"version":1,"name":"X","palette":{"colors":{"unknown":[0,0,0]}}}"#,
                Note::NotATheme,
            ),
            (
                r#"{"version":1,"name":"X","palette":{"colors":{"header":[999,0,0]}}}"#,
                Note::NotATheme,
            ),
            (
                r#"{"version":1,"name":"X","palette":{},"settings":{}}"#,
                Note::NotATheme,
            ),
            (r#"{"version":1,"name":" X","palette":{}}"#, Note::BadName),
        ] {
            assert_eq!(decode(text.as_bytes()), Err(why), "{text}");
        }
        assert_eq!(
            decode(&vec![b' '; MAX_THEME_BYTES + 1]),
            Err(Note::TooLarge)
        );
        assert_eq!(encode("   ", &CustomTheme::default()), Err(Note::Name));
    }
    #[test]
    fn a_colour_is_six_hexadecimal_digits_with_or_without_the_hash_and_nothing_else() {
        assert_eq!(rgb("#708090"), Some([0x70, 0x80, 0x90]));
        assert_eq!(rgb(" a1B2c3 "), Some([0xa1, 0xb2, 0xc3]));
        // What the field is filled with is always taken back as the same colour.
        assert_eq!(rgb(&hex([1, 2, 255])), Some([1, 2, 255]));
        for text in [
            "", "#", "#70809", "#7080900", "#70809g", "+10203", "#+1+2+3", "é0000",
        ] {
            assert_eq!(rgb(text), None, "{text}");
        }
    }
    #[test]
    fn every_role_has_one_name_and_the_name_is_a_message_of_the_catalog() {
        for &(role, name) in ROLES {
            assert_eq!(title(role), name);
            assert!(i18n::has(name), "{name}");
        }
        let mut names: Vec<_> = ROLES.iter().map(|(_, name)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ROLES.len());
    }
    #[test]
    fn the_built_in_palettes_are_readable_and_a_text_in_the_colour_of_its_surface_is_named() {
        for base in [ThemeBase::Dark, ThemeBase::Light] {
            let mut theme = CustomTheme {
                base,
                ..Default::default()
            };
            assert_eq!(unreadable(Palette::custom(&theme)), None);
            let button = Palette::custom(&theme).color(ThemeColor::Button);
            theme.colors.insert(ThemeColor::ButtonText, button);
            assert_eq!(
                unreadable(Palette::custom(&theme)),
                Some((ThemeColor::ButtonText, ThemeColor::Button))
            );
            // A colour that is read on nothing (an indicator) is never reported, whatever it is.
            theme.colors.remove(&ThemeColor::ButtonText);
            let page = Palette::custom(&theme).color(ThemeColor::Background);
            theme.colors.insert(ThemeColor::Send, page);
            assert_eq!(unreadable(Palette::custom(&theme)), None);
        }
    }
}
