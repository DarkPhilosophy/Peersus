//! The Languages page of Settings: the language the windows are shown in, whether it follows the device's, how far each
//! language is translated, and the ways to bring a language or give one back: download it (choosing it is enough), import a
//! file, save the template a translation starts from, and share a translation. Which languages there are and how far along
//! each is, is `view`'s ([`view::language_choices`]); the files and the fetching are the owner's.

use crate::{
    i18n,
    link::Link,
    text,
    theme::Palette,
    view::{self, LanguageChoice},
};
use freya::prelude::*;
use peersus_core::{
    config::{Language, SettingsPatch},
    model::{Command, LanguageState, LanguageStatus},
};

/// The page's buttons, by the words on them; Settings search finds them by the same words.
pub const CHECK_AGAIN: &str = "Check again";
pub const IMPORT: &str = "Import a language file…";
pub const SAVE_TEMPLATE: &str = "Save the template…";
pub const SHARE: &str = "Share a translation (opens browser)";

/// The buttons this platform shows: a phone imports through the system's picker and has no template to save.
pub fn action_labels() -> &'static [&'static str] {
    if cfg!(target_os = "android") {
        &[CHECK_AGAIN, IMPORT, SHARE]
    } else {
        &[CHECK_AGAIN, IMPORT, SAVE_TEMPLATE, SHARE]
    }
}

/// The name a language gives itself, else its tag.
fn name_of(choices: &[LanguageChoice], language: Language) -> String {
    view::choice_of(choices, language)
        .map_or_else(|| language.tag().to_owned(), |choice| choice.name.clone())
}

/// What following the device's language means here: the device's language by its name, or English when it names none.
pub fn follow_words(
    language: Language,
    state: &LanguageState,
    choices: &[LanguageChoice],
) -> String {
    match state.device {
        Some(device) => i18n::format(
            language,
            "This device is set to {language}.",
            &[("language", &name_of(choices, device))],
        ),
        None => i18n::translate(
            language,
            "This device names no language, so English is used.",
        )
        .to_owned(),
    }
}

/// How far a language is translated, whether this device has its file, and how large that file is, as its row says: what
/// the Extensions page says of a package (installed or not), so that a person knows what choosing it downloads.
pub fn about_words(language: Language, choice: &LanguageChoice) -> String {
    let mut about = i18n::format(
        language,
        "{percent}% translated",
        &[("percent", &choice.percent.to_string())],
    );
    let mut then = |more: &str| {
        about = i18n::format(
            language,
            "{first} · {second}",
            &[("first", &about), ("second", more)],
        );
    };
    if choice.language.is_english() {
        return about;
    }
    then(i18n::translate(
        language,
        if choice.imported {
            "Imported"
        } else if choice.downloaded {
            "Downloaded"
        } else {
            "Not downloaded"
        },
    ));
    if choice.machine {
        then(i18n::translate(language, "AI translated"));
    }
    if choice.bytes > 0 {
        then(&view::size_in_words(choice.bytes));
    }
    about
}

/// What became of something done on the page.
#[derive(Clone, Debug, PartialEq)]
enum Note {
    // On Android the system picker imports through the platform and says so itself, and there is no template to save.
    #[cfg_attr(
        target_os = "android",
        expect(dead_code, reason = "a desktop file dialog only")
    )]
    Unread,
    #[cfg_attr(
        target_os = "android",
        expect(dead_code, reason = "a desktop file dialog only")
    )]
    Imported,
    #[cfg_attr(
        target_os = "android",
        expect(dead_code, reason = "a desktop file dialog only")
    )]
    Saved,
    #[cfg_attr(
        target_os = "android",
        expect(dead_code, reason = "a desktop file dialog only")
    )]
    NotSaved,
    NoBrowser,
    /// The owner's own sentence for why it would not.
    Owner(String),
}

impl Note {
    fn words(&self) -> String {
        match self {
            Note::Unread => i18n::tr("The language file could not be read.").to_owned(),
            Note::Imported => i18n::tr("The language file was imported.").to_owned(),
            Note::Saved => i18n::tr("The template was saved.").to_owned(),
            Note::NotSaved => i18n::tr("The template could not be saved.").to_owned(),
            Note::NoBrowser => i18n::tr("Could not open a browser.").to_owned(),
            Note::Owner(reason) => i18n::tr(reason).to_owned(),
        }
    }
}

#[derive(PartialEq)]
pub struct LanguagesPage {
    pub link: Link,
    pub palette: Palette,
    pub state: LanguageState,
    /// The language the person chose; none while the windows follow the device's.
    pub chosen: Option<Language>,
}

