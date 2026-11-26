//! Appearance controls for the saved graphics preference and the native window's live report.
use crate::{i18n, link::Link, settings_index::Index, text, theme::Palette};
use freya::prelude::*;
use freya_winit::config::{RendererBackend as Backend, RendererFailure, RendererStatus};
use peersus_core::config::{Renderer, SettingsPatch};

const CHOICES: [Renderer; 4] = [
    Renderer::Auto,
    Renderer::Vulkan,
    Renderer::OpenGl,
    Renderer::Software,
];
const TITLE: &str = "Graphics renderer";
const COMPARE: &str = "Compare renderers";

pub(crate) const fn preference(value: Renderer) -> RendererPreference {
    match value {
        Renderer::Auto => RendererPreference::Auto,
        Renderer::Vulkan => Backend::Vulkan.preference(),
        Renderer::OpenGl => Backend::OpenGl.preference(),
        Renderer::Software => Backend::Software.preference(),
    }
}

fn name_in(backend: Backend, language: i18n::Language) -> &'static str {
    match backend {
        Backend::OpenGl if cfg!(target_os = "android") => "OpenGL ES",
        Backend::Software => i18n::translate(language, backend.name()),
        _ => backend.name(),
    }
}

fn name(backend: Backend) -> &'static str {
    name_in(backend, i18n::language())
}

fn choice_name(choice: Renderer) -> &'static str {
    preference(choice)
        .backend()
        .map_or_else(|| i18n::tr("Automatic"), name)
}

fn compiled(choice: Renderer) -> bool {
    preference(choice)
        .backend()
        .is_none_or(|backend| Backend::compiled().contains(&backend))
}

pub(crate) fn search(index: &mut Index) {
    let names = Backend::compiled()
        .iter()
        .map(|&backend| name_in(backend, index.language()))
        .collect::<Vec<_>>()
        .join(" · ");
    index.add(TITLE, names);
    if !cfg!(target_os = "android") {
        index.add(COMPARE, index.tr("Frame time, CPU and memory"));
    }
}

fn words(message: &str, field: &str, value: &str) -> String {
    i18n::format(i18n::language(), message, &[(field, value)])
}

fn note(text: impl Into<std::borrow::Cow<'static, str>>, palette: Palette) -> Label {
    label().text(text).color(palette.muted).font_size(13.)
}

