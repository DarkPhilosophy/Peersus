//! How the interface is arranged for its window: the sidebar, the forms (wide, compact, side), the header and the name in it,
//! the safe area, where the bottom bar goes and how text fits. Pure rules, no toolkit.
use super::*;

/// The sidebar's two widths and what happens between them. It is always there: it is wide, with
/// the names of the places, or narrow, with only their icons.
pub struct Sidebar;

impl Sidebar {
    /// Width with names, in pixels.
    pub const WIDE: f32 = 248.;
    /// Width with icons only, in pixels: an icon and its padding.
    pub const NARROW: f32 = 56.;

    /// The entries at the top of the sidebar, in order.
    pub const TOP: [SettingsTab; 8] = [
        SettingsTab::Overview,
        SettingsTab::Device,
        SettingsTab::Appearance,
        SettingsTab::Languages,
        SettingsTab::Network,
        SettingsTab::Extensions,
        SettingsTab::Permissions,
        SettingsTab::Binds,
    ];

    /// How many characters of the device name fit beside the picture in the profile row.
    pub const PROFILE_CHARACTERS: usize = 14;

    /// The entry at the foot of the sidebar, just above the profile.
    pub const FOOT: SettingsTab = SettingsTab::About;

    /// The tab a click on the profile opens: the one where the name shown in it is edited.
    pub const PROFILE: SettingsTab = SettingsTab::Device;

    /// What the sidebar is, from where the person is and the one option. Outside Settings it is
    /// not there, or only its icons if the person asked for that; inside Settings it is expanded,
    /// and it is the same from hidden and from icons alike.
    pub fn state(inside_settings: bool, keep_icons: bool) -> peersus_core::config::Navigation {
        use peersus_core::config::Navigation;
        match (inside_settings, keep_icons) {
            (true, _) => Navigation::Expanded,
            (false, true) => Navigation::Icons,
            (false, false) => Navigation::Hidden,
        }
    }

    /// The sidebar of the screen that is shown. While the device has no name the only thing on screen is the
    /// question that asks for it, and the sidebar is not there at all, whatever page was asked for: it would
    /// stand beside the question with the page's own menu in it, and a way into a page that is not shown.
    ///
    /// NOTICE(@DarkPhilosophy): the permanent Android sidebar is disabled; navigation uses the top Settings sheet.
    /// TODO(@DarkPhilosophy): reconsider when designing a left-side mobile menu. Desktop preferences remain unchanged.
    pub fn state_for(
        named: bool,
        inside_settings: bool,
        keep_icons: bool,
        form: Form,
    ) -> peersus_core::config::Navigation {
        use peersus_core::config::Navigation;
        if !named || cfg!(target_os = "android") || form == Form::Compact {
            return Navigation::Hidden;
        }
        Self::state(inside_settings, keep_icons)
    }

    /// The page that opening `wanted` from the menu leads to: the page itself once the device has a name, and
    /// none while it has not, since the question is all there is until it is answered.
    pub fn page_opened<P>(named: bool, wanted: P) -> Option<P> {
        named.then_some(wanted)
    }

    /// `name` cut to at most `room` characters, with an ellipsis in place of what was cut, and
    /// counted in characters and not bytes, so that an accented letter is never split. A name
    /// that already fits is left as it is.
    pub fn fit_name(name: &str, room: usize) -> String {
        let count = name.chars().count();
        if count <= room {
            return name.to_owned();
        }
        if room == 0 {
            return String::new();
        }
        let mut kept: String = name.chars().take(room - 1).collect();
        kept.push('\u{2026}');
        kept
    }

    /// The letter shown on the profile picture when there is no photo: the first letter of the
    /// name in capitals, or a question mark when the name has none.
    pub fn initial(name: &str) -> char {
        name.trim()
            .chars()
            .next()
            .and_then(|c| c.to_uppercase().next())
            .unwrap_or('?')
    }

    /// The width a state is heading for: nothing when the bar is hidden, the narrow width for the
    /// pictures alone, the full width when it is expanded. The bar is this one width, followed
    /// from wherever it is drawn, so that every move between states is the same kind of move and
    /// expanded is reached from hidden and from the pictures alike.
    pub fn width_for(state: peersus_core::config::Navigation) -> f32 {
        use peersus_core::config::Navigation;
        match state {
            Navigation::Hidden => 0.,
            Navigation::Icons => Self::NARROW,
            Navigation::Expanded => Self::WIDE,
        }
    }

