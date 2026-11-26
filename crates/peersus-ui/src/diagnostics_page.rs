//! The Diagnostics page of Settings (desktop): crash capture, switched on or off, how it stands for the owner and for this
//! window, and the crash reports it wrote, with a way to open their folder and to delete them.
//!
//! Capture itself is `peersus_diagnostics::crash_watch`: each program starts `peersus-diag` beside it. The owner reports its
//! own in the snapshot; this window keeps its own here ([`use_window_capture`]), since it is a process of its own.
use crate::{i18n, link::Link, theme::Palette, view};
use freya::prelude::*;
use peersus_core::model::{CaptureState, Command, DiagnosticsState};

/// How capture of this window stands, for the page.
#[derive(Clone, Copy)]
pub(crate) struct WindowCapture(pub State<CaptureState>);

/// Keeps this window's crash capture as the `crash_capture` preference says, and provides how it stands. Starting and
/// stopping happen on a thread of their own; the window never waits for them.
pub(crate) fn use_window_capture(wanted: bool) {
    let status = use_state(|| CaptureState::Off);
    provide_context(WindowCapture(status));
    #[cfg(not(target_os = "android"))]
    {
        let controller = use_hook(move || {
            let folder = peersus_app::instance::data_directory()
                .map(|data| peersus_diagnostics::crash_watch::folder(&data))?;
            let (sender, mut changes) =
                tokio::sync::watch::channel(peersus_diagnostics::crash_watch::Status::Off);
            let controller = std::rc::Rc::new(peersus_diagnostics::crash_watch::Controller::new(
                "peersus-ui",
                folder,
                move |next| {
                    let _ = sender.send(next);
                },
            ));
            let mut status = status;
            spawn(async move {
                while changes.changed().await.is_ok() {
                    let next = *changes.borrow_and_update();
                    status.set_if_modified(peersus_app::crash_capture::state(next));
                }
            });
            Some(controller)
        });
        use_side_effect_with_deps(&wanted, move |on| {
            if let Some(controller) = &controller {
                controller.set(*on);
            }
        });
    }
    #[cfg(target_os = "android")]
    let _ = wanted;
}

pub(crate) const TITLE: &str = "Capture crashes";
pub(crate) const NOTE: &str = "When Peersus or one of its windows crashes, a small program beside it writes a crash report (a minidump) to this device's diagnostics folder. Nothing is sent anywhere. A report holds what the program had in memory, so share it only with someone you trust. Off: nothing watches Peersus.";
const PRIVACY: &str = "Reports stay on this device until you delete them. The newest ten are kept.";
const OPEN_FOLDER: &str = "Open folder";
const DELETE: &str = "Delete reports";
const NONE: &str = "No crash reports.";

/// What the person reads for how capture of one program stands.
fn status_words(state: CaptureState) -> &'static str {
    match state {
        CaptureState::Off => "Off",
        CaptureState::Starting => "Starting…",
        CaptureState::Watching => "Watching",
        CaptureState::NotThisSystem => "Crash capture is available on Linux only",
        CaptureState::Missing => "The crash capture program is not installed beside Peersus",
        CaptureState::NotStarted => "The crash capture program did not start",
        CaptureState::Refused => {
            "The system does not allow crash capture (ptrace is restricted, or a debugger is attached)"
        }
    }
}

/// A size in the units a person reads.
fn size_words(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KiB", bytes.div_ceil(KIB))
    }
}

#[derive(PartialEq)]
pub struct DiagnosticsPage {
    pub link: Link,
    pub palette: Palette,
    pub state: DiagnosticsState,
}

