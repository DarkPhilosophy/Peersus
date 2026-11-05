//! Development-only private catalog setup, shared by desktop and Android.
use crate::{link::Link, theme::Palette};
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
                .child("Private development catalog"),
        );
        if opened() {
            block = block.child(label().text("Enter your read-only GitHub token. No other settings are needed here; the token stays in secure device storage.")
                .color(self.palette.muted).font_size(13.));
            let link = self.link.clone();
            block = block.child(crate::field::field(crate::field::Profile::SECRET, token)
                .placeholder("GitHub token with repository Contents: read").width(Size::fill()))
                .child(rect().horizontal().spacing(8.)
                    .child(Button::new().enabled(!busy()).on_press(move |_| {
                        let key = [
                            0xea, 0x4a, 0x6c, 0x63, 0xe2, 0x9c, 0x52, 0x0a,
                            0xbe, 0xf5, 0x50, 0x7b, 0x13, 0x2e, 0xc5, 0xf9,
                            0x95, 0x47, 0x76, 0xae, 0xbe, 0xbe, 0x7b, 0x92,
                            0x42, 0x1e, 0xea, 0x69, 0x14, 0x46, 0xd2, 0x2c,
                        ];
                        let Some(secret) = SecretText::new(&token.read()) else {
                            status.set(Some("The GitHub token must be a single line of at most 512 bytes".into()));
                            return;
                        };
                        let source = PrivateCatalog {
                            repository: "DarkPhilosophy/Peersus".to_owned(),
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
                    }).child("Save and apply"))
                    .child(Button::new().enabled(!busy()).on_press(move |_| {
                        token.write().zeroize();
                        opened.set(false);
                        status.set(None);
                    }).child("Cancel")));
            if let Some(words) = status.read().as_ref() {
                block = block.child(
                    label()
                        .text(words.clone())
                        .color(self.palette.muted)
                        .font_size(13.),
                );
            }
        }
        block
    }
}
