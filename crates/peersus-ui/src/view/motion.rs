//! How things move and appear: the life drawn around a code, how anything appears and leaves, the top sheet, the timing of a
//! move, and which pointer button presses. One definition for the whole application. Pure rules, no toolkit.
/// How long a code of this device lives, in seconds: what the owner gives an invitation.
pub const CODE_LIFE_SECONDS: u64 = 300;

/// The share of the life of a code that is left, from 1 (just shown) to 0 (ended), for a code shown at `made_at` that the owner ends at `ends_at`,
/// at `now`: all three in seconds on the same clock. The life is what the owner's own deadline gives it, never a number of the window's, so that
/// a code of any length of life burns down to the moment the owner ends it. A code whose deadline is not after the moment it was shown has no life.
pub fn code_fuse(made_at: u64, ends_at: u64, now: u64) -> f32 {
    let life = ends_at.saturating_sub(made_at);
    if life == 0 {
        return 0.;
    }
    (ends_at.saturating_sub(now) as f32 / life as f32).clamp(0., 1.)
}

/// Where the burning tip of the fuse is, for a share `left` of its life that is left (1 whole, 0 gone), on a ring of `radius` centred on
/// (`centre`, `centre`): it begins at the top and burns clockwise, so what is gone is the part the tip has already gone round.
pub fn fuse_tip(left: f32, radius: f32, centre: f32) -> (f32, f32) {
    let turn = std::f32::consts::TAU * (1. - left.clamp(0., 1.));
    (centre + radius * turn.sin(), centre - radius * turn.cos())
}

/// The part of the ring that is still lit, as the two numbers a drawn arc is made from, in degrees as Skia counts them (0 at three o'clock, growing
/// clockwise): where it starts and how far it goes. The fuse burns clockwise from the top, so what is left runs from the tip (where the burning has
/// got to) round to the top again: it starts at the tip and sweeps the share `left` of the whole turn. The same place as [`fuse_tip`] puts the tip,
/// and no other. `None` for a fuse that is gone (nothing to draw) or a number that is not one; a whole fuse sweeps the whole turn.
pub fn fuse_arc(left: f32) -> Option<(f32, f32)> {
    if !left.is_finite() || left <= 0. {
        return None;
    }
    let left = left.min(1.);
    // The top of the ring is -90 degrees. The burnt part is from the top to the tip, so the lit part begins where that ends.
    Some((-90. + 360. * (1. - left), 360. * left))
}

/// Whether the point (`x`, `y`) lies on the disc of radius `disc` about (`centre`, `centre`), or within `margin` of it: the disc holds the code, and
/// nothing of the fuse may be drawn where this is true, so that a spark that flew inwards is dropped and the code is never covered.
pub fn point_on_disc(x: f32, y: f32, centre: f32, disc: f32, margin: f32) -> bool {
    (x - centre).hypot(y - centre) <= disc + margin
}

/// How many particles are born at the tip of the fuse for each frame, and how many frames the longest of them lives: as in the reference, in
/// which four are born for each frame and each fades by its own part of one until it is gone.
pub const FUSE_BORN_PER_FRAME: usize = 4;
pub const FUSE_PARTICLE_FRAMES: u32 = 66;

/// One particle of the sparks, as offsets in pixels from the tip it was born at, its size, its brightness (1 just born, 0 gone), its colour as
/// (red, green, blue) and how many frames ago it was born.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub alpha: f32,
    pub colour: (u8, u8, u8),
    pub age: u32,
}

/// A number between 0 and 1 from `seed`, the same each time: a hash, so that what looks random is a function and can be tested.
fn hashed(seed: u64) -> f32 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// The live particles at `frame`: the ones born at the tip in each of the frames before it and still not faded out. Each has its own direction over
/// the whole circle, its own speed, a size and one colour of gold, orange, white and red; gravity pulls it down as it goes, and it fades by its
/// own part of one for each frame until it is gone. Everything is a function of the frame number, so that the same frame gives the same
/// particles, and `age` is how many frames ago it was born (0 is this frame's, on the tip).
pub fn fuse_particles(frame: u32) -> Vec<Particle> {
    const GRAVITY: f32 = 0.04;
    const COLOURS: [(u8, u8, u8); 4] =
        [(255, 230, 0), (255, 136, 0), (255, 255, 255), (255, 51, 0)];
    let mut live = Vec::new();
    for age in 0..FUSE_PARTICLE_FRAMES.min(frame + 1) {
        let born = frame - age;
        for child in 0..FUSE_BORN_PER_FRAME {
            let seed = u64::from(born) * 16 + child as u64;
            let angle = hashed(seed) * std::f32::consts::TAU;
            let speed = hashed(seed ^ 0xA5A5) * 2.5 + 0.5;
            let decay = hashed(seed ^ 0x5A5A) * 0.03 + 0.015;
            let size = hashed(seed ^ 0x3C3C) * 2.5 + 1.;
            let alpha = 1. - decay * age as f32;
            if alpha <= 0. {
                continue;
            }
            let colour = COLOURS[(hashed(seed ^ 0xC3C3) * 4.) as usize % 4];
            let t = age as f32;
            live.push(Particle {
                x: angle.cos() * speed * t,
                y: angle.sin() * speed * t + 0.5 * GRAVITY * t * t,
                size,
                alpha,
                colour,
                age,
            });
        }
    }
    live
}

