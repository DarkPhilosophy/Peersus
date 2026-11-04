//! Voluntary support, separate from dependency credits and all product entitlements.
//! Opening About is inert: only a deliberate button press opens the external provider.
use crate::{i18n, theme::Palette};
use freya::prelude::*;

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
}

impl Component for Support {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let opener = try_consume_context::<crate::link::UrlOpener>();
        let mut unavailable = use_state(|| false);
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
        let block = block.child(
            Button::new()
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
                .child(i18n::tr("Support on Ko-fi (opens browser)")),
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
