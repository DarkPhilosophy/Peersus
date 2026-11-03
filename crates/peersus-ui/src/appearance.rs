//! Named, portable palettes. Theme exchange never contains application preferences or device data.
use crate::{glyph, link::Link, theme::Palette};
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

fn encode(name: &str, palette: &CustomTheme) -> Result<String, &'static str> {
    if !valid_theme_name(name) {
        return Err("Choose a theme name of 1–64 characters, without surrounding spaces.");
    }
    serde_json::to_string_pretty(&Document {
        version: 1,
        name: name.to_owned(),
        palette: palette.clone(),
    })
    .map_err(|_| "The theme could not be exported.")
}
fn decode(bytes: &[u8]) -> Result<NamedTheme, &'static str> {
    if bytes.len() > MAX_THEME_BYTES {
        return Err("Theme files must be at most 64 KiB.");
    }
    let document: Document =
        serde_json::from_slice(bytes).map_err(|_| "This is not a valid theme file.")?;
    if document.version != 1 {
        return Err("This theme format version is not supported.");
    }
    if !valid_theme_name(&document.name) {
        return Err("The theme name is invalid.");
    }
    Ok(NamedTheme {
        name: document.name,
        palette: document.palette,
    })
}
fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}
fn rgb(text: &str) -> Result<[u8; 3], &'static str> {
    let text = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if text.len() != 6 || !text.is_ascii() {
        return Err("Enter six hexadecimal RGB digits, such as #708090.");
    }
    Ok([
        u8::from_str_radix(&text[0..2], 16).map_err(|_| "Invalid red channel.")?,
        u8::from_str_radix(&text[2..4], 16).map_err(|_| "Invalid green channel.")?,
        u8::from_str_radix(&text[4..6], 16).map_err(|_| "Invalid blue channel.")?,
    ])
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