fn startup_notes(status: &RendererStatus) -> impl Iterator<Item = (&'static str, Backend)> + '_ {
    status
        .environment_override
        .map(|backend| ("A launch override requested {renderer}.", backend))
        .into_iter()
        .chain(status.startup_failures.iter().map(|&(backend, failure)| {
            let message = match failure {
                RendererFailure::NotCompiled => "{renderer} is not included in this build.",
                RendererFailure::Initialization => "{renderer} could not start on this display.",
                RendererFailure::BasicOpenGl => {
                    "Automatic selection skipped the basic {renderer} software adapter."
                }
            };
            (message, backend)
        }))
        .chain(status.recovered_from.map(|backend| {
            (
                "{renderer} stopped working while this window was open.",
                backend,
            )
        }))
}

#[derive(PartialEq)]
pub(crate) struct Controls {
    pub link: Link,
    pub saved: Renderer,
    pub palette: Palette,
}

impl Component for Controls {
    fn render(&self) -> impl IntoElement {
        let status = use_try_consume::<State<RendererStatus>>();
        let status = status.as_ref().map(|status| status.read());
        let (saved, palette) = (self.saved, self.palette);
        let mut failed = use_state(|| false);
        let mut saving = use_state(|| false);
        let active = status.as_ref().map_or_else(
            || i18n::tr("This window does not report a native renderer.").to_owned(),
            |status| words("In use now: {renderer}", "renderer", name(status.active)),
        );
        let order = Backend::compiled()
            .iter()
            .map(|&backend| name(backend))
            .collect::<Vec<_>>()
            .join(" → ");
        let missing = CHOICES
            .into_iter()
            .filter(|&choice| !compiled(choice))
            .map(choice_name)
            .collect::<Vec<_>>()
            .join(", ");
        rect()
            .width(Size::fill())
            .spacing(8.)
            .child(
                label()
                    .text(i18n::tr(TITLE))
                    .color(palette.text)
                    .font_size(16.)
                    .font_weight(FontWeight::BOLD),
            )
            .child(label().text(active).color(palette.text).font_size(14.))
            .children(
                status
                    .iter()
                    .flat_map(|status| startup_notes(status))
                    .map(|(message, backend)| {
                        note(words(message, "renderer", name(backend)), palette)
                    }),
            )
            .child(note(
                words(
                    "Saved preference: {renderer}",
                    "renderer",
                    choice_name(saved),
                ),
                palette,
            ))
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::wrap_spacing(8.))
                    .spacing(8.)
                    .children(CHOICES.into_iter().filter(|&choice| compiled(choice)).map(
                        |choice| {
                            let link = self.link.clone();
                            let button = Button::new()
                                .enabled(!self.link.read_only() && !saving())
                                .on_press(move |_| {
                                    if saving() || choice == saved {
                                        return;
                                    }
                                    saving.set(true);
                                    failed.set(false);
                                    let patch = link.patch_settings(SettingsPatch {
                                        renderer: Some(choice),
                                        ..Default::default()
                                    });
                                    spawn(async move {
                                        failed.set(patch.await.is_err());
                                        saving.set(false);
                                    });
                                })
                                .child(text::caption(choice_name(choice)));
                            if saved == choice {
                                button.filled()
                            } else {
                                button
                            }
                        },
                    )),
            )
            .maybe_child(failed().then(|| {
                note(
                    i18n::tr("The renderer preference could not be saved. Try again."),
                    palette,
                )
            }))
            .child(note(
                i18n::tr(if cfg!(target_os = "android") {
                    "Restart Peersus to apply renderer changes."
                } else {
                    "Renderer changes apply the next time you open a window."
                }),
                palette,
            ))
            .child(note(
                words(
                    "Automatic tries these renderers in order: {renderers}",
                    "renderers",
                    &order,
                ),
                palette,
            ))
            .maybe_child((!missing.is_empty()).then(|| {
                note(
                    words(
                        "Not included in this build: {renderers}",
                        "renderers",
                        &missing,
                    ),
                    palette,
                )
            }))
            .maybe_child(comparison(
                self.link.clone(),
                status.as_ref().map(|status| status.active),
                palette,
            ))
    }
}

#[cfg(target_os = "android")]
fn comparison(_link: Link, _active: Option<Backend>, palette: Palette) -> Option<Element> {
    Some(
        note(
            i18n::tr("Renderer comparison is available on desktop only."),
            palette,
        )
        .into(),
    )
}

#[cfg(not(target_os = "android"))]
fn comparison(link: Link, active: Option<Backend>, palette: Palette) -> Option<Element> {
    Some(
        Comparison {
            link,
            active: active?,
            palette,
        }
        .into(),
    )
}

#[cfg(not(target_os = "android"))]
use crate::renderer_bench::{self, Failure, Recommendation, Report, Update};

#[cfg(not(target_os = "android"))]
#[derive(PartialEq)]
struct Comparison {
    link: Link,
    active: Backend,
    palette: Palette,
}

#[cfg(not(target_os = "android"))]
#[derive(PartialEq)]
enum Run {
    Idle,
    Running(Backend),
    Stopping,
    Cancelled,
    Interrupted,
    Finished(Report),
}

