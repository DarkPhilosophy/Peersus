//! The overlay that asks a person to read and accept the licences and the notice of an extension before it is installed. It is the
//! application's, drawn the same way for every extension: an extension supplies the words and pictures of its pages and nothing else.
//!
//! What is shown and which buttons there are is [`crate::agreement_flow`], which knows no toolkit and is tried on its own. This file only
//! draws it. `Cancel` is at the far left and gives the package up; the forward button is at the right, and on the last page it is `Accept
//! TOS`, which installs. For a package from outside the store the first thing shown is the application's own warning, which has to be
//! scrolled to its end and then held; the extension cannot change a word of it.
use crate::{
    agreement_flow::{self as rules, Flow, Forward, Step},
    i18n,
    link::Link,
    mono,
    theme::Palette,
};
use freya::prelude::*;
use peersus_api::agreement::{Block, Kind, Page, Tone};
use peersus_core::model::{Command, PluginInfo};

/// Which waiting package the person asked to install, shared from the root so that the install button in the list of extensions only asks and
/// the overlay at the root does the rest. Pressing the button installs nothing: the package installs when its agreement is accepted.
#[derive(Clone, Copy)]
pub struct Ask(pub State<Option<peersus_core::plugins::PluginId>>);

/// The overlay over a package that is waiting to be confirmed. Given by the root while the person has asked to install it.
#[derive(Clone, PartialEq)]
pub struct Overlay {
    pub link: Link,
    /// The package as the owner is holding it: its manifest carries the agreement, and `from_store` says where it came from.
    pub plugin: PluginInfo,
    /// That the person already accepted the application's warning about a package from outside the store: then its step is not part of the flow.
    pub risk_accepted: bool,
    pub palette: Palette,
    /// The size of the window, in pixels: the panel's size is the application's one, made smaller only so that it fits.
    pub window: (f32, f32),
    /// The agreement of an installed extension, read again: no warning, nothing to accept, and `Close` gives nothing up.
    pub review: bool,
    /// Called when the overlay should go away, whether the package was accepted or given up.
    pub on_close: EventHandler<()>,
}

// One concrete `Element` is returned because it leaves early with an empty one.
#[allow(refining_impl_trait)]
impl Component for Overlay {
    fn render(&self) -> Element {
        let palette = self.palette;
        let Some(agreements) = self.plugin.manifest.agreements.clone() else {
            return rect().into();
        };
        let (pages, outside_store, review) = (
            agreements.pages.len(),
            !self.plugin.from_store && !self.risk_accepted,
            self.review,
        );
        let mut flow = use_state(move || {
            if review {
                Flow::review(pages)
            } else {
                Flow::new(pages, outside_store)
            }
        });
        // Whether the text on show was read to its end: written by the body, which measures its own scrolling.
        let mut read_to_end = use_state(|| false);
        // Cancel, by its button or by Back: the package waiting is given up (a review of an installed one only closes).
        let cancel = {
            let (link, plugin, on_close) = (
                self.link.clone(),
                self.plugin.id.clone(),
                self.on_close.clone(),
            );
            EventHandler::new(move |()| {
                if !review {
                    link.send(Command::CancelInstall {
                        plugin: plugin.clone(),
                    });
                }
                on_close.call(());
            })
        };
        crate::back::use_back(true, cancel.clone());
        let Some(current) = *flow.read() else {
            return rect().into();
        };
        let (width, height) = rules::panel_size(self.window);
        let step = current.step();
        let (position, total) = current.position();
        let buttons = current.buttons(*read_to_end.read());
        let name = String::from(self.plugin.id.clone());
        let language = i18n::language();

        let kicker = match step {
            Step::Warning => mono(i18n::tr("OUTSIDE THE STORE"), palette.warning, 11.),
            Step::Page(page) => mono(
                i18n::tr(match agreements.pages[page].kind {
                    Kind::Licence => "LICENCE",
                    Kind::Notice => "NOTICE",
                    Kind::Terms => "TERMS",
                }),
                palette.send,
                11.,
            ),
        };
        let title = match step {
            Step::Warning => i18n::tr("This extension did not come through the store").to_owned(),
            Step::Page(page) => agreements.pages[page].title.clone(),
        };
        let header = rect()
            .width(Size::fill())
            .spacing(4.)
            .padding((18., 20., 12., 20.))
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .child(kicker)
                    .child(rect().width(Size::flex(1.)))
                    .child(mono(
                        i18n::format(
                            language,
                            "Step {number} of {total}",
                            &[
                                ("number", &position.to_string()),
                                ("total", &total.to_string()),
                            ],
                        ),
                        palette.muted,
                        11.,
                    )),
            )
            .child(
                label()
                    .text(title)
                    .color(palette.text)
                    .font_size(20.)
                    .font_weight(FontWeight::BOLD),
            )
            // Whose agreement this is, by the name it will have when installed: what the page says is the extension's, and this is not.
            .child(mono(name.clone(), palette.muted, 11.));