impl Component for LanguagesPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // The owner's or the platform's last answer to something done on this page.
        let note = use_state(|| None::<Note>);
        let choices = view::language_choices(&self.state, i18n::messages());
        let wanted = view::wanted_language(self.chosen, &self.state);
        let selected = view::choice_of(&choices, wanted).map(|choice| choice.language);
        let link = self.link.clone();
        let patch = move |language: Option<Option<Language>>| {
            link.send(Command::PatchSettings {
                patch: Box::new(SettingsPatch {
                    language,
                    ..Default::default()
                }),
            });
        };
        let follow_words = follow_words(i18n::language(), &self.state, &choices);
        let mut page = rect()
            .width(Size::fill())
            .spacing(14.)
            .child(crate::mono(i18n::tr("LANGUAGE"), palette.muted, 12.))
            .child(
                label()
                    .text(i18n::tr("English is built in. Any other language is a file downloaded for this version; whatever it does not translate is shown in English."))
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(10.)
                    .cross_align(Alignment::center())
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(4.)
                            .child(
                                label()
                                    .text(i18n::tr("Use the device's language"))
                                    .color(palette.text)
                                    .font_size(15.),
                            )
                            .child(label().text(follow_words).color(palette.muted).font_size(13.)),
                    )
                    .child(Switch::new(WritableSetter::new(self.chosen.is_none(), {
                        let patch = patch.clone();
                        move |on: bool| patch(Some(if on { None } else { Some(wanted) }))
                    }))),
            );
        let status = match self.state.status {
            LanguageStatus::Fetching => {
                Some((i18n::tr("Looking for languages…").to_owned(), palette.muted))
            }
            LanguageStatus::Failed => Some((
                self.state
                    .error
                    .as_deref()
                    .map_or_else(|| i18n::tr("The languages could not be fetched"), i18n::tr)
                    .to_owned(),
                palette.bad,
            )),
            LanguageStatus::Idle if self.state.offered.is_empty() => Some((
                i18n::tr("No other language is offered for this version yet.").to_owned(),
                palette.muted,
            )),
            LanguageStatus::Idle => None,
        };
        if let Some((words, colour)) = status {
            page = page.child(label().text(words).color(colour).font_size(13.));
        }
        let mut list = rect().width(Size::fill()).spacing(10.);
        for choice in choices.iter().cloned() {
            list = list.child(row(
                &self.link,
                &choice,
                selected == Some(choice.language),
                palette,
                note,
            ));
        }
        page = page.child(list);
        if let Some(words) = note.read().clone() {
            page = page.child(
                label()
                    .text(words.words())
                    .color(palette.muted)
                    .font_size(13.),
            );
        }
        page.child(crate::mono(i18n::tr("TRANSLATE"), palette.muted, 12.))
            .child(
                label()
                    .text(i18n::tr("Save the template, translate it in any PO editor, import it here to see it at once, then share it so it can be offered to everyone."))
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(actions(&self.link, note))
    }
}

/// One language: its name, how far it is translated, where its file is, and what can be done with it.
fn row(
    link: &Link,
    choice: &LanguageChoice,
    selected: bool,
    palette: Palette,
    mut note: State<Option<Note>>,
) -> Element {
    let about = about_words(i18n::language(), choice);
    let mut buttons = rect()
        .horizontal()
        .spacing(8.)
        .cross_align(Alignment::center());
    if selected {
        buttons = buttons.child(crate::mono(i18n::tr("IN USE"), palette.send, 12.));
    } else {
        let link = link.clone();
        let chosen = choice.language;
        // As a package in the store: one that is not here is downloaded by choosing it, and the button says so.
        let here = choice.downloaded || choice.imported || choice.language.is_english();
        let mut button = Button::new();
        if !here {
            button = button.filled();
        }
        buttons = buttons.child(
            button
                .on_press(move |_| {
                    note.set(None);
                    link.send(Command::PatchSettings {
                        patch: Box::new(SettingsPatch {
                            language: Some(Some(chosen)),
                            ..Default::default()
                        }),
                    });
                })
                .child(text::caption(
                    i18n::tr(if here { "Use" } else { "Download and use" }).to_owned(),
                )),
        );
    }
    if choice.downloaded || choice.imported {
        let client = link.client().clone();
        let (removed, imported) = (choice.language, choice.imported);
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    let client = client.clone();
                    spawn(async move {
                        let answer = client
                            .command(Command::LanguageRemove {
                                language: removed,
                                imported,
                            })
                            .await;
                        note.set(answer.err().map(|reason| Note::Owner(reason.to_owned())));
                    });
                })
                .child(text::caption(i18n::tr("Remove").to_owned())),
        );
    }
    // The words take the whole width and the buttons go under them: on a phone a translated "% translated · imported" beside
    // two buttons would break inside a word.
    rect()
        .width(Size::fill())
        .padding(14.)
        .spacing(8.)
        .corner_radius(12.)
        .background(palette.surface)
        .border(
            Border::new()
                .fill(if selected {
                    palette.send
                } else {
                    palette.border
                })
                .width(1.)
                .alignment(BorderAlignment::Inner),
        )
        // The name a language gives itself comes from its file, of any length.
        .child(
            label()
                .text(choice.name.clone())
                .color(palette.text)
                .font_size(15.)
                .width(Size::fill())
                .max_lines(1)
                .text_overflow(TextOverflow::Ellipsis),
        )
        .child(label().text(about).color(palette.muted).font_size(13.))
        .child(buttons)
        .into()
}

