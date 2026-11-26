//! What the application has to say, floating over the page: the owner's sentences and the notifications on show.
use super::*;

/// How long a message stays before it goes by itself.
pub(crate) const NOTICE_MS: u64 = 7_000;
/// Most messages on show at once; older ones make room.
pub(crate) const MOST_NOTICES: usize = 3;

/// What the application has to say, floating over the page and never moving it: the owner's sentences, each of which appears,
/// stays a few seconds and goes, or is dismissed; and after them the notifications on show (`posted`), each a card with the
/// buttons its sender named, which stays for as long as the owner keeps it on show. A message of a plugin (`[id] sentence`) says
/// which extension it was, by the name it is installed under, so the person knows what failed without opening anything.
///
/// What is remembered is WHICH texts were already shown: a message that is still in the owner's list is not shown
/// again, and a different one that arrives later is new and is seen. The owner also counts every time it says
/// something (`said_count`), even a sentence its list already ends with: a failure that happens again after its message
/// went away is shown again, instead of hiding behind the copy the list still holds.
#[derive(PartialEq)]
pub(crate) struct Notices {
    pub(crate) link: Link,
    /// The notifications on show, as the view chose and named them.
    pub(crate) posted: Vec<view::NotificationCard>,
    /// The pairing requests that wait: a decision about who may connect, with a clock on it, so it is shown over whatever
    /// screen the person is on. A card of this stack like any question, and never a band that pushes the page down.
    pub(crate) approvals: Vec<Approval>,
    pub(crate) said: Vec<String>,
    /// How many of `said`, from the start, are the owner's own list, which `said_count` counts.
    pub(crate) owned: usize,
    pub(crate) said_count: u64,
    pub(crate) palette: Palette,
    /// Top and bottom room occupied by the window bar.
    pub(crate) clearance: (f32, f32),
    /// Wide desktop's measured page slot, which excludes the incoming offer.
    pub(crate) slot_height: Option<State<f32>>,
    /// Plugin id and the name it is shown by.
    pub(crate) plugins: Vec<(String, String)>,
}

impl Component for Notices {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // The owner's messages that are already in its list when this is first drawn are old news (see `notices_already_said`).
        let seen: State<(Vec<String>, u64)> = use_state(|| {
            (
                view::notices_already_said(&self.said, self.owned),
                self.said_count,
            )
        });
        let mut showing: State<Vec<(String, u64)>> = use_state(Vec::new);
        // Nothing is written while this is drawn: what arrived is taken in after the render, when the owner's list changes,
        // and what has had its time is dropped by the clock's task, never by the render that reads it.
        let owned = self.owned;
        use_side_effect_with_deps(
            &(self.said.clone(), self.said_count),
            move |(said, count): &(Vec<String>, u64)| {
                let mut seen = seen;
                let now = crate::monotonic_ms();
                let (before, before_count) = seen.peek().clone();
                let fresh = view::fresh_notices(&before, before_count, said, owned, *count);
                seen.set((said.clone(), *count));
                if !fresh.is_empty() {
                    let mut next = showing.peek().clone();
                    for text in fresh {
                        // The same sentence again (a burst of failures says the same thing many times) keeps the one
                        // already shown on screen longer instead of stacking copies of it.
                        match next.iter_mut().find(|(shown, _)| *shown == text) {
                            Some((_, at)) => *at = now,
                            None => next.push((text, now)),
                        }
                    }
                    let extra = next.len().saturating_sub(MOST_NOTICES);
                    next.drain(..extra);
                    showing.set(next);
                }
            },
        );
        use_hook(move || {
            let mut ticks =
                crate::link::clock(crate::monotonic_ms, std::time::Duration::from_millis(500));
            spawn(async move {
                loop {
                    let now = *ticks.borrow_and_update();
                    let current = showing.peek().clone();
                    let kept: Vec<(String, u64)> = current
                        .iter()
                        .filter(|(_, at)| now.saturating_sub(*at) < NOTICE_MS)
                        .cloned()
                        .collect();
                    if kept.len() != current.len() {
                        showing.set(kept);
                    }
                    if ticks.changed().await.is_err() {
                        return;
                    }
                }
            });
        });
        let live: Vec<(String, u64)> = showing.read().clone();
        // With nothing to say, nothing at all: the row below spans the window and would lie over the page.
        if live.is_empty() && self.posted.is_empty() && self.approvals.is_empty() {
            return rect();
        }
        // As wide as the window allows, up to 380 px, against its right edge: a fixed 380 px was wider than a phone's
        // screen and went off its left side, title and start of the sentence with it. On a desktop the messages sit at
        // the bottom right, where desktops put theirs; on a phone at the top, clear of the keyboard and the input row.
        let (top, bottom) = self.clearance;
        const EDGE: f32 = 12.;
        const WIDTH: f32 = 380.;
        let max_height = self
            .slot_height
            .map(|height| height() - top - bottom - 2. * EDGE);
        if max_height.is_some_and(|height| height <= 0.) {
            return rect();
        }
        #[cfg(target_os = "android")]
        let corner = {
            let _ = bottom;
            Position::new_absolute().top(top).left(0.)
        };
        #[cfg(not(target_os = "android"))]
        let corner = {
            let _ = top;
            Position::new_absolute().bottom(bottom).left(0.)
        };
        let mut stack = rect()
            .width(Size::fill())
            .max_width(Size::px(WIDTH))
            .spacing(8.);
        for (text, _) in live {
            let (owner, sentence) = match text
                .strip_prefix('[')
                .and_then(|rest| rest.split_once("] "))
            {
                Some((id, sentence)) => (
                    Some(
                        self.plugins
                            .iter()
                            .find(|(known, _)| known == id)
                            .map_or_else(|| id.to_owned(), |(_, name)| name.clone()),
                    ),
                    sentence.to_owned(),
                ),
                None => (None, text.clone()),
            };
            let gone = text.clone();
            stack = stack.child(notice_card(
                palette,
                palette.bad,
                owner,
                i18n::tr(&sentence).to_owned(),
                None,
                None,
                EventHandler::new(move |()| {
                    let next: Vec<_> = showing
                        .peek()
                        .iter()
                        .filter(|(shown, _)| *shown != gone)
                        .cloned()
                        .collect();
                    showing.set(next);
                }),
            ));
        }
        for approval in &self.approvals {
            stack = stack.child(ApprovalCard {
                link: self.link.clone(),
                approval: approval.clone(),
                palette,
            });
        }
        for card in &self.posted {
            stack = stack.child(posted_card(&self.link, palette, card, None));
        }
        let content = if let Some(height) = max_height {
            ScrollView::new()
                .width(Size::fill())
                .height(Size::auto())
                .max_width(Size::px(WIDTH))
                .max_height(Size::px(height))
                .child(stack)
                .into_element()
        } else {
            stack.into_element()
        };
        rect()
            .position(corner)
            .layer(Layer::Overlay)
            .width(Size::fill())
            .padding(EDGE)
            .cross_align(Alignment::end())
            .child(content)
    }
}