#[derive(PartialEq)]
pub struct Editor {
    pub link: Link,
    pub settings: Settings,
    pub palette: Palette,
}
impl Component for Editor {
    fn render(&self) -> impl IntoElement {
        let initial = self.settings.custom_theme.clone();
        let mut draft = use_state(move || initial);
        let mut selected = use_state(|| ThemeColor::Background);
        let mut value = use_state(|| hex(Palette::custom(&draft.read().clone()).color(selected())));
        let mut name = use_state(String::new);
        let mut exchange = use_state(String::new);
        let mut note = use_state(String::new);
        let mut overwrite = use_state(|| None::<(String, CustomTheme, CustomTheme)>);
        let Preview(mut preview) = consume_context::<Preview>();
        use_drop(move || preview.set(None));
        // The panel is drawn in the colours the application is in, so a light page does not get a black block with white buttons on it.
        // It falls back to the readable colours of the draft's base only when the application's own text could not be read on its page.
        let chrome = if crate::theme::contrast_ratio(self.palette.text, self.palette.surface) >= 4.5
        {
            self.palette
        } else {
            match draft.read().clone().base {
                ThemeBase::Dark => crate::theme::DARK,
                ThemeBase::Light => crate::theme::LIGHT,
            }
        };
        let palette = Palette::custom(&draft.read().clone());
        let selected_label = ROLES
            .iter()
            .find(|(role, _)| *role == selected())
            .map(|(_, name)| *name)
            .unwrap_or("Element");
        let mut choices = rect().width(Size::fill()).spacing(4.);
        for &(role, title) in ROLES {
            let button = Button::new()
                .on_press(move |_| {
                    selected.set(role);
                    value.set(hex(Palette::custom(&draft.read().clone()).color(role)));
                })
                .child(title);
            choices = choices.child(if selected() == role {
                button.filled().into_element()
            } else {
                button.into_element()
            });
        }
        let apply_color = move |_| match rgb(&value.read().clone()) {
            Ok(color) => {
                let mut next = draft.read().clone();
                next.colors.insert(selected(), color);
                preview.set(Some(next.clone()));
                draft.set(next);
                note.set(String::new());
            }
            Err(error) => note.set(error.to_owned()),
        };
        let save_link = self.link.clone();
        let library = self.settings.saved_themes.clone();
        let save = move |_| {
            let title = name.read().clone();
            if !valid_theme_name(&title) {
                note.set(
                    "Choose a theme name of 1–64 characters, without surrounding spaces.".into(),
                );
                return;
            }
            let palette = draft.read().clone();
            let replacement = library
                .get(&title)
                .filter(|saved| **saved != palette)
                .map(|saved| (title.clone(), saved.clone(), palette.clone()));
            if replacement.is_some() && *overwrite.read() != replacement {
                note.set(
                    "This name already exists. Confirm replacement below, or use another name."
                        .into(),
                );
                overwrite.set(replacement);
                return;
            }
            let client = save_link.client().clone();
            spawn(async move {
                match client
                    .command(Command::PatchSettings {
                        patch: SettingsPatch {
                            theme: Some(Theme::Custom),
                            custom_theme: Some(palette.clone()),
                            save_theme: Some(NamedTheme {
                                name: title,
                                palette,
                            }),
                            ..Default::default()
                        },
                    })
                    .await
                {
                    Ok(_) => {
                        preview.set(None);
                        overwrite.set(None);
                        note.set("Theme saved in your library.".into());
                    }
                    Err(_) => note.set("The theme could not be saved.".into()),
                }
            });
        };
        let mut saved = rect().width(Size::fill()).spacing(6.);
        for (title, palette) in &self.settings.saved_themes {
            let title = title.clone();
            let palette = palette.clone();
            let link = self.link.clone();
            saved = saved.child(
                Button::new()
                    .child(format!("Load {title}"))
                    .on_press(move |_| {
                        draft.set(palette.clone());
                        name.set(title.clone());
                        value.set(hex(Palette::custom(&palette).color(selected())));
                        preview.set(None);
                        overwrite.set(None);
                        link.send(Command::PatchSettings {
                            patch: SettingsPatch {
                                theme: Some(Theme::Custom),
                                custom_theme: Some(palette.clone()),
                                ..Default::default()
                            },
                        });
                    }),
            );
        }
        let copy = move |_| match encode(&name.read().clone(), &draft.read().clone()) {
            Ok(json) => {
                exchange.set(json.clone());
                note.set(
                    if Clipboard::set(json).is_ok() {
                        "Theme copied. Paste it into a file or another installation."
                    } else {
                        "Theme JSON is in the exchange field below; it can be selected and copied."
                    }
                    .into(),
                );
            }
            Err(error) => note.set(error.into()),
        };
        let import = move |_| match decode(exchange.read().clone().as_bytes()) {
            Ok(theme) => {
                name.set(theme.name);
                draft.set(theme.palette.clone());
                preview.set(Some(theme.palette.clone()));
                value.set(hex(Palette::custom(&theme.palette).color(selected())));
                overwrite.set(None);
                note.set(
                    "Imported for preview. Save to register this theme in your library.".into(),
                );
            }
            Err(error) => note.set(error.into()),
        };
        let body = rect().width(Size::fill()).spacing(10.).padding(14.).corner_radius(12.)
            .background(chrome.surface)
            .child(label().text("CUSTOM THEME").color(chrome.text).font_size(16.))
            .child(label().text("Choose an element, edit its RGB colour, then apply. The preview shows the named element and updates the real interface without saving.").color(chrome.muted).font_size(13.))
            .child(rect().horizontal().spacing(8.)
                .child(Button::new().child("Dark base").on_press(move |_| { let mut next = draft.read().clone(); next.base = ThemeBase::Dark; preview.set(Some(next.clone())); draft.set(next); value.set(hex(Palette::custom(&draft.read().clone()).color(selected()))); }))
                .child(Button::new().child("Light base").on_press(move |_| { let mut next = draft.read().clone(); next.base = ThemeBase::Light; preview.set(Some(next.clone())); draft.set(next); value.set(hex(Palette::custom(&draft.read().clone()).color(selected()))); })))
            .child(rect().height(Size::px(190.)).width(Size::fill()).child(ScrollView::new().child(choices)))
            .child(label().text(format!("Editing: {selected_label}")).color(chrome.text))
            .child(Input::new(value).placeholder("#RRGGBB").width(Size::fill()))
            .child(Button::new().child("Apply colour to preview").on_press(apply_color))
            .child(sample(palette, selected()))
            .child(Button::new().child("Cancel preview").on_press(move |_| { preview.set(None); note.set("Preview cancelled; saved preferences are unchanged.".into()); }))
            .child(Button::new().child("Reset selected element").on_press(move |_| {
                let mut next = draft.read().clone(); next.colors.remove(&selected()); value.set(hex(Palette::custom(&next).color(selected())));
                draft.set(next.clone()); preview.set(Some(next));
            }))
            .child(Input::new(name).placeholder("Your theme name").width(Size::fill()))
            .child(Button::new().filled().child(if overwrite.read().as_ref().is_some_and(|(title, _, palette)| *title == *name.read() && *palette == *draft.read()) { "Confirm replace existing theme" } else { "Save named theme" }).on_press(save))
            .child(label().text("SAVED THEME LIBRARY").color(chrome.text))
            .child(saved)
            .child(label().text("IMPORT / EXPORT").color(chrome.text))
            .child(Button::new().child("Export / copy theme JSON").on_press(copy))
            .child(Input::new(exchange).placeholder("Paste exported theme JSON here").width(Size::fill()))
            .child(Button::new().child("Import JSON for preview").on_press(import));
        #[cfg(not(target_os = "android"))]
        let body = {
            let mut body = body;
            let link = self.link.clone();
            body = body.child(
                Button::new()
                    .child("Import theme file…")
                    .on_press(move |_| {
                        let link = link.clone();
                        spawn(async move {
                            if let Some(file) = rfd::AsyncFileDialog::new()
                                .add_filter("Theme JSON", &["json"])
                                .pick_file()
                                .await
                            {
                                let path = file.path().to_owned();
                                let result = link
                                    .blocking(move || {
                                        use std::io::Read;
                                        let file = std::fs::File::open(path)
                                            .map_err(|_| "The theme file could not be opened.")?;
                                        let mut bytes = Vec::new();
                                        file.take(MAX_THEME_BYTES as u64 + 1)
                                            .read_to_end(&mut bytes)
                                            .map_err(|_| "The theme file could not be read.")?;
                                        decode(&bytes)
                                    })
                                    .await;
                                match result {
                                    Ok(Ok(theme)) => {
                                        name.set(theme.name);
                                        draft.set(theme.palette.clone());
                                        preview.set(Some(theme.palette.clone()));
                                        value.set(hex(
                                            Palette::custom(&theme.palette).color(selected())
                                        ));
                                        overwrite.set(None);
                                        note.set(
                                            "Imported for preview. Save to register this theme."
                                                .into(),
                                        );
                                    }
                                    Ok(Err(error)) => note.set(error.into()),
                                    Err(_) => note.set("The theme file could not be read.".into()),
                                }
                            }
                        });
                    }),
            );
            let link = self.link.clone();
            body = body.child(
                Button::new()
                    .child("Export theme file…")
                    .on_press(move |_| {
                        let json = match encode(&name.read().clone(), &draft.read().clone()) {
                            Ok(json) => json,
                            Err(error) => {
                                note.set(error.into());
                                return;
                            }
                        };
                        let link = link.clone();
                        spawn(async move {
                            if let Some(file) = rfd::AsyncFileDialog::new()
                                .set_file_name("theme.json")
                                .add_filter("Theme JSON", &["json"])
                                .save_file()
                                .await
                            {
                                let path = file.path().to_owned();
                                match link.blocking(move || std::fs::write(path, json)).await {
                                    Ok(Ok(())) => note.set("Theme exported.".into()),
                                    _ => note.set("The theme could not be exported.".into()),
                                }
                            }
                        });
                    }),
            );
            body
        };
        body.child(
            label()
                .text(note.read().clone())
                .color(chrome.text)
                .font_size(13.),
        )
    }
}