    /// How visible the names are at a given width: not at all up to the pictures' width, fully at
    /// the full width, and in between as it opens, so that a name never sits on a bar too narrow
    /// to hold it.
    pub fn names_at(width: f32) -> f32 {
        ((width - Self::NARROW) / (Self::WIDE - Self::NARROW)).clamp(0., 1.)
    }

    /// How long a move from width `from` to width `to` takes, in milliseconds: in proportion to the
    /// distance, so that a move turned round in the middle is quick and the speed is always the
    /// same. Nothing at all with reduced motion.
    pub fn duration_ms(from: f32, to: f32, reduced_motion: bool) -> u64 {
        Motion::duration_ms(from / Self::WIDE, to / Self::WIDE, reduced_motion)
    }
}

/// How the interface is arranged for the shape of its window. One interface that adapts, not two: the
/// same screens are drawn, and what changes is where the places are and how much of the chrome stays.
/// The shape decides, portrait or landscape, and not a number of pixels: a window dragged tall is a
/// phone-shaped window whatever its width. One more thing is asked, of the platform and not of the shape:
/// whether the screen is held in the hands. A phone turned on its side is as wide as a small laptop, but it
/// has no window to close and is reached by two thumbs at its short edges, so it is a form of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Landscape or square, on a desk: the places across the top, the bar beside the page.
    Wide,
    /// Portrait: the places along the bottom, within thumb's reach, and the profile is its picture alone.
    Compact,
    /// Landscape, held in the hands: the places down the left edge, where the thumb of the hand that holds it is; no window buttons; the
    /// pages laid out for the width they have, as when wide.
    Side,
}

impl Form {
    /// The form for a window `width` by `height` pixels, on a screen that is `held` in the hands or is not. Portrait, strictly taller
    /// than wide, is compact wherever it is. A size that is not yet known (nothing has been laid out, so it is zero or not a number) is
    /// the landscape arrangement of that kind of screen, which is what is shown until the first measurement: on a desk the desktop's, so
    /// that nothing flashes the wrong one, and in the hands never the desktop's, which has buttons for a window a phone does not have.
    pub fn of(width: f32, height: f32, held: bool) -> Self {
        let known = |side: f32| side.is_finite() && side > 0.;
        if known(width) && known(height) && height > width {
            Self::Compact
        } else if held {
            Self::Side
        } else {
            Self::Wide
        }
    }

    /// Whether the bar of the places ([`Form::bottom_places`]) stands down the left edge and not along the bottom: a screen held on its
    /// side is short, and the hands are at its ends.
    pub fn places_are_at_the_side(self) -> bool {
        self == Self::Side
    }