        let body = rect()
            .key(format!("{step:?}"))
            .width(Size::fill())
            .height(Size::flex(1.))
            .child(Body {
                step,
                agreements: agreements.pages.clone(),
                palette,
                read_to_end,
            });

        // Cancel at the far left, always; what goes on at the right is the flow's.
        let mut right = rect().horizontal().spacing(10.);
        if buttons.back {
            right = right.child(
                Button::new()
                    .on_press(move |_| {
                        if let Some(next) = flow.write().as_mut() {
                            next.back();
                        }
                        read_to_end.set_if_modified(false);
                    })
                    .child(i18n::tr("Back")),
            );
        }
        right = match buttons.forward {
            Forward::None => right,
            Forward::Hold { enabled } => right.child(HoldConfirm {
                enabled,
                palette,
                on_done: EventHandler::new(move |()| {
                    let at_end = *read_to_end.peek();
                    if let Some(next) = flow.write().as_mut() {
                        next.confirm_warning(at_end);
                    }
                    read_to_end.set_if_modified(false);
                }),
            }),
            Forward::Next => right.child(
                Button::new()
                    .filled()
                    .on_press(move |_| {
                        if let Some(next) = flow.write().as_mut() {
                            next.advance();
                        }
                        read_to_end.set_if_modified(false);
                    })
                    .child(i18n::tr("Next")),
            ),
            Forward::Accept => {
                let (accept_link, accepted, on_done) = (
                    self.link.clone(),
                    self.plugin.clone(),
                    self.on_close.clone(),
                );
                right.child(
                    Button::new()
                        .filled()
                        .on_press(move |_| {
                            let Some(done) = current.accept() else {
                                return;
                            };
                            // What the owner is told is what was shown: the digest of the agreement as this window holds it, and whether
                            // the warning for a package from outside the store was held.
                            let Some(agreement) = accepted.manifest.agreements.as_ref() else {
                                return;
                            };
                            accept_link.send(Command::ConfirmInstall {
                                plugin: accepted.id.clone(),
                                agreement: Some(peersus_core::agreements::digest(agreement)),
                                risk_acknowledged: done.risk_acknowledged,
                            });
                            // The warning was read and held: that is the person's acceptance of the risk of installing from outside the
                            // store, and it is kept, so that it is not asked for again with the next extension.
                            if done.risk_acknowledged {
                                accept_link.send(Command::PatchSettings {
                                    patch: Box::new(peersus_core::config::SettingsPatch {
                                        outside_store_risk_accepted: Some(true),
                                        ..Default::default()
                                    }),
                                });
                            }
                            on_done.call(());
                        })
                        .child(i18n::tr("Accept TOS")),
                )
            }
        };
        let footer = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .padding((12., 20., 16., 20.))
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(BorderWidth {
                        top: 1.,
                        right: 0.,
                        bottom: 0.,
                        left: 0.,
                    })
                    .alignment(BorderAlignment::Inner),
            )
            .child(
                Button::new()
                    .on_press(move |_| cancel.call(()))
                    .child(i18n::tr(if review { "Close" } else { "Cancel" })),
            )
            .child(rect().width(Size::flex(1.)))
            .child(right);

        let panel = rect()
            .layer(Layer::Overlay)
            .position(
                Position::new_global()
                    .top(((self.window.1 - height) / 2.).max(0.))
                    .left(((self.window.0 - width) / 2.).max(0.)),
            )
            .width(Size::px(width))
            .height(Size::px(height))
            .content(Content::flex())
            .background(palette.raised)
            .corner_radius(16.)
            .overflow(Overflow::Clip)
            .border(
                Border::new()
                    .fill(if step == Step::Warning {
                        palette.warning
                    } else {
                        palette.border
                    })
                    .width(if step == Step::Warning { 2. } else { 1. })
                    .alignment(BorderAlignment::Inner),
            )
            // Nothing reaches what is under the panel through it.
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
            .child(header)
            .child(body)
            .child(footer);

        // The dark layer takes every press and does nothing with it: the only ways out are the two buttons, so that giving up and accepting
        // are both things the person does on purpose.
        rect()
            .layer(Layer::OverlayLevel(9))
            .position(Position::new_global().top(0.).left(0.))
            .width(Size::window_percent(100.))
            .height(Size::window_percent(100.))
            .background((0, 0, 0, 190))
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
            .on_touch_start(|e: Event<TouchEventData>| e.stop_propagation())
            .on_touch_move(|e: Event<TouchEventData>| e.stop_propagation())
            .on_touch_end(|e: Event<TouchEventData>| e.stop_propagation())
            .child(panel)
            .into()
    }
}

