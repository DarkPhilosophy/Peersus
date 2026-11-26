//! Optional startup of the account's owner, separate from keeping a window-started owner alive.
//! Registration is read back from the OS after each change, not inferred from the switch.
use crate::{
    i18n,
    link::Link,
    theme::{Palette, state_border},
};
use freya::prelude::*;
use peersus_app::service::{Manager, Status};

const NO_ANSWER: &str = "The system's user service manager could not be asked.";
const NO_PROGRAM: &str = "The program to start was not found next to this window.";

#[derive(PartialEq)]
pub(crate) struct StartupRow {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
}

impl Component for StartupRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let link = self.link.clone();
        let manager = Manager::detect();
        let owner = crate::link::owner_program();
        let mut status: State<Option<Result<Status, String>>> = use_state(|| None);
        let mut note: State<Option<String>> = use_state(|| None);
        let mut busy: State<bool> = use_state(|| false);

        use_hook({
            let link = link.clone();
            let manager = manager.as_ref().ok().cloned();
            move || {
                let Some(manager) = manager else {
                    return;
                };
                let link = link.clone();
                spawn(async move {
                    let answer = link
                        .blocking({
                            let manager = manager.clone();
                            move || manager.status()
                        })
                        .await;
                    status.set(Some(match answer {
                        Ok(result) => result,
                        Err(_) => Err(NO_ANSWER.to_owned()),
                    }));
                });
            }
        });

        let toggle = {
            let link = link.clone();
            let manager = manager.as_ref().ok().cloned();
            let owner = owner.clone();
            move |value: bool| {
                let Some(manager) = manager.clone() else {
                    return;
                };
                if link.read_only() || *busy.read() {
                    return;
                }
                busy.set(true);
                note.set(None);
                let link = link.clone();
                let owner = owner.clone();
                spawn(async move {
                    let job = {
                        let manager = manager.clone();
                        move || {
                            if value {
                                match owner {
                                    Some(owner) => manager.enable(&owner),
                                    None => Err(NO_PROGRAM.to_owned()),
                                }
                            } else {
                                manager.disable()
                            }
                        }
                    };
                    match link.blocking(job).await {
                        Ok(Ok(())) => {}
                        Ok(Err(reason)) => note.set(Some(reason)),
                        Err(_) => note.set(Some(NO_ANSWER.to_owned())),
                    }
                    // Ask the system again at once, so the switch shows what it did and not what was asked.
                    status.set(Some(
                        link.blocking(move || manager.status())
                            .await
                            .unwrap_or_else(|_| Err(NO_ANSWER.to_owned())),
                    ));
                    busy.set(false);
                });
            }
        };

        let current = status.read();
        let on = current
            .as_ref()
            .and_then(|answer| answer.as_ref().ok())
            .is_some_and(|status| status.enabled);
        #[cfg(target_os = "macos")]
        let pending = current
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .is_some_and(|s| s.requires_approval);
        #[cfg(not(target_os = "macos"))]
        let pending = false;
        let available = current.as_ref().is_some_and(Result::is_ok);
        let state_line = if manager.is_err() {
            None
        } else {
            Some(match current.as_ref() {
                None => i18n::tr("Checking whether the system starts Peersus with your session.").to_owned(),
                Some(Err(reason)) => i18n::tr(reason).to_owned(),
                #[cfg(target_os = "linux")]
                Some(Ok(status)) => match (status.enabled, status.running) {
                    (true, true) => i18n::tr("The system will start it with your session, and it is running now."),
                    (true, false) => i18n::tr("The system will start it with your session; it is not running now."),
                    (false, true) => i18n::tr("The system will not start it with your session; it is running now until you sign out."),
                    (false, false) => i18n::tr("The system will not start it with your session, and it is not running now."),
                }
                .to_owned(),
                #[cfg(windows)]
                Some(Ok(_)) => i18n::tr("Windows applies this account's choice at the next sign-in. No service instance runs for a signed-out account.").to_owned(),
                #[cfg(target_os = "macos")]
                Some(Ok(status)) => if status.requires_approval {
                    i18n::tr("Approval is required in System Settings → General → Login Items. No connection has been confirmed.")
                } else {
                    i18n::tr("This user service runs only in your signed-in macOS session. It does not control the login screen.")
                }.to_owned(),
            })
        };
        #[cfg(target_os = "linux")]
        let linger_line = match current.as_ref() {
            Some(Ok(status)) if manager.is_ok() && status.enabled => Some(match status.lingering {
                Some(true) => i18n::tr(
                    "Your session runs before you sign in (lingering). The keyring is locked until you sign in, so Peersus waits and does not connect before then.",
                ),
                Some(false) => i18n::tr(
                    "Your session starts when you sign in, so this starts then, not when the computer starts. Peersus does not turn lingering on.",
                ),
                None => i18n::tr("Whether your session runs before you sign in could not be read."),
            }),
            _ => None,
        };
        #[cfg(not(target_os = "linux"))]
        let linger_line: Option<&str> = None;
        let note = note.read().clone();
        let note_line = note.as_deref().map(|line| i18n::tr(line).to_owned());
        let reason = manager
            .as_ref()
            .err()
            .map(|reason| i18n::tr(reason).to_owned());
        let note = match reason {
            Some(reason) => Some(reason),
            None if !available || pending => None,
            None if on => Some(i18n::tr("On: Peersus starts with your session and keeps running without a window, so your devices can reach it. This is separate from Run in the background.").to_owned()),
            None => Some(i18n::tr("Off: Peersus starts when you open it. This is separate from Run in the background.").to_owned()),
        };

        let mut card = rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(state_border(palette, on))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(
                        label()
                            .text(i18n::tr("Start when you sign in"))
                            .color(palette.text)
                            .font_size(16.)
                            .font_weight(FontWeight::BOLD),
                    )
                    .maybe_child(
                        note.map(|line| label().text(line).color(palette.muted).font_size(13.)),
                    )
                    .maybe_child(
                        state_line
                            .map(|line| label().text(line).color(palette.muted).font_size(13.)),
                    )
                    .maybe_child(
                        linger_line
                            .map(|line| label().text(line).color(palette.muted).font_size(13.)),
                    )
                    .maybe_child(
                        note_line
                            .map(|line| label().text(line).color(palette.warning).font_size(13.)),
                    ),
            );
        if available {
            card = card.child(
                Switch::new(WritableSetter::new(on || pending, toggle))
                    .enabled(!*busy.read() && !link.read_only()),
            );
        }
        card
    }
}