    /// The places in the bar of a screen that is held, in order: the same four as across the top on a desktop, in the same
    /// order, so that a person finds each where they know it. Send is the first and the screen a person opens
    /// the application on; without it there would be no way back to it from the others. Along the bottom when upright, down the left
    /// edge when on its side ([`Form::places_are_at_the_side`]). Empty when wide, where the places are across the top.
    pub fn bottom_places(self) -> &'static [BottomPlace] {
        match self {
            Self::Compact | Self::Side => &[
                BottomPlace::Send,
                BottomPlace::Receive,
                BottomPlace::Peers,
                BottomPlace::Transfers,
            ],
            Self::Wide => &[],
        }
    }

    /// Whether the Send screen is the circle alone: no heading, no sentence beside it. The circle is
    /// already the button that chooses files, and shows the code once there is something to send.
    pub fn send_is_the_circle_alone(self) -> bool {
        self == Self::Compact
    }

    /// What the circle says under its title. A phone is touched and not clicked, and holds no
    /// "computer" to browse.
    pub fn circle_invitation(self) -> &'static str {
        match self {
            Self::Compact | Self::Side => "tap to choose files",
            Self::Wide => "click to browse your computer",
        }
    }

    /// Whether the header has the window's own minimise, maximise and close buttons. A phone has no such
    /// window to shrink or close: the system draws and owns that, so they are not drawn at all there.
    pub fn header_has_window_buttons(self) -> bool {
        self == Self::Wide
    }

    /// Whether the header says "connected" in words. On a phone the coloured dot alone says it, and the
    /// words would only crowd the wordmark and the menu button on a narrow screen.
    pub fn header_says_connected_in_words(self) -> bool {
        self == Self::Wide
    }

    /// Whether the header gives way as the page scrolls (see [`HeaderHide`]). On a phone the screen is short and the bar takes a
    /// twelfth of it, so it goes while the person reads down and returns when they scroll up; on a desktop the room is there and
    /// the bar holds the places and the window's own buttons, so it stays.
    pub fn header_hides_on_scroll(self) -> bool {
        self != Self::Wide
    }

    /// How far below the top of the page whatever must stay readable there starts: a message that floats at the top, the
    /// start of one of the application's pages. Where the header gives way to the page its two pieces lie over the
    /// page's top corners, the way to Settings among them: what is drawn there lies under them (a message's start was
    /// hidden by the mark and its close button by the way to Settings). It starts under their band instead; on a
    /// desktop the bar takes its own room and there is nothing to clear. An extension's whole page is not moved: it
    /// fills the room under the header's pieces, and what it draws in its top corners is its own to lay out.
    pub fn clear_of_header(self) -> f32 {
        if self.header_hides_on_scroll() {
            HEADER_TAB
        } else {
            0.
        }
    }

    /// Whether the places of Settings are a sheet that comes down from the top, as on a phone, where a bar at the side takes a quarter of
    /// the width of a screen that is already narrow; the desktop has the room, and keeps its bar.
    pub fn settings_places_come_from_the_top(self) -> bool {
        self != Self::Wide
    }

    /// The space at each side of the header, in pixels: less on a phone, where every pixel is needed.
    pub fn header_side_padding(self) -> f32 {
        match self {
            Self::Compact | Self::Side => 14.,
            Self::Wide => 28.,
        }
    }

    /// The largest the portal may be, from the width it has: its own size, or less if the width is smaller,
    /// never past the width. A width not yet known keeps the portal at its own size.
    pub fn portal_extent(own: f32, width: f32) -> f32 {
        if width.is_finite() && width > 0. {
            own.min(width)
        } else {
            own
        }
    }

    /// The width of the line round the disc, in the toolkit's logical pixels, for a screen that has `scale` device pixels to each: a
    /// whole number of DEVICE pixels, and never fewer than two. A line one logical pixel thick is 3.125 device pixels on some
    /// phones, and it does not stay where it is drawn: at a fraction of a pixel it is spread over four rows and shows at a third of its
    /// colour, and sometimes it is not seen at all, which is a ring that comes and goes as the page above it changes. A scale that is
    /// not a positive number gives the two logical pixels that were measured to hold.
    pub fn portal_line(scale: f64) -> f32 {
        if !(scale.is_finite() && scale > 0.) {
            return 2.;
        }
        let device = (scale * 1.5).round().max(2.);
        (device / scale) as f32
    }

    /// The size of the title of a screen that asks for one thing, in pixels. Smaller on a phone, where the
    /// desktop's size would be wider than the screen and break into lines of single words.
    pub fn ask_title_size(self) -> f32 {
        match self {
            Self::Compact => 36.,
            Self::Wide | Self::Side => 56.,
        }
    }

    /// Whether the field and its button stand one above the other. Side by side they are wider than a phone.
    pub fn ask_stacks_field_and_button(self) -> bool {
        self == Self::Compact
    }

    /// The space left at each side of a screen that asks for one thing, in pixels.
    pub fn ask_side_margin(self) -> f32 {
        match self {
            Self::Compact => 24.,
            Self::Wide | Self::Side => 0.,
        }
    }

    /// The space a page keeps at each side, in pixels. The desktop's is wide, which is what makes a page read
    /// as a page; on a phone the same space would take a third of the screen, so it is small there.
    pub fn page_side_margin(self) -> f32 {
        match self {
            Self::Compact | Self::Side => 20.,
            Self::Wide => 56.,
        }
    }

    /// The space a page keeps above its first line and below its last, in pixels. Every pixel of height on a phone
    /// is wanted for what the page is for, so it is small there; the desktop's is what it was.
    pub fn page_vertical_padding(self) -> f32 {
        match self {
            Self::Compact | Self::Side => 12.,
            Self::Wide => 28.,
        }
    }

    /// The space between the sections of a page, in pixels: the desktop's is what it was, a phone's is closer, so that
    /// the title, the circle and what is under it are seen together without scrolling.
    pub fn page_section_gap(self) -> f32 {
        match self {
            Self::Compact | Self::Side => 16.,
            Self::Wide => 28.,
        }
    }

    /// The width a card of a device in a list keeps, or none. On a desktop cards stand side by side and keep
    /// their own width; on a phone a card of that width is wider than half the screen and the next one would be
    /// pushed off it, so it keeps none and is as wide as the screen, with the cards one under another.
    pub fn device_card_width(self) -> Option<f32> {
        match self {
            Self::Wide | Self::Side => Some(280.),
            Self::Compact => None,
        }
    }

    /// Whether the buttons of a device's card stand one under the other, below the name. Beside the name they take the width the name
    /// needs: on a phone that leaves it a column of one letter, and a name and a state written downwards, a letter to a line. On the
    /// desktop the card is wide enough for all of them in a row, as it always was.
    pub fn device_card_buttons_stack(self) -> bool {
        self == Self::Compact
    }

    /// Whether the devices nearby that are not paired are listed beside the rest of Peers, at the right. A desktop has the
    /// width for a column of its own; on a phone they come under the pairing buttons, above the paired devices.
    pub fn nearby_list_is_beside(self) -> bool {
        self != Self::Compact
    }

    /// Whether a page puts the explanation beside the portal or under it. Side by side they need more width
    /// than a phone has, and the explanation is what is left with none.
    pub fn page_puts_portal_beside_text(self) -> bool {
        self != Self::Compact
    }
}

