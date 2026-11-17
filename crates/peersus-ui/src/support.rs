//! Voluntary support, separate from dependency credits and all product entitlements.
//! Opening About is inert: only a deliberate button press opens the external provider.
use crate::{i18n, theme::Palette};
use freya::{animation::*, prelude::*};

/// The creator handle is also declared by `.github/FUNDING.yml`.
pub const KO_FI_URL: &str = "https://ko-fi.com/DarkPhilosophy";
pub const TITLE: &str = "Support Peersus — entirely optional";
pub const DESCRIPTION: &str = "If Peersus is useful to you, you can leave a voluntary tip. All app functions remain free, with no ads, paid features or priority access.";
pub const PAYMENT_NOTE: &str = "Card payments use the creator's linked Stripe account where offered by Ko-fi; payment methods vary. Peersus does not handle your card details.";
pub const TERMS: &str = "A tip buys no product, service, access or reward. Processor fees may apply. No charitable tax-deductibility is promised. Public thanks is a separate, optional choice on the website, never an in-app benefit.";

/// An unobtrusive About-only block. No provider SDK, startup work, or payment state.
#[derive(PartialEq)]
pub struct Support {
    pub palette: Palette,
    /// The person's "reduce motion" choice: the button keeps its colours but stops moving.
    pub reduced: bool,
}

/// The colours the support button's sheen travels through.
const SHEEN: [(u8, u8, u8); 4] = [
    (255, 94, 91),
    (255, 170, 64),
    (186, 104, 255),
    (255, 94, 91),
];

impl Component for Support {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let opener = try_consume_context::<crate::link::UrlOpener>();
        let mut unavailable = use_state(|| false);
        // A soft light passes across the button now and then. Between passes, and whenever the button is off screen or motion is
        // reduced, nothing moves and nothing is redrawn.
        let mut visible = use_state(|| false);
        let shine = use_animation(|_| AnimNum::new(0., 1.).time(1500).function(Function::Linear));
        let reduced = self.reduced;
        // One loop at most: the one started for the last change is cancelled before another starts, and when the button leaves the
        // screen or motion is reduced none runs.
        let mut looping: State<Option<TaskHandle>> = use_state(|| None);
        use_side_effect_with_deps(&(reduced, visible()), move |&(reduced, shown)| {
            if let Some(task) = looping.write().take() {
                task.cancel();
            }
            if reduced || !shown {
                return;
            }
            let mut shine = shine;
            looping.set(Some(spawn(async move {
                loop {
                    shine.reset();
                    shine.start();
                    timer(std::time::Duration::from_millis(4200)).await;
                }
            })));
        });
        use_drop(move || {
            if let Some(task) = looping.write().take() {
                task.cancel();
            }
        });
        let phase = if reduced { 1. } else { shine.get().value() };
        let band = (phase < 1.).then(|| -25. + phase * 150.);
        let block = rect()
            .width(Size::fill())
            .padding(18.)
            .spacing(12.)
            .corner_radius(14.)
            .background(palette.surface)
            .child(
                label()
                    .text(i18n::tr(TITLE))
                    .color(palette.text)
                    .font_size(16.)
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                label()
                    .text(i18n::tr(DESCRIPTION))
                    .color(palette.text)
                    .font_size(14.),
            );
        // The button's colours, with the light drawn into the same gradient: a brighter band of those colours travels across.
        let colour_at = |at: f32| {
            let at = at.clamp(0., 100.) / 50.;
            let i = (at.floor() as usize).min(1);
            let t = at - i as f32;
            let (a, b) = (SHEEN[i], SHEEN[i + 1]);
            let mix = |x: u8, y: u8| x as f32 + (y as f32 - x as f32) * t;
            (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
        };
        let rgb = |(r, g, b): (f32, f32, f32), lift: f32| {
            let up = |c: f32| (c + (255. - c) * lift).round() as u8;
            Color::from_rgb(up(r), up(g), up(b))
        };
        let mut stops: Vec<(f32, Color)> = (0..3)
            .map(|i| (i as f32 * 50., rgb(colour_at(i as f32 * 50.), 0.)))
            .collect();
        if let Some(at) = band {
            const WIDTH: f32 = 14.;
            stops.retain(|(position, _)| (*position - at).abs() >= WIDTH);
            for (position, lift) in [(at - WIDTH, 0.), (at, 0.6), (at + WIDTH, 0.)] {
                if (0. ..=100.).contains(&position) {
                    stops.push((position, rgb(colour_at(position), lift)));
                }
            }
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let base = stops.into_iter().fold(
            LinearGradient::new().angle(100.),
            |gradient, (position, colour)| gradient.stop((colour, position)),
        );
        let block = block.child(
            rect()
                .width(Size::fill())
                .padding((14., 18.))
                .corner_radius(14.)
                .main_align(Alignment::center())
                .cross_align(Alignment::center())
                .background(base)
                .on_visible(move |_| visible.set(true))
                .on_hidden(move |_| visible.set(false))
                .shadow(
                    Shadow::new()
                        .blur(10.)
                        .spread(1.)
                        .color(Color::from_argb(80, 255, 120, 140)),
                )
                .on_press(move |_| {
                    if let Some(opener) = &opener {
                        unavailable.set(!opener.open(KO_FI_URL));
                        return;
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        Platform::get().open_url(KO_FI_URL);
                        unavailable.set(false);
                    }
                    #[cfg(target_os = "android")]
                    unavailable.set(true);
                })
                .child(
                    label()
                        .text(format!(
                            "☕  {}",
                            i18n::tr("Support on Ko-fi (opens browser)")
                        ))
                        .color(Color::WHITE)
                        .font_size(16.)
                        .font_weight(FontWeight::BOLD)
                        // The label spans the button, so centred text is centred in the button, on one line or two.
                        .width(Size::fill())
                        .text_align(TextAlign::Center),
                ),
        );
        let block = if unavailable() {
            block.child(label()
                .text(i18n::tr("Could not open a browser. You can open https://ko-fi.com/DarkPhilosophy manually."))
                .color(palette.bad).font_size(13.))
        } else {
            block
        };
        block
            .child(
                label()
                    .text(i18n::tr(PAYMENT_NOTE))
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(
                label()
                    .text(i18n::tr(TERMS))
                    .color(palette.muted)
                    .font_size(13.),
            )
    }
}