#[cfg(not(target_os = "android"))]
impl Component for Comparison {
    fn render(&self) -> impl IntoElement {
        use std::{cell::RefCell, rc::Rc};
        use tokio::sync::{mpsc, oneshot};

        let mut run = use_state(|| Run::Idle);
        let cancel = use_hook(|| Rc::new(RefCell::new(None::<oneshot::Sender<()>>)));
        let cancel_on_drop = Rc::clone(&cancel);
        use_drop(move || {
            cancel_on_drop.borrow_mut().take();
        });
        let (active, palette) = (self.active, self.palette);
        let state = run.read();
        let started = matches!(*state, Run::Running(_) | Run::Stopping);
        let link = self.link.clone();
        let start = {
            let cancel = Rc::clone(&cancel);
            move |_| {
                if cancel.borrow().is_some() || link.read_only() {
                    return;
                }
                let (stop, cancel_rx) = oneshot::channel();
                *cancel.borrow_mut() = Some(stop);
                run.set(Run::Running(active));
                let (updates, mut events) = mpsc::channel(2);
                drop(
                    link.executor()
                        .spawn(renderer_bench::compare(active, updates, cancel_rx)),
                );
                let cancel = Rc::clone(&cancel);
                spawn(async move {
                    let outcome = loop {
                        match events.recv().await {
                            Some(Update::Started(backend)) => {
                                if !matches!(*run.peek(), Run::Stopping) {
                                    run.set(Run::Running(backend));
                                }
                            }
                            Some(Update::Finished(report)) => break Run::Finished(report),
                            Some(Update::Cancelled) => break Run::Cancelled,
                            None => break Run::Interrupted,
                        }
                    };
                    cancel.borrow_mut().take();
                    run.set(outcome);
                });
            }
        };
        // Read the shared stop slot only from handlers; the visible run state, not a borrowed
        // sender, decides which action to expose.
        let mut group = rect().width(Size::fill()).spacing(8.).child(note(i18n::tr(
            "Run a short test in temporary windows. Results stay on this device. Leaving this page stops the test.",
        ), palette));
        if started {
            group = group.child(
                Button::new()
                    .enabled(!matches!(*state, Run::Stopping))
                    .on_press(move |_| {
                        if let Some(stop) = cancel.borrow_mut().take() {
                            run.set(Run::Stopping);
                            let _ = stop.send(());
                        }
                    })
                    .child(i18n::tr("Cancel")),
            );
        } else {
            group = group.child(
                Button::new()
                    .enabled(!self.link.read_only())
                    .on_press(start)
                    .child(text::caption(i18n::tr(COMPARE))),
            );
        }
        match &*state {
            Run::Idle => group,
            Run::Running(backend) => group.child(note(
                words("Testing {renderer}…", "renderer", name(*backend)),
                palette,
            )),
            Run::Stopping => group.child(note(i18n::tr("Stopping renderer comparison…"), palette)),
            Run::Cancelled => {
                group.child(note(i18n::tr("Renderer comparison cancelled."), palette))
            }
            Run::Interrupted => group.child(note(
                i18n::tr("Renderer comparison could not finish. Try again."),
                palette,
            )),
            Run::Finished(report) => group.child(results(report, active, palette)),
        }
    }
}

#[cfg(not(target_os = "android"))]
fn failure_words(failure: Failure) -> &'static str {
    match failure {
        Failure::Unavailable => "Could not start on this display",
        Failure::TimedOut => "Timed out",
        Failure::Interrupted => "Test did not finish",
        Failure::NoCounters => "CPU or memory counters unavailable",
        Failure::SceneMismatch => "Test frame did not match the scene",
    }
}