/// The selected role is shown on the SAME kind of element, not as an unrelated colour swatch.
fn sample(p: Palette, role: ThemeColor) -> Element {
    let (title, element): (&str, Element) = match role {
        ThemeColor::Header => (
            "Top header",
            rect()
                .padding(14.)
                .background(p.header)
                .child(
                    label()
                        .text("Header · navigation and window controls")
                        .color(p.text),
                )
                .into(),
        ),
        ThemeColor::Sidebar => (
            "Sidebar",
            rect()
                .padding(14.)
                .background(p.sidebar)
                .child(glyph::draw(
                    glyph::of_tab(crate::view::SettingsTab::Appearance),
                    glyph::LIST,
                    p.icons,
                ))
                .child(label().text("Appearance").color(p.text))
                .into(),
        ),
        ThemeColor::Icons | ThemeColor::ActiveIcons => (
            "Navigation icon",
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
                "Window control (preview only)",
                rect()
                    .width(Size::px(crate::HEADER_ROUND))
                    .height(Size::px(crate::HEADER_ROUND))
                    .corner_radius(crate::HEADER_ROUND / 2.)
                    .background((r, g, b))
                    .into(),
            )
        }
        ThemeColor::Portal | ThemeColor::PortalText => (
            "Portal disc and its text",
            rect()
                .width(Size::px(120.))
                .height(Size::px(120.))
                .corner_radius(60.)
                .center()
                .background(p.portal)
                .child(label().text("PORTAL").color(p.portal_text))
                .into(),
        ),
        ThemeColor::Button | ThemeColor::ButtonText => (
            "Primary button",
            rect()
                .padding((12., 20.))
                .corner_radius(10.)
                .background(p.button)
                .child(label().text("Primary action").color(p.button_text))
                .into(),
        ),
        ThemeColor::Warning | ThemeColor::WarningBackground => (
            "Warning notice",
            rect()
                .padding(14.)
                .background(p.warning_background)
                .child(
                    label()
                        .text("Warning: verify received files.")
                        .color(p.warning),
                )
                .into(),
        ),
        ThemeColor::ScannerOutline => (
            "Camera detection marker",
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
                ThemeColor::Send => "Sending →",
                ThemeColor::Receive => "← Receiving",
                ThemeColor::Good => "● Connected · Complete",
                _ => "● Offline · Failed",
            };
            let [r, g, b] = p.color(role);
            (
                "Transfer or connection indicator",
                label().text(text).color((r, g, b)).font_size(18.).into(),
            )
        }
        ThemeColor::Text | ThemeColor::Muted => (
            "Text on the page",
            label()
                .text(if role == ThemeColor::Text {
                    "Primary text — names and headings"
                } else {
                    "Secondary text — descriptions and details"
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
                "Surface and border",
                rect()
                    .padding(18.)
                    .background(if role == ThemeColor::Border {
                        p.surface
                    } else {
                        (r, g, b)
                    })
                    .border(Border::new().fill(p.border).width(2.))
                    .child(label().text("Panel / page content").color(p.text))
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
                .text(format!("PREVIEW — {title}"))
                .color(p.text)
                .font_size(12.),
        )
        .child(element)
        .into()
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
        for text in [
            r#"{"version":2,"name":"X","palette":{}}"#,
            r#"{"version":1,"name":"X","palette":{"colors":{"unknown":[0,0,0]}}}"#,
            r#"{"version":1,"name":"X","palette":{"colors":{"header":[999,0,0]}}}"#,
            r#"{"version":1,"name":"X","palette":{},"settings":{}}"#,
        ] {
            assert!(decode(text.as_bytes()).is_err());
        }
        assert!(decode(&vec![b' '; MAX_THEME_BYTES + 1]).is_err());
        assert!(encode("   ", &CustomTheme::default()).is_err());
    }
}