/// The scrolling text of one step. It measures what it is and how far it is scrolled, and says whether the whole of it was seen.
#[derive(Clone, PartialEq)]
struct Body {
    step: Step,
    agreements: Vec<Page>,
    palette: Palette,
    read_to_end: State<bool>,
}

impl Component for Body {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let controller = use_scroll_controller(ScrollConfig::default);
        let mut viewport = use_state(|| 0f32);
        let mut content = use_state(|| 0f32);
        let offset = -controller.position().y;
        let mut read_to_end = self.read_to_end;
        read_to_end.set_if_modified(rules::scrolled_to_end(
            offset,
            *viewport.read(),
            *content.read(),
        ));
        let text = match self.step {
            Step::Warning => warning(palette),
            Step::Page(page) => page_text(&self.agreements[page], palette),
        };
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| viewport.set_if_modified(e.area.height()))
            .child(
                ScrollView::new_controlled(controller).expanded().child(
                    rect()
                        .width(Size::fill())
                        .padding((4., 20., 20., 20.))
                        .spacing(14.)
                        .on_sized(move |e: Event<SizedEventData>| {
                            content.set_if_modified(e.area.height());
                        })
                        .child(text),
                ),
            )
    }
}

/// The application's own warning about a package from outside the store. Not the extension's, and not a word of it can be changed by one.
fn warning(palette: Palette) -> Rect {
    let paragraph = |text: &'static str| {
        label()
            .text(i18n::tr(text))
            .color(palette.text)
            .font_size(15.)
    };
    rect()
        .width(Size::fill())
        .spacing(14.)
        .child(paragraph(
            "Nobody at Peersus has reviewed it, signed it or promised that it is safe. It runs with your access on this device and can read and send whatever you let it reach.",
        ))
        .child(paragraph(
            "The Peersus project cannot take on the risk of an extension from outside its store, and is not responsible for what it does to you, your files or your devices. The risk is yours.",
        ))
        .child(paragraph(
            "Prefer the store: what is listed there was reviewed before it was offered. If you do not know who made this package, do not install it.",
        ))
        .child(
            rect()
                .width(Size::fill())
                .padding(14.)
                .corner_radius(10.)
                .background(palette.warning_background)
                .border(
                    Border::new()
                        .fill(palette.warning)
                        .width(1.5)
                        .alignment(BorderAlignment::Inner),
                )
                .child(
                    label()
                        .text(i18n::tr(
                            "Read to the end, then hold the button to confirm that you understand this and accept the risk.",
                        ))
                        .color(palette.text)
                        .font_size(15.)
                        .font_weight(FontWeight::BOLD),
                ),
        )
}

