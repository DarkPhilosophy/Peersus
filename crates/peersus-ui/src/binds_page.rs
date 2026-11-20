//! The Binds page of Settings (desktop only): every action a key can run, with its title, its description and its bindings. The
//! bindings are the buttons: each chord is a button that records a new chord in its place when pressed (Backspace removes it,
//! Escape leaves it), and `+` records one more, up to three. What the rows are, and whether a chord is free, is `view`'s; this
//! draws them and reads the recorded keys.

use crate::{
    chords, i18n,
    link::Link,
    text,
    theme::Palette,
    view::{self, BindRefusal, BindRow},
};
use freya::prelude::*;
use peersus_api::chord::Chord;
use peersus_core::{bindings::Bindings, model::Command};

/// Whether the Binds page is recording keys now, provided by the window's root, which runs no binding meanwhile: the chord being
/// recorded must not also run what it ran before.
#[derive(Clone, Copy)]
pub struct Recording(pub State<bool>);

/// What is being recorded: the row, and the chord it replaces (none for `+`).
#[derive(Clone, Copy, PartialEq)]
struct Slot {
    row: usize,
    replaces: Option<Chord>,
}

#[derive(PartialEq)]
pub struct BindsPage {
    pub link: Link,
    pub palette: Palette,
    pub rows: Vec<BindRow>,
    pub bindings: Bindings,
    /// Whether the person has the bindings work while the window is not in front too (Linux only).
    pub outside: bool,
}

impl Component for BindsPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // The binding being recorded, and the words of the last refusal.
        let mut recording = use_state(|| None::<Slot>);
        let mut refused = use_state(|| None::<String>);
        // What is held so far while recording (`ctrl+alt+…`), shown on the button live until a key completes the chord.
        let mut live = use_state(|| None::<String>);
        let shared = try_consume_context::<Recording>();
        use_side_effect(move || {
            let on = recording.read().is_some();
            if let Some(Recording(mut flag)) = shared {
                flag.set_if_modified(on);
            }
        });
        use_drop(move || {
            if let Some(Recording(mut flag)) = shared {
                flag.set_if_modified(false);
            }
        });
        let rows = self.rows.clone();
        let bindings = self.bindings.clone();
        let link = self.link.clone();
        let record = {
            let rows = rows.clone();
            move |e: Event<KeyboardEventData>| {
                let Some(slot) = *recording.peek() else {
                    return;
                };
                let Some(row) = rows.get(slot.row) else {
                    recording.set(None);
                    return;
                };
                let mut stop = |next: Option<Bindings>| {
                    live.set(None);
                    recording.set(None);
                    if let Some(next) = next {
                        refused.set(None);
                        link.send(Command::SetBindings { bindings: next });
                    }
                };
                if e.modifiers.is_empty() {
                    match e.code {
                        // Escape alone leaves the binding as it was.
                        Code::Escape => return stop(None),
                        // Backspace or Delete alone removes the binding being changed.
                        Code::Backspace | Code::Delete => {
                            return stop(
                                slot.replaces.map(|chord| view::unbound(&bindings, chord)),
                            );
                        }
                        _ => {}
                    }
                }
                // A modifier on its own, or typing, is not yet a chord: recording goes on, showing what is held.
                let Some(chord) = chords::of(&e.key, e.code, e.modifiers) else {
                    live.set_if_modified(chords::held_words(&e.key, e.modifiers, true));
                    return;
                };
                if slot.replaces == Some(chord) {
                    return stop(None);
                }
                let without = slot
                    .replaces
                    .map_or_else(|| bindings.clone(), |old| view::unbound(&bindings, old));
                match view::rebound(&without, &rows, chord, &row.target) {
                    Ok(next) => stop(Some(next)),
                    Err(refusal) => {
                        stop(None);
                        refused.set(Some(refusal_words(chord, row, &refusal)));
                    }
                }
            }
        };
        let mut page = rect()
            .width(Size::fill())
            .spacing(14.)
            .on_global_key_down(record)
            .on_global_key_up(move |e: Event<KeyboardEventData>| {
                if recording.peek().is_some() {
                    live.set_if_modified(chords::held_words(&e.key, e.modifiers, false));
                }
            })
            .child(crate::mono(i18n::tr("KEY BINDINGS"), palette.muted, 12.))
            .child(
                label()
                    .text(i18n::tr("A key runs what its row says while the window is in front. Press + and then the keys together to add one, up to three; press a binding to change it, then Backspace to remove it or Escape to leave it."))
                    .color(palette.muted)
                    .font_size(13.),
            );
        // Where the desktop has a portal for it (Linux): the bindings asked of the desktop, to work while the window is behind others.
        if cfg!(target_os = "linux") {
            let link = self.link.clone();
            page = page.child(
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
                                    .text(i18n::tr("Also while the window is not in front"))
                                    .color(palette.text)
                                    .font_size(15.),
                            )
                            .child(
                                label()
                                    .text(i18n::tr("The desktop asks you to approve the keys, and may give another key where one is taken."))
                                    .color(palette.muted)
                                    .font_size(13.),
                            ),
                    )
                    .child(Switch::new(WritableSetter::new(self.outside, move |on: bool| {
                        link.send(Command::PatchSettings {
                            patch: Box::new(peersus_core::config::SettingsPatch {
                                bindings_outside: Some(on),
                                ..Default::default()
                            }),
                        });
                    }))),
            );
        }
        if let Some(words) = refused.read().clone() {
            page = page.child(label().text(words).color(palette.bad).font_size(13.));
        }
        let now = *recording.read();
        let live_words = live.read().clone();
        let mut list = rect().width(Size::fill()).spacing(10.);
        for (index, row) in rows.iter().enumerate() {
            // The bindings themselves, then `+` while there is room: one row of buttons and nothing else.
            let mut slots: Vec<Slot> = row
                .chords
                .iter()
                .map(|chord| Slot {
                    row: index,
                    replaces: Some(*chord),
                })
                .collect();
            if !row.full() {
                slots.push(Slot {
                    row: index,
                    replaces: None,
                });
            }
            // The bindings, then the add button inline after them: once a key is recorded it is a binding in its place and a new add
            // button follows it. The add button is drawn as one (outlined, in the accent) so it is never taken for a binding.
            let mut buttons = rect()
                .horizontal()
                .spacing(8.)
                .cross_align(Alignment::center());
            for slot in slots {
                let this = now == Some(slot);
                let label = if this {
                    let held = live_words
                        .clone()
                        .unwrap_or_else(|| i18n::tr("press the keys…").to_owned());
                    // Changing a binding: what else the keys do here is said on the button itself.
                    if slot.replaces.is_some() {
                        i18n::format(
                            i18n::language(),
                            "{held} · Backspace removes",
                            &[("held", &held)],
                        )
                    } else {
                        held
                    }
                } else {
                    slot.replaces
                        .map_or_else(|| "+".to_owned(), |chord| chord.to_string())
                };
                let words = if slot.replaces.is_none() && !this {
                    crate::mono(label, palette.send, 15.)
                } else {
                    text::caption(label)
                };
                let mut button = Button::new()
                    .on_press(move |_| {
                        refused.set(None);
                        live.set(None);
                        recording.set(if this { None } else { Some(slot) });
                    })
                    .child(words);
                if this {
                    button = button.filled();
                } else if slot.replaces.is_none() {
                    button = button.outline().compact();
                }
                buttons = buttons.child(button);
            }
            list = list.child(
                rect()
                    .width(Size::fill())
                    .padding(14.)
                    .spacing(6.)
                    .corner_radius(12.)
                    .background(palette.surface)
                    .border(
                        Border::new()
                            .fill(palette.border)
                            .width(1.)
                            .alignment(BorderAlignment::Inner),
                    )
                    // A quick glance first: what it is. Then what it does exactly, then its keys.
                    .child(
                        label()
                            .text(row_words(row))
                            .color(palette.text)
                            .font_size(15.),
                    )
                    .maybe_child(
                        row.help
                            .as_deref()
                            .map(|help| help_text(row, help, palette)),
                    )
                    .child(
                        rect()
                            .width(Size::fill())
                            .margin((4., 0., 0., 0.))
                            .child(buttons),
                    ),
            );
        }
        page.child(list)
    }
}