/// The card of one posted notification, with its answers: the same in the window's stack and in the panel outside the window.
/// `carrying` is the panel's, where letting go at the end of a carry is not a press of what was under the pointer.
pub(crate) fn posted_card(
    link: &Link,
    palette: Palette,
    card: &view::NotificationCard,
    carrying: Option<Carrying>,
) -> impl IntoElement + use<> {
    let id = card.id;
    let mut answers = rect().spacing(6.);
    if card.buttons_in_a_row() {
        answers = answers.horizontal();
    }
    for button in &card.buttons {
        let link = link.clone();
        let pressed = button.id.clone();
        answers = answers.child(
            Button::new()
                .child(button.label.clone())
                .on_press(move |_| {
                    if Carrying::now(carrying) {
                        return;
                    }
                    link.send(Command::AnswerNotification {
                        id,
                        button: Some(pressed.clone()),
                    });
                }),
        );
    }
    let open = card.opens.then(|| {
        let link = link.clone();
        EventHandler::new(move |()| {
            if !Carrying::now(carrying) {
                link.send(Command::OpenNotification { id });
            }
        })
    });
    let link = link.clone();
    notice_card(
        palette,
        if card.warning {
            palette.warning
        } else {
            palette.send
        },
        card.from.clone(),
        // The application's own words are in its table; an extension's are its own and are shown as written.
        match card.from {
            None => i18n::tr(&card.text).to_owned(),
            Some(_) => card.text.clone(),
        },
        (!card.buttons.is_empty()).then(|| answers.into_element()),
        open,
        EventHandler::new(move |()| {
            if !Carrying::now(carrying) {
                link.send(Command::AnswerNotification { id, button: None });
            }
        }),
    )
}

/// One card of the stack: who it is from, what it says, the answers offered under it, and the cross that closes it. Its ground
/// is the surface with a third of `tint` in it, so that what went wrong and what is asked are told apart at a glance. A card
/// that opens something (`on_open`) does it when its words are pressed.
pub(crate) fn notice_card(
    palette: Palette,
    tint: (u8, u8, u8),
    from: Option<String>,
    text: String,
    answers: Option<Element>,
    on_open: Option<EventHandler<()>>,
    on_close: EventHandler<()>,
) -> impl IntoElement {
    let blend = |tint: u8, surface: u8| ((tint as u16 + surface as u16 * 2) / 3) as u8;
    let mut words = rect()
        .width(Size::flex(1.))
        .spacing(2.)
        .maybe_child(from.map(|name| {
            label()
                .text(name)
                .color(palette.text)
                .font_size(12.)
                .font_weight(FontWeight::BOLD)
        }))
        .child(label().text(text).color(palette.text).font_size(13.));
    if let Some(open) = on_open {
        words = words.on_press(move |_| open.call(()));
    }
    rect()
        .width(Size::fill())
        .padding((10., 14.))
        .corner_radius(10.)
        .spacing(8.)
        .background((
            blend(tint.0, palette.surface.0),
            blend(tint.1, palette.surface.1),
            blend(tint.2, palette.surface.2),
        ))
        .shadow(Shadow::new().y(4.).blur(16.).color((0, 0, 0, 90)))
        .child(
            rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .child(words)
                .child(
                    rect()
                        .padding((2., 8.))
                        .on_press(move |_| on_close.call(()))
                        .child(label().text("\u{2715}").color(palette.text).font_size(14.)),
                ),
        )
        .maybe_child(answers)
}
