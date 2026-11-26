//! Development-only private catalog setup, shared by desktop and Android.
use crate::{i18n::tr, link::Link, theme::Palette};
use freya::prelude::*;
use peersus_core::{model::SecretText, private_store::PrivateCatalog};
use zeroize::Zeroize;

#[derive(PartialEq)]
pub struct Configuration {
    pub link: Link,
    pub palette: Palette,
}

impl Component for Configuration {
    fn render(&self) -> impl IntoElement {
        let mut opened = use_state(|| false);
        let mut token = use_state(String::new);
        let mut busy = use_state(|| false);
        let mut status = use_state(|| None::<String>);
        let mut block = rect().width(Size::fill()).spacing(8.).child(
            Button::new()
                .on_press(move |_| {
                    if !busy() {
                        token.write().zeroize();
                        opened.set(!opened());
                        status.set(None);
                    }
                })
                .child(tr("Private development catalog")),
        );
        if opened() {
            block = block.child(label().text(tr("Enter your read-only GitHub token. No other settings are needed here; the token stays in secure device storage."))
                .color(self.palette.muted).font_size(13.));
            let link = self.link.clone();
            block = block.child(crate::field::field(crate::field::Profile::SECRET, token)
                .placeholder(tr("GitHub token with repository Contents: read")).width(Size::fill()))
                .child(rect().horizontal().spacing(8.)
                    .child(Button::new().enabled(!busy()).on_press(move |_| {
                        // The development catalog key (its seed is a local, ignored file); no product build holds it.
                        let key = [
                            18, 166, 4, 59, 248, 80, 183, 87, 204, 114, 191, 16, 53, 156, 137, 127,
                            22, 191, 111, 8, 216, 244, 231, 67, 173, 102, 242, 24, 221, 40, 51, 107,
                        ];
                        let Some(secret) = SecretText::new(&token.read()) else {
                            status.set(Some("The GitHub token must be a single line of at most 512 bytes".into()));
                            return;
                        };
                        let source = PrivateCatalog {
                            repository: "axxa-dev/Peersus".to_owned(),
                            directory: "extension".to_owned(),
                            revision: "extension".to_owned(), token: secret, public_key: key,
                        };
                        if let Err(words) = source.validate() {
                            status.set(Some(words.into()));
                            return;
                        }
                        token.write().zeroize();
                        busy.set(true);
                        status.set(None);
                        let link = link.clone();
                        spawn(async move {
                            match link.configure_private_store(source).await {
                                Ok(()) => {
                                    status.set(Some("Token saved securely and applied. The extension listing is updating in the background.".into()));
                                }
                                Err(words) => status.set(Some(words)),
                            }
                            busy.set(false);
                        });
                    }).child(tr("Save and apply")))
                    .child(Button::new().enabled(!busy()).on_press(move |_| {
                        token.write().zeroize();
                        opened.set(false);
                        status.set(None);
                    }).child(tr("Cancel"))));
            // English sentences (its own and the owner's) until they are drawn.
            if let Some(words) = status.read().as_ref() {
                block = block.child(
                    label()
                        .text(tr(words).to_owned())
                        .color(self.palette.muted)
                        .font_size(13.),
                );
            }
        }
        block
    }
}