/// A row's description: optional, at most `MAX_HELP` characters (refused longer in the manifest), and a long one scrolls inside a
/// box of three lines instead of making its row tall.
fn help_text(row: &BindRow, help: &str, palette: Palette) -> Element {
    /// Past this many characters a description may not fit in three lines of the narrowest Settings.
    const SHORT: usize = 140;
    let words = label()
        .text(match row.from {
            None => i18n::tr(help).to_owned(),
            Some(_) => help.to_owned(),
        })
        .color(palette.muted)
        .font_size(13.);
    if help.chars().count() <= SHORT {
        return words.into();
    }
    ScrollView::new()
        .width(Size::fill())
        .height(Size::px(54.))
        .auto_hide_scrollbars(false)
        .child(words)
        .into()
}

/// Why a recorded chord was not added, in words.
fn refusal_words(chord: Chord, row: &BindRow, refusal: &BindRefusal) -> String {
    match refusal {
        BindRefusal::Taken(held) => i18n::format(
            i18n::language(),
            "{chord} already runs {what}.",
            &[("chord", &chord.to_string()), ("what", &row_words(held))],
        ),
        BindRefusal::Full => i18n::format(
            i18n::language(),
            "{what} has three key bindings already.",
            &[("what", &row_words(row))],
        ),
    }
}

/// What a row runs, in words: the application's own in the person's language; an extension's action as it wrote it, with the
/// extension's name and the device it is for.
pub(crate) fn row_words(row: &BindRow) -> String {
    let what = match &row.from {
        None => i18n::tr(&row.label).to_owned(),
        Some(from) => format!("{from}: {}", row.label),
    };
    match &row.device {
        Some(device) => format!("{what} ({device})"),
        None => what,
    }
}

/// What the Binds page gives Settings search (desktop only, where the page is): each row by what it does, with its words, its
/// extension, its device and its keys.
pub fn search(index: &mut crate::settings_index::Index, view: &view::View) {
    if cfg!(target_os = "android") {
        return;
    }
    for row in view::bind_rows(view) {
        let detail = row
            .help
            .as_deref()
            .map(|help| index.tr(help))
            .into_iter()
            .chain(row.from.clone())
            .chain(row.device.clone())
            .chain(row.chords.iter().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join(" · ");
        index.add(&row.label, detail);
    }
}