#[cfg(not(target_os = "android"))]
fn results(report: &Report, active: Backend, palette: Palette) -> Element {
    let cell = |text: String, weight, colour| {
        rect()
            .width(Size::flex(weight))
            .child(label().text(text).color(colour).font_size(13.))
    };
    let row = || {
        rect()
            .width(Size::fill())
            .horizontal()
            .content(Content::flex())
            .spacing(8.)
            .padding((4., 8.))
    };
    let heading = [
        "Renderer",
        "p50 (ms)",
        "p95 (ms)",
        "Main CPU (%)",
        "RAM (MiB)",
    ];
    let table = rect()
        .width(Size::fill())
        .padding(4.)
        .spacing(4.)
        .corner_radius(8.)
        .background(palette.surface)
        .child(
            row().children(heading.into_iter().enumerate().map(|(index, message)| {
                cell(
                    i18n::tr(message).to_owned(),
                    if index == 0 { 1.8 } else { 1. },
                    palette.muted,
                )
            })),
        )
        .children(report.rows.iter().map(|(backend, reading)| {
            let name = name(*backend);
            let name = if *backend == report.baseline {
                words("{renderer} (baseline)", "renderer", name)
            } else {
                name.to_owned()
            };
            let row = row().child(cell(name, 1.8, palette.text));
            match reading {
                Ok(reading) => row.children(
                    [
                        reading.p50_ms,
                        reading.p95_ms,
                        reading.cpu_percent,
                        reading.resident_mib(),
                    ]
                    .map(|value| cell(format!("{value:.1}"), 1., palette.text)),
                ),
                Err(failure) => row.child(cell(
                    i18n::tr(failure_words(*failure)).to_owned(),
                    4.,
                    palette.muted,
                )),
            }
        }));
    let recommend = match report.recommend() {
        Recommendation::NoBaseline => {
            i18n::tr("No recommendation: the current renderer could not be measured. Try again.")
                .to_owned()
        }
        Recommendation::OnlyBaseline => {
            i18n::tr("No recommendation: only the current renderer could be measured.").to_owned()
        }
        Recommendation::Keep => i18n::tr(
            "Keep the current renderer. No alternative showed a clear gain without a trade-off.",
        )
        .to_owned(),
        Recommendation::Smoother(backend) => words(
            "Try {renderer}: its slow frames were shorter in this test.",
            "renderer",
            name(backend),
        ),
        Recommendation::LessCpu(backend) => words(
            "Try {renderer}: it used less main-thread CPU in this test.",
            "renderer",
            name(backend),
        ),
        Recommendation::LessMemory(backend) => words(
            "Try {renderer}: it used less memory in this test.",
            "renderer",
            name(backend),
        ),
    };
    rect().width(Size::fill()).spacing(8.)
        .child(table)
        .child(note(i18n::tr("p50 is the typical frame time; p95 shows the slower frames. CPU is a percentage of one core."), palette))
        .child(label().font_size(14.).color(palette.text).text(if report.baseline == active { recommend } else {
            i18n::tr("The active renderer changed. Rerun the test for a current baseline.").to_owned()
        })).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_status_names_the_fallback_without_claiming_untried_backends() {
        let status = RendererStatus {
            preference: RendererPreference::Auto,
            environment_override: Some(Backend::Vulkan),
            active: Backend::OpenGl,
            startup_failures: vec![(Backend::Vulkan, RendererFailure::Initialization)],
            recovered_from: None,
        };
        assert_eq!(
            startup_notes(&status).collect::<Vec<_>>(),
            [
                ("A launch override requested {renderer}.", Backend::Vulkan),
                (
                    "{renderer} could not start on this display.",
                    Backend::Vulkan
                ),
            ]
        );
        assert_eq!(
            name_in(status.active, i18n::Language::default()),
            if cfg!(target_os = "android") {
                "OpenGL ES"
            } else {
                "OpenGL"
            }
        );
    }

    #[test]
    fn only_compiled_backends_are_selectable() {
        let selected = CHOICES
            .into_iter()
            .filter(|&choice| compiled(choice))
            .collect::<Vec<_>>();
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "android"))]
        assert_eq!(
            selected,
            [
                Renderer::Auto,
                Renderer::Vulkan,
                Renderer::OpenGl,
                Renderer::Software
            ]
        );
        #[cfg(target_os = "macos")]
        assert_eq!(selected, [Renderer::Auto, Renderer::Software]);
    }
}