impl Component for DiagnosticsPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // The folder is read again each time the page is shown: a window that crashed left a report the owner did not see.
        let refresh = self.link.clone();
        use_hook(move || refresh.send(Command::RefreshDiagnostics {}));
        let window = try_consume_context::<WindowCapture>()
            .map_or(CaptureState::Off, |capture| *capture.0.read());
        let fact = |name: &'static str, value: &'static str| -> Element {
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::flex())
                .spacing(12.)
                .child(
                    label()
                        .text(i18n::tr(name))
                        .color(palette.muted)
                        .font_size(14.)
                        .width(Size::flex(1.)),
                )
                .child(
                    label()
                        .text(i18n::tr(value))
                        .color(palette.text)
                        .font_size(14.)
                        .max_width(Size::percent(60.)),
                )
                .into()
        };
        let mut reports = card(palette).child(
            label()
                .text(i18n::tr("Crash reports"))
                .color(palette.text)
                .font_size(16.)
                .font_weight(FontWeight::BOLD),
        );
        if self.state.reports.is_empty() {
            reports = reports.child(
                label()
                    .text(i18n::tr(NONE))
                    .color(palette.muted)
                    .font_size(13.),
            );
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let language = i18n::language();
        for report in &self.state.reports {
            reports = reports.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(12.)
                    .child(
                        crate::text::caption(report.file.clone())
                            .width(Size::flex(1.))
                            .text_overflow(TextOverflow::Ellipsis)
                            .color(palette.text)
                            .font_size(13.),
                    )
                    .child(
                        label()
                            .text(format!(
                                "{} · {}",
                                view::seen_words_in(language, Some(report.unix_seconds), now),
                                size_words(report.bytes)
                            ))
                            .color(palette.muted)
                            .font_size(13.),
                    ),
            );
        }
        let mut buttons = rect().width(Size::fill()).horizontal().spacing(8.);
        if let Some(folder) = self.state.folder.clone() {
            let opener = try_consume_context::<crate::link::UrlOpener>();
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        let url = format!("file://{}", folder.display());
                        match &opener {
                            Some(opener) => {
                                let _ = opener.open(&url);
                            }
                            #[cfg(not(target_os = "android"))]
                            None => Platform::get().open_url(url),
                            #[cfg(target_os = "android")]
                            None => {}
                        }
                    })
                    .child(crate::text::caption(i18n::tr(OPEN_FOLDER).to_owned())),
            );
        }
        if !self.state.reports.is_empty() {
            let link = self.link.clone();
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| link.send(Command::DeleteCrashReports {}))
                    .child(crate::text::caption(i18n::tr(DELETE).to_owned())),
            );
        }
        rect()
            .width(Size::fill())
            .spacing(14.)
            .child(
                card(palette)
                    .child(fact("Peersus core", status_words(self.state.owner)))
                    .child(fact("This window", status_words(window))),
            )
            .child(
                reports
                    .child(
                        label()
                            .text(i18n::tr(PRIVACY))
                            .color(palette.muted)
                            .font_size(12.),
                    )
                    .child(buttons),
            )
    }
}

fn card(palette: Palette) -> Rect {
    rect()
        .width(Size::fill())
        .padding(18.)
        .spacing(8.)
        .corner_radius(12.)
        .background(palette.surface)
        .border(
            Border::new()
                .fill(palette.border)
                .width(1.)
                .alignment(BorderAlignment::Inner),
        )
}

/// What the Diagnostics page gives Settings search.
pub fn search(index: &mut crate::settings_index::Index) {
    index.add(TITLE, index.tr(NOTE));
    let actions = [OPEN_FOLDER, DELETE]
        .iter()
        .map(|label| index.tr(label))
        .collect::<Vec<_>>()
        .join(" · ");
    index.add("Crash reports", actions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_kibibytes_then_mebibytes() {
        assert_eq!(size_words(1), "1 KiB");
        assert_eq!(size_words(1024), "1 KiB");
        assert_eq!(size_words(1025), "2 KiB");
        assert_eq!(size_words(3 * 1024 * 1024 + 512 * 1024), "3.5 MiB");
    }

    #[test]
    fn every_capture_state_has_its_own_sentence() {
        let all = [
            CaptureState::Off,
            CaptureState::Starting,
            CaptureState::Watching,
            CaptureState::NotThisSystem,
            CaptureState::Missing,
            CaptureState::NotStarted,
            CaptureState::Refused,
        ];
        let words: std::collections::BTreeSet<&str> =
            all.iter().map(|state| status_words(*state)).collect();
        assert_eq!(words.len(), all.len());
    }
}