/// A page of the agreement: what it is and what changes first, in the extension's words, then its links, then its own content.
fn page_text(page: &Page, palette: Palette) -> Rect {
    let section = |heading: &'static str, text: &str| {
        rect()
            .width(Size::fill())
            .spacing(4.)
            .child(mono(i18n::tr(heading), palette.muted, 11.))
            .child(
                label()
                    .text(text.to_owned())
                    .color(palette.text)
                    .font_size(15.),
            )
    };
    let mut column = rect()
        .width(Size::fill())
        .spacing(14.)
        .child(section("WHAT THIS IS", &page.explains))
        .child(section("WHAT CHANGES FOR YOU", &page.changes));
    if let Some(spdx) = &page.spdx {
        column = column.child(mono(spdx.clone(), palette.send, 12.));
    }
    if !page.links.is_empty() {
        let mut links = rect().width(Size::fill()).spacing(8.).child(mono(
            i18n::tr("READ THE FULL TEXT"),
            palette.muted,
            11.,
        ));
        for link in &page.links {
            links = links.child(address(&link.label, &link.url, palette));
        }
        column = column.child(links);
    }
    for block in &page.body {
        column = column.child(block_element(block, palette));
    }
    column
}

/// A link to read: its label, and under it the whole address, so that where it leads is never hidden behind a word.
fn address(text: &str, url: &str, palette: Palette) -> Element {
    let target = url.to_owned();
    rect()
        .width(Size::fill())
        .spacing(2.)
        .on_press(move |_| Platform::get().open_url(target.clone()))
        .child(
            label()
                .text(text.to_owned())
                .color(palette.receive)
                .font_size(15.),
        )
        .child(mono(url.to_owned(), palette.muted, 11.))
        .into()
}

/// One block of a page. Plain content: it has no size and no position of its own, and nothing in it can be pressed but a link.
fn block_element(block: &Block, palette: Palette) -> Element {
    match block {
        Block::Heading { text } => label()
            .text(text.clone())
            .color(palette.text)
            .font_size(17.)
            .font_weight(FontWeight::BOLD)
            .into(),
        Block::Paragraph { text, muted } => label()
            .text(text.clone())
            .color(if *muted { palette.muted } else { palette.text })
            .font_size(15.)
            .into(),
        Block::List { items, ordered } => {
            let mut list = rect().width(Size::fill()).spacing(6.);
            for (index, item) in items.iter().enumerate() {
                let bullet = if *ordered {
                    format!("{}.", index + 1)
                } else {
                    "•".to_owned()
                };
                list = list.child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .spacing(8.)
                        .content(Content::flex())
                        .child(label().text(bullet).color(palette.muted).font_size(15.))
                        .child(
                            rect().width(Size::flex(1.)).child(
                                label()
                                    .text(item.clone())
                                    .color(palette.text)
                                    .font_size(15.),
                            ),
                        ),
                );
            }
            list.into()
        }
        Block::Callout { tone, text } => {
            let (edge, ground) = match tone {
                Tone::Info => (palette.send, palette.surface),
                Tone::Warning => (palette.warning, palette.warning_background),
                Tone::Danger => (palette.bad, palette.surface),
            };
            rect()
                .width(Size::fill())
                .padding(12.)
                .corner_radius(10.)
                .background(ground)
                .border(
                    Border::new()
                        .fill(edge)
                        .width(1.5)
                        .alignment(BorderAlignment::Inner),
                )
                .child(
                    label()
                        .text(text.clone())
                        .color(palette.text)
                        .font_size(15.),
                )
                .into()
        }
        Block::Table { header, rows } => {
            let row = |cells: &[String], head: bool| {
                let mut line = rect()
                    .width(Size::fill())
                    .horizontal()
                    .spacing(10.)
                    .content(Content::flex());
                for cell in cells {
                    line = line.child(
                        rect().width(Size::flex(1.)).child(
                            label()
                                .text(cell.clone())
                                .color(if head { palette.muted } else { palette.text })
                                .font_size(if head { 12. } else { 14. })
                                .font_weight(if head {
                                    FontWeight::BOLD
                                } else {
                                    FontWeight::NORMAL
                                }),
                        ),
                    );
                }
                line
            };
            let mut table = rect()
                .width(Size::fill())
                .spacing(6.)
                .child(row(header, true));
            for cells in rows {
                table = table.child(row(cells, false));
            }
            table.into()
        }
        Block::Link { label, url } => address(label, url, palette),
        Block::Divider => rect()
            .width(Size::fill())
            .height(Size::px(1.))
            .background(palette.border)
            .into(),
    }
}