/// The height of the header, in pixels: the one number that both the bar's drawing and the rule that hides it use.
pub const HEADER_HEIGHT: f32 = 72.;

/// How tall the two tabs the header leaves in the top corners are once its middle has gone, in pixels: the mark and the way to
/// Settings, with a little air around them.
pub const HEADER_TAB: f32 = 56.;

/// The name beside the mark in the header. The mark stands for its first letter, so this is the rest of the word.
pub const NAME: &str = "eersus";

/// How long the mark and the name take to come, in milliseconds: the mark nods first, then the name is written beside it. They go
/// the same way back in the same time.
pub const NAME_WRITTEN_MS: u64 = 700;

/// The share of that time that is the mark's nod; the rest is the letters'.
pub const MARK_NODS: f32 = 0.3;

/// How long a name that was written unasked, in a window nobody is using, stays before it is rubbed out, in milliseconds.
pub const IDLE_NAME_HELD_MS: u64 = 2600;
// Held long enough to be read once written, and far shorter than any wait before the next (see `idle_name_after`).
const _: () = assert!(IDLE_NAME_HELD_MS >= NAME_WRITTEN_MS * 2 && IDLE_NAME_HELD_MS < 25_000);

/// The side of the mark in the header, in pixels.
pub const MARK: f32 = 44.;

/// As much of the name as is drawn when `shown` of it is (0 none, 1 all): whole letters from its start, as a word is written and
/// rubbed out, so that it needs no measuring and is never a letter cut in half. A share that is not a number is the whole name,
/// which is what a bar that knows nothing yet shows.
pub fn name_shown(shown: f32) -> &'static str {
    if !shown.is_finite() {
        return NAME;
    }
    let letters = (shown.clamp(0., 1.) * NAME.len() as f32).round() as usize;
    &NAME[..letters.min(NAME.len())]
}

/// How far the mark is into its nod (0 at rest, 1 at the deepest) when the mark and the name are `progress` of the way to written
/// (0 neither, 1 both): it nods over the first [`MARK_NODS`] of the way and is at rest for the rest. What is not a number is at rest.
pub fn mark_nod(progress: f32) -> f32 {
    if !progress.is_finite() {
        return 0.;
    }
    let through = (progress / MARK_NODS).clamp(0., 1.);
    (std::f32::consts::PI * through).sin().clamp(0., 1.)
}

