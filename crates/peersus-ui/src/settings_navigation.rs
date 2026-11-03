//! Search jumps use measured content coordinates on both layouts, never guessed card indices.
use crate::{settings_search::Target, theme::Palette};
use freya::prelude::*;

#[derive(Clone, Copy)]
pub struct Navigation {
    pub origin: State<Option<f32>>,
    pub requested: State<Option<Target>>,
    pub selected: State<Option<Target>>,
    pub go_to: State<Option<f32>>,
    pub tap: State<crate::view::Tap>,
}

pub fn anchor(target: Target, content: impl IntoElement, palette: Palette) -> Element {
    Anchor { target, content: content.into_element(), palette, inline: false }.into()
}

/// Contribution controls keep their intrinsic width inside one horizontal device row.
pub fn inline_anchor(target: Target, content: impl IntoElement, palette: Palette) -> Element {
    Anchor { target, content: content.into_element(), palette, inline: true }.into()
}

#[derive(PartialEq)]
struct Anchor { target: Target, content: Element, palette: Palette, inline: bool }
impl Component for Anchor {
    fn render(&self) -> impl IntoElement {
        let Some(mut navigation) = try_consume_context::<Navigation>() else { return self.content.clone() };
        let requested = navigation.requested.read().as_ref() == Some(&self.target);
        let selected = navigation.selected.read().as_ref() == Some(&self.target);
        let node = rect().width(if self.inline { Size::auto() } else { Size::fill() })
            .border(Border::new().fill(if selected { Color::from(self.palette.receive) } else { Color::TRANSPARENT }).width(1.).alignment(BorderAlignment::Inner))
            .child(self.content.clone());
        if requested {
            node.on_sized(move |event: Event<SizedEventData>| {
                if let Some(origin) = *navigation.origin.peek() {
                    navigation.go_to.set(Some((event.area.min_y() - origin).max(0.)));
                    navigation.requested.set(None);
                }
            }).into()
        } else {
            node.into()
        }
    }
}

/// Retain the toolkit's focus/keyboard/accessibility behavior, but never turn a scrolling
/// finger into a search selection. The parent ScrollView still receives every movement.
#[derive(PartialEq)]
pub struct ResultButton {
    pub entry: crate::settings_search::Entry,
    pub palette: Palette,
    pub on_select: EventHandler<Target>,
}
impl Component for ResultButton {
    fn render(&self) -> impl IntoElement {
        let mut tap = consume_context::<Navigation>().tap;
        let target = self.entry.target.clone();
        let on_select = self.on_select.clone();
        rect().width(Size::fill())
            .child(Button::new().width(Size::fill())
                .on_pointer_down(move |event: Event<PointerEventData>| {
                    let point = event.global_location();
                    let mut next = crate::view::Tap::default();
                    next.down((point.x as f32, point.y as f32));
                    tap.set(next);
                })
                .on_press(move |event: Event<PressEventData>| {
                    let point = match event.data() {
                        PressEventData::Mouse(data) => Some(data.global_location),
                        PressEventData::Touch(data) => Some(data.global_location),
                        PressEventData::Keyboard(_) | PressEventData::Accessibility(_) => None,
                    };
                    let allowed = point.is_none_or(|point| {
                        let mut next = *tap.peek();
                        let allowed = next.up((point.x as f32, point.y as f32));
                        tap.set(next);
                        allowed
                    });
                    if allowed { on_select.call(target.clone()); }
                })
                .child(rect().width(Size::fill()).spacing(4.)
                    .child(label().text(format!("{} · {}", self.entry.title, crate::i18n::tr(self.entry.target.tab().title()))).color(self.palette.text).font_size(16.))
                    .child(label().text(self.entry.detail.chars().take(140).collect::<String>()).color(self.palette.muted).font_size(13.))))
    }
}