/// The button of the warning. It does nothing when pressed: it is HELD, and it fills while it is held, and confirms when it is full. A touch
/// that is let go early, or that strays, starts again from nothing; and until the text was read to its end it cannot be held at all.
#[derive(Clone, PartialEq)]
struct HoldConfirm {
    enabled: bool,
    palette: Palette,
    on_done: EventHandler<()>,
}

impl Component for HoldConfirm {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut fill = use_state(|| 0f32);
        // The moment the finger or button went down, on the clock that only goes forward; none when nothing is held.
        let mut down: State<Option<u64>> = use_state(|| None);
        // Which hold the clock was started for, so that a clock left over from one that ended never confirms the next.
        let mut hold_number = use_state(|| 0u64);
        let mut tap = use_state(crate::view::Tap::default);
        let enabled = self.enabled;
        let done = self.on_done.clone();
        let width = 190.;
        let share = *fill.read();
        let words = if !enabled {
            i18n::tr("Read to the end to go on")
        } else if share > 0. {
            i18n::tr("Keep holding")
        } else {
            i18n::tr("Hold to confirm")
        };
        let filled = rect()
            .position(Position::new_absolute().top(0.).left(0.))
            .width(Size::px(width * share))
            .height(Size::fill())
            .background(
                Color::from_rgb(palette.warning.0, palette.warning.1, palette.warning.2)
                    .with_a(200),
            )
            .interactive(false);
        rect()
            .width(Size::px(width))
            .height(Size::px(40.))
            .center()
            .corner_radius(10.)
            .overflow(Overflow::Clip)
            .background(if enabled {
                palette.surface
            } else {
                palette.background
            })
            .opacity(if enabled { 1. } else { 0.55 })
            .border(
                Border::new()
                    .fill(if enabled {
                        palette.warning
                    } else {
                        palette.border
                    })
                    .width(1.5)
                    .alignment(BorderAlignment::Inner),
            )
            .on_pointer_down(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                if !enabled {
                    return;
                }
                let at = e.global_location();
                tap.write().down((at.x as f32, at.y as f32));
                let number = *hold_number.peek() + 1;
                hold_number.set(number);
                let began = crate::monotonic_ms();
                down.set(Some(began));
                fill.set(0.);
                let mut clock =
                    crate::link::clock(crate::monotonic_ms, std::time::Duration::from_millis(33));
                let done = done.clone();
                spawn(async move {
                    while clock.changed().await.is_ok() {
                        // A clock belongs to the hold it was started for, and to a finger that is still down and has not strayed.
                        if *hold_number.peek() != number
                            || down.peek().is_none()
                            || *tap.peek() == crate::view::Tap::default()
                        {
                            fill.set_if_modified(0.);
                            break;
                        }
                        let held = crate::monotonic_ms().saturating_sub(began);
                        fill.set_if_modified(rules::hold_fill(held));
                        if rules::hold_confirms(held) {
                            down.set(None);
                            fill.set_if_modified(0.);
                            done.call(());
                            break;
                        }
                    }
                });
            })
            .on_capture_global_pointer_move(move |e: Event<PointerEventData>| {
                let at = e.global_location();
                tap.write().moved((at.x as f32, at.y as f32));
            })
            .on_global_pointer_up(move |_| {
                // Let go before it was full: nothing happened, and it starts again from nothing.
                down.set(None);
                tap.write().cancel();
                fill.set_if_modified(0.);
            })
            .on_touch_cancel(move |_| {
                down.set(None);
                tap.write().cancel();
                fill.set_if_modified(0.);
            })
            .child(filled)
            .child(
                label()
                    .text(words)
                    .color(palette.text)
                    .font_size(14.)
                    .font_weight(FontWeight::BOLD),
            )
    }
}
