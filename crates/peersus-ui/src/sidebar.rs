//! The sidebar: the bar down the left side that holds the dashboard menu, the seven entries of
//! Settings (Overview, Device, Appearance, Network, Extensions, History, About). Inside Settings it
//! is expanded, with a picture and a name for each entry, and it comes in from the left edge, the
//! page being pushed along as it grows, instead of simply appearing.
//!
//! The places of the application (Send, Receive, Peers) are not in it: they are the bar across the
//! top, and are left exactly as they are.
//!
//! How wide each state is and when the names show are `view::Sidebar`, which knows no toolkit.
//! This file only draws it. The bar is one width, followed toward the width it is asked to have
//! from wherever it is drawn: it starts from nothing, so it grows in from the left edge.

use freya::prelude::*;
use peersus_core::config::Navigation;

use crate::{
    avatar_view::{Avatar, Shared},
    glyph, motion,
    theme::Palette,
    view::{SettingsTab, Sidebar},
};

/// How far in from the edge of the bar a picture sits when the bar is wide, in pixels.
const INSET: f32 = 14.;

/// One entry of the dashboard menu: its picture in its own colour, and its name while there is room.
#[derive(PartialEq)]
struct Entry {
    tab: SettingsTab,
    active: bool,
    palette: Palette,
    /// The width the sidebar has now, in pixels: the name appears as it grows past the pictures.
    width: f32,
    on_select: EventHandler<SettingsTab>,
}

impl Component for Entry {
    fn render(&self) -> impl IntoElement {
        let (tab, palette, on_select) = (self.tab, self.palette, self.on_select.clone());
        let mut hovered = use_state(|| false);
        let own = if self.active { palette.active_icons } else { palette.icons };
        let name = Sidebar::names_at(self.width);
        // A picture sits at the same distance from the left edge until the bar is about to be only
        // as wide as the pictures, where it is centred. Nothing is left to push it past the edge.
        let pictures_only = name <= 0.;
        let mut row = rect()
            .width(Size::fill())
            .height(Size::px(44.))
            .padding((0., if pictures_only { 0. } else { INSET * name }))
            .horizontal()
            .main_align(if pictures_only { Alignment::Center } else { Alignment::Start })
            .cross_align(Alignment::Center)
            .spacing(12.)
            .corner_radius(12.)
            .background(if self.active || hovered() { palette.surface } else { palette.sidebar })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| on_select.call(tab))
            // The mark of the entry that is shown, in its own colour, at the left of the picture.
            .child(
                rect()
                    .width(Size::px(3.))
                    .height(Size::px(18.))
                    .corner_radius(2.)
                    .background(if self.active { own } else { palette.sidebar }),
            )
            .child(glyph::picture(
                glyph::name_of_tab(tab),
                glyph::LIST,
                &palette,
                if self.active { own } else { glyph::resting_ink(&palette) },
            ));
        // The name takes no room until it has begun to show, so a narrow bar holds the picture alone.
        if !pictures_only {
            row = row.child(
                rect().opacity(name).child(
                    label()
                        .text(crate::i18n::tr(tab.title()))
                        .color(if self.active { palette.text } else { palette.muted })
                        .font_size(15.)
                        .max_lines(1),
                ),
            );
        }
        row
    }
}

/// The profile row at the very bottom: a round picture with the first letter of the device name,
/// and the name beside it, cut with an ellipsis when it is too long for the room.
#[derive(PartialEq)]
struct Profile {
    name: String,
    picture: Option<Shared>,
    palette: Palette,
    width: f32,
    on_select: EventHandler<SettingsTab>,
}

impl Component for Profile {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut hovered = use_state(|| false);
        let names = Sidebar::names_at(self.width);
        let circle = Avatar {
            picture: self.picture.as_ref().map(|shared| shared.0.clone()),
            name: self.name.clone(),
            size: 36.,
            palette,
        };
        let on_select = self.on_select.clone();
        // A click opens the tab where the name shown here is edited.
        let mut row = rect()
            .width(Size::fill())
            .height(Size::px(52.))
            .padding((0., INSET))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(12.)
            .corner_radius(12.)
            .background(if hovered() { palette.surface } else { palette.sidebar })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| on_select.call(Sidebar::PROFILE))
            .child(circle);
        if names > 0. {
            row = row.child(
                rect().opacity(names).child(
                    label()
                        .text(Sidebar::fit_name(&self.name, Sidebar::PROFILE_CHARACTERS))
                        .color(palette.text)
                        .font_size(15.)
                        .max_lines(1),
                ),
            );
        }
        row
    }
}

/// The sidebar, expanded: the dashboard menu, growing in from the left edge. The first six entries
/// are at the top, About is at the foot, and under it is the profile.
#[derive(PartialEq)]
pub struct Bar {
    pub active: SettingsTab,
    pub device_name: String,
    /// This device's own picture, if it has one: the profile row draws it instead of the initial.
    pub picture: Option<Shared>,
    /// What the sidebar is asked to be: hidden, only its icons, or expanded. The width follows.
    pub state: Navigation,
    pub reduced_motion: bool,
    pub palette: Palette,
    pub on_select: EventHandler<SettingsTab>,
}

impl Component for Bar {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // One width, followed toward the state's own, from wherever it was last drawn: it starts
        // from nothing, so it grows in from the left edge, and a move turned round in the middle
        // goes back from that very spot.
        let width = motion::use_toward(Sidebar::width_for(self.state), Sidebar::WIDE, self.reduced_motion);
        let entry = |tab: SettingsTab| Entry {
            tab,
            active: tab == self.active,
            palette,
            width,
            on_select: self.on_select.clone(),
        };
        let mut bar = rect()
            .width(Size::px(width))
            .height(Size::fill())
            .background(palette.sidebar)
            .padding((6., 0.))
            .spacing(6.)
            // `flex` on the column is what lets the spacer take what is left and so push About
            // and the profile to the bottom.
            .content(Content::flex())
            // What the width is too narrow for is cut, not drawn over the page beside it.
            .overflow(Overflow::Clip);
        for tab in Sidebar::TOP {
            bar = bar.child(entry(tab));
        }
        bar.child(rect().height(Size::flex(1.)))
            .child(entry(Sidebar::FOOT))
            .child(Profile {
                name: self.device_name.clone(),
                picture: self.picture.clone(),
                palette,
                width,
                on_select: self.on_select.clone(),
            })
    }
}