/// Check again, import a file and save the template (where there is a file chooser), and share a translation.
fn actions(link: &Link, mut note: State<Option<Note>>) -> Element {
    let mut buttons = rect()
        .width(Size::fill())
        .horizontal()
        .content(Content::wrap_spacing(8.))
        .spacing(8.);
    {
        let link = link.clone();
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    note.set(None);
                    link.send(Command::LanguagesRefresh {});
                })
                .child(text::caption(i18n::tr(CHECK_AGAIN).to_owned())),
        );
    }
    // Where a picked file has no path (Android), the platform's picker reads it and imports it, and says itself what went wrong.
    if let Some(picker) = try_consume_context::<crate::link::LanguagePicker>() {
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    note.set(
                        (picker.0)()
                            .err()
                            .map(|reason| Note::Owner(reason.to_owned())),
                    );
                })
                .child(text::caption(i18n::tr(IMPORT).to_owned())),
        );
    }
    #[cfg(not(target_os = "android"))]
    {
        let importing = link.clone();
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    let link = importing.clone();
                    spawn(async move {
                        let Some(file) = rfd::AsyncFileDialog::new()
                            .add_filter(i18n::tr("Language file"), &["po"])
                            .pick_file()
                            .await
                        else {
                            return;
                        };
                        let path = file.path().to_owned();
                        let read = link
                            .blocking(move || {
                                use std::io::Read;
                                let mut text = String::new();
                                std::fs::File::open(path)
                                    .ok()?
                                    .take(peersus_api::language::MAX_FILE_BYTES as u64 + 1)
                                    .read_to_string(&mut text)
                                    .ok()?;
                                Some(text)
                            })
                            .await;
                        let Ok(Some(text)) = read else {
                            note.set(Some(Note::Unread));
                            return;
                        };
                        let answer = link
                            .client()
                            .command(Command::LanguageImport { text })
                            .await;
                        note.set(Some(match answer {
                            Ok(_) => Note::Imported,
                            Err(reason) => Note::Owner(reason.to_owned()),
                        }));
                    });
                })
                .child(text::caption(i18n::tr(IMPORT).to_owned())),
        );
        let link = link.clone();
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    let link = link.clone();
                    spawn(async move {
                        let Some(file) = rfd::AsyncFileDialog::new()
                            .set_file_name("messages.pot")
                            .add_filter(i18n::tr("Language template"), &["pot"])
                            .save_file()
                            .await
                        else {
                            return;
                        };
                        let path = file.path().to_owned();
                        let saved = link
                            .blocking(move || std::fs::write(path, i18n::TEMPLATE_TEXT))
                            .await;
                        note.set(Some(match saved {
                            Ok(Ok(())) => Note::Saved,
                            _ => Note::NotSaved,
                        }));
                    });
                })
                .child(text::caption(i18n::tr(SAVE_TEMPLATE).to_owned())),
        );
    }
    let opener = try_consume_context::<crate::link::UrlOpener>();
    buttons
        .child(
            Button::new()
                .on_press(move |_| {
                    let url = peersus_app::languages::SHARE_URL;
                    let opened = match &opener {
                        Some(opener) => opener.open(url),
                        #[cfg(not(target_os = "android"))]
                        None => {
                            Platform::get().open_url(url);
                            true
                        }
                        #[cfg(target_os = "android")]
                        None => false,
                    };
                    note.set((!opened).then_some(Note::NoBrowser));
                })
                .child(text::caption(i18n::tr(SHARE).to_owned())),
        )
        .into()
}

/// What the Languages page gives Settings search: following the device, each language as its row says, and the buttons.
pub fn search(index: &mut crate::settings_index::Index, view: &view::View) {
    let language = index.language();
    let choices = view::language_choices(&view.languages, i18n::messages());
    index.add(
        "Use the device's language",
        follow_words(language, &view.languages, &choices),
    );
    for choice in &choices {
        index.add(&choice.name, about_words(language, choice));
    }
    let actions = action_labels()
        .iter()
        .map(|label| index.tr(label))
        .collect::<Vec<_>>()
        .join(" · ");
    index.add("TRANSLATE", actions);
}