/// How anything that appears on screen appears: it fades in while growing from a little smaller
/// and rising the last few pixels into its place, and it leaves the same way backwards. One
/// definition for the whole application, so that the menu, the tabs, a page and a dialog all move
/// alike and nothing is ever just there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Popup {
    /// From 0 (not there) to 1 (whole).
    pub opacity: f32,
    /// Pixels it is still below its place; 0 once it has arrived.
    pub lift: f32,
    /// Its size as a share of its full size, growing to 1.
    pub scale: f32,
}

impl Popup {
    /// How far below its place a thing starts, in pixels.
    pub const RISE: f32 = 12.;
    /// The share of its size a thing starts from.
    pub const SMALLEST: f32 = 0.85;
    /// How long it takes, in milliseconds.
    pub const DURATION_MS: u64 = 180;

    /// Where it is when the animation is `progress` of the way (0 to 1). The curve starts fast
    /// and ends slowly (cubic ease-out), the way things settle in the physical world.
    pub fn at(progress: f32) -> Popup {
        let t = progress.clamp(0., 1.);
        let eased = 1. - (1. - t).powi(3);
        Popup {
            opacity: eased,
            lift: Self::RISE * (1. - eased),
            scale: Self::SMALLEST + (1. - Self::SMALLEST) * eased,
        }
    }

    /// Whole and in place: what a thing is once it has arrived, or always when motion is reduced.
    pub fn still() -> Popup {
        Popup {
            opacity: 1.,
            lift: 0.,
            scale: 1.,
        }
    }
}

/// The sheet that comes down from the top of a phone's screen and holds the places of Settings. It moves with the same ease as a popup:
/// the sheet is placed by the distance of its top edge from the top of the usable area, and a dark layer under it grows as it comes.
pub struct TopSheet;

impl TopSheet {
    /// The darkest the layer under the sheet gets, as the alpha of black (of 255): the page is still seen under it.
    pub const SCRIM: u8 = 170;

    /// How strongly the page under the open sheet is blurred, in pixels of radius.
    pub const BLUR: f32 = 14.;

    /// Where the top edge of a sheet `height` tall is when it is `progress` (0 to 1) of the way down, in a usable area that begins `top` from
    /// the top of the window: wholly above the area when closed, in its place when open, and never beyond either.
    pub fn top(progress: f32, height: f32, top: f32) -> f32 {
        let eased = 1. - (1. - progress.clamp(0., 1.)).powi(3);
        top - (1. - eased) * height
    }

    /// The alpha of the layer under the sheet at `progress`: nothing when closed, `SCRIM` when open.
    pub fn scrim(progress: f32) -> u8 {
        (f32::from(Self::SCRIM) * progress.clamp(0., 1.)).round() as u8
    }

    /// The height of the sheet in `room` pixels of usable height: all of it, down to the bar of places, never negative. It is closed by
    /// its Close, Back, or the gesture that opened it.
    pub fn height(room: f32) -> f32 {
        room.max(0.)
    }
}

/// How long a move takes and when a thing that is going away may finally be taken off the screen.
/// A move is a new animation from wherever the thing is now to where it is going (never a
/// reversal, which would start by jumping to the far end), so one interrupted halfway turns round
/// from that very spot, at the same speed, and nothing ever jumps.
pub struct Motion;

impl Motion {
    /// The time to travel the whole way, in milliseconds.
    pub const DURATION_MS: u64 = Popup::DURATION_MS;

    /// The time to go from `from` to `to` (both 0 to 1): in proportion to the distance, so a turn
    /// in the middle is quick and the speed is always the same. None at all with reduced motion.
    pub fn duration_ms(from: f32, to: f32, reduced_motion: bool) -> u64 {
        if reduced_motion {
            return 0;
        }
        (((to - from).abs() * Self::DURATION_MS as f32).round()) as u64
    }

    /// Whether a thing is on the screen: while it is wanted, and for as long as it has not yet
    /// finished going away. Taking it off earlier would make it disappear at once.
    pub fn mounted(wanted: bool, progress: f32) -> bool {
        wanted || progress > 0.
    }
}

impl Motion {
    /// The progress to start the next move from: where the thing is now, as it was last drawn,
    /// kept within its two ends. Never the end the last move was heading for, which would be a jump.
    pub fn start_from(last_drawn: f32) -> f32 {
        last_drawn.clamp(0., 1.)
    }

    /// Whether a thing that is going toward `target` has arrived, to within a hair.
    pub fn arrived(progress: f32, target: f32) -> bool {
        (progress - target).abs() < 1e-3
    }
}

/// The button of a mouse a press came from, as far as a press cares: the first one, or any other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    First,
    Other,
}

/// Whether the lifting of a pointer is a press of what is under it. A finger always is (`None`: no mouse); of a mouse only its first button
/// is, so that the button which opens a menu does not also do what a click does. A mouse that names no button is taken as its first.
pub fn presses(mouse: Option<Option<PointerButton>>) -> bool {
    !matches!(mouse, Some(Some(PointerButton::Other)))
}