/// How much of the name is written at that `progress`, for [`name_shown`]: none while the mark nods, then all the way.
pub fn name_part(progress: f32) -> f32 {
    if !progress.is_finite() {
        return progress;
    }
    ((progress - MARK_NODS) / (1. - MARK_NODS)).clamp(0., 1.)
}

/// How long a window nobody is using waits before it writes its name by itself for the `round`-th time, in milliseconds: between 25
/// and 90 seconds, and not the same from one time to the next, so that it is a sign of life and not a clock. `seed` is anything that
/// differs from one start to the next; the same seed gives the same waits, which is what lets this be tried.
pub fn idle_name_after(seed: u64, round: u32) -> u64 {
    const LEAST: u64 = 25_000;
    const MOST: u64 = 90_000;
    // SplitMix64: every bit of the round reaches every bit of the wait, so neighbouring rounds are not neighbouring waits.
    let mut mixed = seed.wrapping_add(u64::from(round).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^= mixed >> 31;
    LEAST + mixed % (MOST - LEAST + 1)
}

/// Whether the header is hidden as a page scrolls, so that a phone gives its whole screen to what is being read and the bar comes
/// back as soon as the person scrolls up. Scrolling only DECIDES: once the page has gone [`HeaderHide::TURN`] pixels one way, the bar
/// is to go (down) or to come back (up), and the bar then goes the whole way on its own clock, however far the finger goes after. A
/// tremble back and forth smaller than that decides nothing, and at the very top the bar is always there.
///
/// Pure, so it is tried with plain numbers and not through a window. `top` is how far the page is scrolled (0 at the very top).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeaderHide {
    /// Where the page was at the last look, in pixels from its top.
    last: f32,
    /// How far the page has gone in the direction it is going now, in pixels: down positive, up negative.
    travel: f32,
    /// Whether the bar is to be hidden.
    hidden: bool,
}

impl HeaderHide {
    /// How far the page must go one way before the bar turns, in pixels.
    pub const TURN: f32 = 12.;

    /// The state after the page is at `top` pixels from its start. At the very top the bar is wanted whatever came before (there is
    /// nothing above to read), a page that is not scrolled never hides it, and a position that is not a number changes nothing.
    pub fn after(self, top: f32) -> Self {
        if !top.is_finite() {
            return self;
        }
        let top = top.max(0.);
        if top <= 0. {
            return Self::default();
        }
        let moved = top - self.last;
        // A turn of the page's direction starts the count again from that move.
        let travel = if moved * self.travel > 0. {
            self.travel + moved
        } else {
            moved
        };
        let hidden = if travel >= Self::TURN {
            true
        } else if travel <= -Self::TURN {
            false
        } else {
            self.hidden
        };
        Self {
            last: top,
            travel,
            hidden,
        }
    }

    /// Whether the bar is to be hidden; how it gets there is the bar's own motion.
    pub fn hidden(self) -> bool {
        self.hidden
    }
}

/// The margins the system keeps free around the interface (the status bar above, the navigation bar below, a
/// notch at a side), in the toolkit's logical pixels. Nothing in it is a number written in this program: it is
/// what the system said, divided by the scale factor of the screen it is on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SafeArea {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl SafeArea {
    /// The margins for a screen of `width` by `height` physical pixels of which the system says only the rectangle
    /// `left, top, right, bottom` (physical pixels, `right` and `bottom` exclusive) is for the application,
    /// at `scale` physical pixels to a logical one.
    ///
    /// Nothing is free when the rectangle is empty (the system has not given one yet), when it is not inside
    /// the screen, when it is upside down, or when the scale is not a positive number: a wrong margin would
    /// cut the interface, and no margin cannot.
    pub fn from_physical(rectangle: (i32, i32, i32, i32), screen: (u32, u32), scale: f64) -> Self {
        let (left, top, right, bottom) = rectangle;
        let (width, height) = (screen.0 as i32, screen.1 as i32);
        let usable = scale.is_finite()
            && scale > 0.
            && right > left
            && bottom > top
            && left >= 0
            && top >= 0
            && right <= width
            && bottom <= height;
        if !usable {
            return Self::default();
        }
        let logical = |pixels: i32| (f64::from(pixels) / scale) as f32;
        Self {
            top: logical(top),
            left: logical(left),
            right: logical(width - right),
            bottom: logical(height - bottom),
        }
    }
}

