//! The places and the ways to them: the tabs across the top, the bar along the bottom (or down the side), and the menu that
//! opens the rest.
use super::*;

#[derive(PartialEq)]
pub(crate) struct Tab {
    pub(crate) entry: NavEntry,
    pub(crate) active: bool,
    pub(crate) palette: Palette,
    pub(crate) dense: bool,
    pub(crate) on_select: EventHandler<Page>,
}
impl Component for Tab {
    fn render(&self) -> impl IntoElement {
        let (page, palette) = (self.entry.page, self.palette);
        let on_select = self.on_select.clone();
        let mut hovered = use_state(|| false);
        let accent = page.accent(&palette);
        let tab = rect()
            .width(Size::flex(1.))
            .padding((0., if self.dense { 6. } else { 22. }))
            .height(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .spacing(8.)
            .border(if self.active {
                Border::new()
                    .fill(accent)
                    .width(BorderWidth {
                        top: 0.,
                        right: 0.,
                        bottom: 3.,
                        left: 0.,
                    })
                    .alignment(BorderAlignment::Inner)
            } else {
                // No border at all: a default one would box in every inactive tab.
                Border::new().width(BorderWidth {
                    top: 0.,
                    right: 0.,
                    bottom: 0.,
                    left: 0.,
                })
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |_| on_select.call(page));
        let tab = tab.child(glyph::picture_on(
            self.entry.icon,
            glyph::BAR,
            accent,
            if self.active || hovered() {
                palette.text
            } else {
                glyph::resting_ink(&palette)
            },
            palette.header,
        ));
        tab.child(
            mono(
                i18n::tr(&self.entry.caption).to_owned(),
                if self.active || hovered() {
                    palette.text
                } else {
                    palette.muted
                },
                14.,
            )
            .max_lines(1)
            .width(Size::flex(1.)),
        )
    }
}

/// The place a bottom-bar entry opens.
pub(crate) fn page_of(place: view::BottomPlace) -> Page {
    match place {
        view::BottomPlace::Send => Page::Send,
        view::BottomPlace::Peers => Page::Peers,
        view::BottomPlace::Receive => Page::Receive,
        view::BottomPlace::Transfers => Page::Transfers,
    }
}

/// One entry of the main navigation: where it goes, the words under it and its picture. The application's own places and every
/// extension's pages are the SAME entries, drawn by the same code into the same bar and opened into the same clipped page slot: the
/// application does not know what any page shows, only that an owner (itself or an extension) registered it.
#[derive(Clone, PartialEq)]
pub(crate) struct NavEntry {
    pub(crate) page: Page,
    /// The words under the picture, not yet translated: the place's own name, or what an extension called its page.
    pub(crate) caption: String,
    /// A picture the application has; a name an extension gave that it does not have is drawn as the generic one.
    pub(crate) icon: &'static str,
}

impl NavEntry {
    /// One of the application's own places.
    fn own(page: Page) -> Self {
        let icon = match page {
            Page::Send => "send",
            Page::Receive => "receive",
            Page::Peers => "peers",
            Page::Transfers => "transfers",
            Page::Extensions => "plugins",
            Page::Settings | Page::Contributed(_) => "settings",
        };
        Self {
            page,
            caption: page.name().to_owned(),
            icon,
        }
    }
}

/// Every entry of the main navigation for this form, in order: the application's own places, then what extensions registered.
/// The one list both the desktop's header and the phone's bottom bar are drawn from, so the two can never differ.
pub(crate) fn nav_entries(form: view::Form, contributed: &[NavEntry]) -> Vec<NavEntry> {
    let places = form.bottom_places();
    let own: Vec<Page> = if places.is_empty() {
        Page::MAIN.to_vec()
    } else {
        places.iter().map(|place| page_of(*place)).collect()
    };
    own.into_iter()
        .map(NavEntry::own)
        .chain(contributed.iter().cloned())
        .collect()
}

/// The pages extensions registered in the main navigation, in the order they are drawn, after the application's own: a place
/// of its own for each, or, when the person chose so (`extension_pages_together`), one place, "Extensions", they all share.
pub(crate) fn contributed_entries(view: &View) -> Vec<NavEntry> {
    let pages = contributed_page::pages_here(view, peersus_api::contributions::Placement::Primary);
    if pages.is_empty() {
        return Vec::new();
    }
    if view.settings.extension_pages_together {
        return vec![NavEntry::own(Page::Extensions)];
    }
    pages
        .into_iter()
        .map(|(plugin, page)| NavEntry {
            page: Page::Contributed(tab_key(plugin.id.as_str(), &page.id)),
            icon: glyph::known_or_generic(page.icon.as_deref()),
            caption: page.label,
        })
        .collect()
}

/// The identity of an extension's page in the navigation: its owner and its own id, and never its place among the others, so that
/// an extension that stops, or another that comes, never moves the person to someone else's page.
pub(crate) fn tab_key(owner: &str, page: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (owner, page).hash(&mut hasher);
    hasher.finish()
}

/// One place in the bar of a screen that is held: its picture over its name, an equal share of the bar's length, and large enough
/// for a thumb. Marked like a tab is, in the place's own colour, on the edge of the bar that faces the page.
#[derive(PartialEq)]
pub(crate) struct BottomTab {
    pub(crate) entry: NavEntry,
    pub(crate) active: bool,
    /// Whether the bar stands down the left edge (see `view::Form::places_are_at_the_side`), and not along the bottom.
    pub(crate) side: bool,
    pub(crate) palette: Palette,
    pub(crate) on_select: EventHandler<Page>,
}
impl Component for BottomTab {
    fn render(&self) -> impl IntoElement {
        let (page, palette) = (self.entry.page, self.palette);
        let on_select = self.on_select.clone();
        let accent = page.accent(&palette);
        let colour = if self.active { accent } else { palette.muted };
        // The edge that faces the page: the top of a bar along the bottom, the right of one down the left edge.
        let mark = |width: f32| {
            if self.side {
                BorderWidth {
                    top: 0.,
                    right: width,
                    bottom: 0.,
                    left: 0.,
                }
            } else {
                BorderWidth {
                    top: width,
                    right: 0.,
                    bottom: 0.,
                    left: 0.,
                }
            }
        };
        let (width, height) = if self.side {
            (Size::fill(), Size::flex(1.))
        } else {
            (Size::flex(1.), Size::fill())
        };
        rect()
            .width(width)
            .height(height)
            .center()
            .spacing(4.)
            .border(if self.active {
                Border::new()
                    .fill(accent)
                    .width(mark(3.))
                    .alignment(BorderAlignment::Inner)
            } else {
                Border::new().width(mark(0.))
            })
            .on_press(move |_| on_select.call(page))
            .child(glyph::picture_on(
                self.entry.icon,
                glyph::BAR,
                accent,
                if self.active {
                    palette.text
                } else {
                    glyph::resting_ink(&palette)
                },
                palette.background,
            ))
            // Every place has the same share of the bar, whatever its name is in the language in use and however many places
            // extensions add: the name is made to fit that share, and is never broken over two lines.
            .child(text::Fitted {
                text: i18n::tr(&self.entry.caption).to_owned().into(),
                fit: view::Fit::CAPTION,
                colour,
                family: Some("monospace"),
            })
    }
}

/// The bar of the places on a screen that is held, where a thumb reaches: along the bottom when upright, down the left edge when the
/// screen is on its side. ONE bar, drawn from the same entries either way.
#[derive(PartialEq)]
pub(crate) struct BottomBar {
    pub(crate) active: Page,
    /// Everything in the bar, from [`nav_entries`].
    pub(crate) entries: Vec<NavEntry>,
    /// Whether it stands down the left edge (see `view::Form::places_are_at_the_side`).
    pub(crate) side: bool,
    pub(crate) palette: Palette,
    pub(crate) on_select: EventHandler<Page>,
}
/// How wide the bar of the places is when it stands down the left edge, in pixels: a thumb's width, with its name under the picture.
pub(crate) const SIDE_BAR: f32 = 76.;
/// Where the bottom bar begins, in window coordinates, as last laid out: the sheet's dimmed layer stops there so the bar stays clear.
#[derive(Clone, Copy)]
pub(crate) struct BottomBarTop(pub State<Option<f32>>);
impl Component for BottomBar {
    fn render(&self) -> impl IntoElement {
        let top = try_consume_context::<BottomBarTop>().filter(|_| !self.side);
        let mut bar = if self.side {
            rect().width(Size::px(SIDE_BAR)).height(Size::fill())
        } else {
            rect()
                .width(Size::fill())
                .height(Size::px(64.))
                .horizontal()
        };
        bar = bar
            .maybe(top.is_some(), |bar| {
                let mut top = top.expect("checked").0;
                bar.on_sized(move |e: Event<SizedEventData>| {
                    top.set_if_modified(Some(e.area.min_y()))
                })
            })
            // `flex` is what makes each place's `Size::flex(1.)` an equal share of the bar.
            .content(Content::flex())
            .background(self.palette.background)
            .border(
                Border::new()
                    .fill(self.palette.border)
                    .width(if self.side {
                        BorderWidth {
                            top: 0.,
                            right: 1.,
                            bottom: 0.,
                            left: 0.,
                        }
                    } else {
                        BorderWidth {
                            top: 1.,
                            right: 0.,
                            bottom: 0.,
                            left: 0.,
                        }
                    })
                    .alignment(BorderAlignment::Inner),
            );
        for entry in self.entries.iter().cloned() {
            bar = bar.child(BottomTab {
                active: entry.page == self.active,
                entry,
                side: self.side,
                palette: self.palette,
                on_select: self.on_select.clone(),
            });
        }
        bar
    }
}

/// The menu button at the right of the header. A plain click goes to Settings; the right button
/// opens the menu of the other screens, which is drawn by [`menu_layer`] over the page, because a
/// panel that is a child of this small button would be cut to its size.
#[derive(PartialEq)]
pub(crate) struct MenuButton {
    pub(crate) palette: Palette,
    pub(crate) on_select: EventHandler<Page>,
    pub(crate) open: State<bool>,
    /// Whether a press opens the sheet of places that comes down from the top (a phone), and not Settings itself.
    pub(crate) sheet: bool,
}
impl Component for MenuButton {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut open = self.open;
        let mut hovered = use_state(|| false);
        let mut touch = use_state(|| view::Touch::new(HEADER_ROUND));
        // The same kind of control as the three window buttons beside it: a disc, as large as theirs, filled with the accent the person
        // chose and with a mark drawn large enough to be read, so that Settings is no smaller and no paler than the buttons it stands with.
        let fill = if hovered() {
            glyph::dim(palette.send, palette.background, 0.8)
        } else {
            palette.send
        };
        let ink = glyph::ink_on(fill);
        rect()
            .width(Size::px(HEADER_ROUND))
            .height(Size::px(HEADER_ROUND))
            .corner_radius(HEADER_ROUND / 2.)
            .center()
            .background(fill)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_pointer_down(move |e: Event<PointerEventData>| {
                // The header drags the window from any pointer-down: this button is not a handle.
                e.stop_propagation();
                // A touch that BEGINS here is the only one that can press this button: where it began, and when, is said now, so that a finger that
                // began elsewhere and is lifted over the button (the bar moves under a finger as the page scrolls) does not open Settings.
                let (g, l) = (e.global_location(), e.element_location());
                let mut next = view::Touch::new(HEADER_ROUND);
                next.down(
                    (l.x as f32, l.y as f32),
                    (g.x as f32, g.y as f32),
                    monotonic_ms(),
                );
                touch.set(next);
                // The right button opens the menu. A single handler does both, because a second
                // one on the same event could replace this one.
                if matches!(&*e, PointerEventData::Mouse(m) if m.button == Some(MouseButton::Right))
                {
                    open.set(!open());
                }
            })
            .on_capture_global_pointer_move(move |e: Event<PointerEventData>| {
                let g = e.global_location();
                let mut next = *touch.peek();
                next.moved((g.x as f32, g.y as f32));
                touch.set_if_modified(next);
            })
            .on_touch_cancel(move |_| {
                let mut next = *touch.peek();
                next.cancel();
                touch.set(next);
            })
            .on_pointer_press({
                let on_select = self.on_select.clone();
                let sheet = self.sheet;
                move |e: Event<PointerEventData>| {
                    // The right button opened the menu when it went down; lifting it is not a click as well.
                    let mouse = match &*e {
                        PointerEventData::Mouse(mouse) => Some(mouse.button.map(|button| {
                            if button == MouseButton::Left {
                                view::PointerButton::First
                            } else {
                                view::PointerButton::Other
                            }
                        })),
                        PointerEventData::Touch(_) => None,
                    };
                    if !view::presses(mouse) {
                        return;
                    }
                    // Only a touch that began on the button, stayed on it and was lifted inside its round is a press; the toolkit says a press for
                    // whatever is under the finger when it is lifted. How long it was held is not asked: this button only opens a place, as the
                    // tabs do, and the circles' actuation point (a brush must not send) would refuse a quick tap, and a touchpad's tap-to-click.
                    // A mouse click, which has no finger to be carried away, is judged the same way and passes.
                    let (g, l) = (e.global_location(), e.element_location());
                    let mut next = *touch.peek();
                    let pressed =
                        next.lifted_inside((l.x as f32, l.y as f32), (g.x as f32, g.y as f32));
                    touch.set(next);
                    if !pressed {
                        return;
                    }
                    // On a phone there is no right button, so the press opens the places of Settings, which come down from the top; a
                    // second press closes them. Elsewhere it goes straight to Settings, as it always did.
                    if sheet {
                        open.set(!open());
                    } else {
                        open.set(false);
                        on_select.call(Page::Settings);
                    }
                }
            })
            .child(glyph::draw_duo(
                "settings",
                glyph::HEADER_MARK,
                ink,
                ink,
                glyph::dim(glyph::soft_of(fill), fill, 0.9),
                glyph::soft_of(fill),
            ))
    }
}

/// What the right click on the menu button opens: every tab of Settings, in the order Settings lists them (`SettingsTab::ALL`, the one
/// list of them, so a tab that is added is here too), as a panel under the header at the right. Returned in two parts, the panel and the
/// layer under it that takes a click anywhere else to close it, because only the panel moves when it opens.
pub(crate) fn menu_layer(
    palette: Palette,
    on_open: EventHandler<SettingsTab>,
    mut open: State<bool>,
) -> (Element, Element) {
    let mut panel = rect()
        .position(Position::new_absolute().top(66.).right(28.))
        .width(Size::px(200.))
        .padding(6.)
        .spacing(2.)
        .corner_radius(12.)
        .background(palette.raised)
        .border(
            Border::new()
                .fill(palette.border)
                .width(1.)
                .alignment(BorderAlignment::Inner),
        )
        // The backdrop closes the menu on a pointer-down: this one must not reach it, or the menu
        // would be gone before the click on an item is finished (a press is a down AND an up).
        .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation());
    for &tab in SettingsTab::shown() {
        let on_open = on_open.clone();
        panel = panel.child(MenuItem {
            tab,
            palette,
            on_choose: EventHandler::new(move |tab: SettingsTab| {
                open.set(false);
                on_open.call(tab);
            }),
        });
    }
    let backdrop: Element = rect()
        .width(Size::fill())
        .height(Size::fill())
        .on_pointer_down(move |_| open.set(false))
        .into();
    (panel.into(), backdrop)
}

#[derive(PartialEq)]
pub(crate) struct MenuItem {
    pub(crate) tab: SettingsTab,
    pub(crate) palette: Palette,
    pub(crate) on_choose: EventHandler<SettingsTab>,
}
impl Component for MenuItem {
    fn render(&self) -> impl IntoElement {
        let (tab, palette) = (self.tab, self.palette);
        let on_choose = self.on_choose.clone();
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::fill())
            .padding((9., 12.))
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(10.)
            .corner_radius(8.)
            .background(if hovered() {
                palette.surface
            } else {
                palette.raised
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |e: Event<PressEventData>| {
                // A press goes up to its parent: the menu is drawn over the page, but without this
                // the press would also reach the layer under it, which closes the menu first.
                e.stop_propagation();
                on_choose.call(tab);
            })
            .child(glyph::picture(
                glyph::name_of_tab(tab),
                glyph::LIST,
                &palette,
                palette.muted,
            ))
            .child(
                label()
                    .text(i18n::tr(tab.title()))
                    .color(palette.text)
                    .font_size(14.),
            )
    }
}