/// A place that has a tab in the bottom bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BottomPlace {
    Send,
    Receive,
    Peers,
    Transfers,
}

/// How large one line of text is drawn in a box it does not choose the width of: as large as fits, between two sizes. The line
/// is never broken; below the smallest size it is cut instead. The width of a text is estimated from the number of its
/// characters, which is known before anything is laid out, with the share of its size a character of that face takes at its
/// widest; the estimate errs on the wide side, so that what it says fits does fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// The size when there is room to spare, in pixels.
    pub largest: f32,
    /// The size below which the text is no longer made smaller, and is left to be cut.
    pub smallest: f32,
    /// The width of a character, as a share of its size.
    pub em_per_character: f32,
}

impl Fit {
    /// The small capitals under a picture, in the monospace every label of that kind is set in: a place of the bottom bar.
    /// A monospace character is 0.6 of its size wide; the rest is the margin that keeps two captions apart.
    pub const CAPTION: Self = Self {
        largest: 11.,
        smallest: 8.,
        em_per_character: 0.64,
    };

    /// The width, in pixels, a line of `characters` takes at `size`.
    pub fn width_of(self, characters: usize, size: f32) -> f32 {
        characters as f32 * size * self.em_per_character
    }

    /// The largest size, up to [`Fit::largest`], at which a line of `characters` fits in `room`, and never under
    /// [`Fit::smallest`]. A room that is not known yet (nothing measured, so 0) gives the smallest: the text grows into its
    /// box when the box is known, and never starts outside it.
    pub fn size_in(self, room: f32, characters: usize) -> f32 {
        let fitting = room.max(0.) / (characters.max(1) as f32 * self.em_per_character);
        fitting.clamp(self.smallest, self.largest)
    }
}

/// How large a screen's title is drawn, from the room it has. It is one line, never broken: a title
/// that wraps puts its second line over the first, so it is made smaller instead.
pub struct Headline;

impl Headline {
    /// The size when there is room to spare, in pixels.
    pub const LARGEST: f32 = 76.;
    /// The size below which a title is no longer a title, and is left to be cut.
    pub const SMALLEST: f32 = 34.;
    /// The rule of a title. The width of a character of the bold face, as a share of its size, is at the widest of the
    /// words used: measured from the embedded font (4.6 to 5.7 em for ten characters) with a margin for the bold weight,
    /// which is a little wider than the one measured.
    const FIT: Fit = Fit {
        largest: Self::LARGEST,
        smallest: Self::SMALLEST,
        em_per_character: 0.62,
    };

    /// The width, in pixels, a line of `characters` takes at `size`.
    pub fn width_of(characters: usize, size: f32) -> f32 {
        Self::FIT.width_of(characters, size)
    }

    /// The largest size, up to [`Headline::LARGEST`], at which a line of `characters` fits in `room`.
    pub fn size_for(room: f32, characters: usize) -> f32 {
        Self::FIT.size_in(room, characters)
    }

    /// The largest a title may be on a screen of this form. The desktop's is [`Headline::LARGEST`], which is what a
    /// wide screen has room for. On a phone the same size is as wide as the screen, so every title fills it and the
    /// page is mostly title: it is held to a size that leaves the room to what the page is for.
    pub fn largest_in(form: Form) -> f32 {
        match form {
            Form::Wide => Self::LARGEST,
            Form::Compact | Form::Side => 40.,
        }
    }

    /// The size of a title of `characters` in `room`, on a screen of `form`: what fits, held to what the form allows,
    /// and never under [`Headline::SMALLEST`], so that a title is a title and is cut only when nothing else is left.
    pub fn size_in(form: Form, room: f32, characters: usize) -> f32 {
        Self::size_for(room, characters)
            .min(Self::largest_in(form))
            .max(Self::SMALLEST)
    }
}
