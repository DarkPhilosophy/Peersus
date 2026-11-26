//! What a pressed chord runs: the window's keys and, on Linux, the keys the desktop reports while the window is behind others.
use super::*;

/// Run what `chord` is bound to now, once for one press: the window and the desktop can both report the same press.
pub(crate) fn run_chord(
    link: &Link,
    chord: peersus_api::chord::Chord,
    page: State<Page>,
    offer: State<Option<view::Offered>>,
    sink: Option<link::FolderSink>,
    said: State<Option<String>>,
    mut last: State<Option<(peersus_api::chord::Chord, std::time::Instant)>>,
) {
    const ONE_PRESS: std::time::Duration = std::time::Duration::from_millis(300);
    let now = std::time::Instant::now();
    if let Some((before, at)) = *last.peek()
        && before == chord
        && now.duration_since(at) < ONE_PRESS
    {
        return;
    }
    last.set(Some((chord, now)));
    let Some(target) = link
        .client()
        .snapshot()
        .settings
        .bindings
        .get(&chord)
        .cloned()
    else {
        return;
    };
    run_bound(link, target, page, offer, sink, said);
}

/// What the outside bindings need of the root to run a chord.
#[cfg(target_os = "linux")]
pub(crate) struct OutsideRun {
    pub(crate) link: Link,
    pub(crate) page: State<Page>,
    pub(crate) offer: State<Option<view::Offered>>,
    pub(crate) sink: Option<link::FolderSink>,
    pub(crate) said: State<Option<String>>,
    pub(crate) recording: State<bool>,
    pub(crate) last: State<Option<(peersus_api::chord::Chord, std::time::Instant)>>,
}

/// Keep the desktop asked for the person's bindings while they have them work outside the window, and run what it reports pressed.
/// The portal is started only while the switch is on and something is bound, and ended when either goes: the desktop's dialog is
/// never opened for a person who did not ask for it.
#[cfg(target_os = "linux")]
pub(crate) async fn outside_keys(run: OutsideRun) {
    let (heard_to, mut heard) = tokio::sync::mpsc::unbounded_channel();
    let mut snapshots = run.link.client().subscribe();
    let mut outside: Option<outside_chords::Outside> = None;
    let mut seen: Option<(bool, peersus_core::bindings::Bindings)> = None;
    loop {
        let snapshot = snapshots.borrow_and_update().clone();
        let settings = &snapshot.settings;
        let now = (settings.bindings_outside, settings.bindings.clone());
        if seen.as_ref() != Some(&now) {
            if now.0 && !now.1.is_empty() {
                // The words the desktop's dialog shows for each: what its row on the Binds page says.
                let rows = view::bind_rows(&View::of(&snapshot));
                let asked: outside_chords::Asked = now
                    .1
                    .iter()
                    .map(|(chord, target)| {
                        let words = rows
                            .iter()
                            .find(|row| row.target == *target)
                            .map_or_else(|| chord.to_string(), binds_page::row_words);
                        (*chord, words)
                    })
                    .collect();
                outside
                    .get_or_insert_with(|| outside_chords::Outside::start(heard_to.clone()))
                    .ask(asked);
            } else {
                outside = None;
            }
            seen = Some(now);
        }
        tokio::select! {
            changed = snapshots.changed() => if changed.is_err() { break },
            chord = heard.recv() => {
                let Some(chord) = chord else { break };
                if *run.recording.peek() {
                    continue;
                }
                run_chord(&run.link, chord, run.page, run.offer, run.sink.clone(), run.said, run.last);
                // A page shown by a key pressed elsewhere is brought forward with its window.
                use peersus_core::bindings::{AppAction, BindTarget};
                if matches!(
                    run.link.client().snapshot().settings.bindings.get(&chord),
                    Some(BindTarget::App { action: AppAction::ShowSend | AppAction::ShowReceive })
                ) {
                    Platform::get().with_window(Platform::window_id(), |window| {
                        window.set_minimized(false);
                        window.focus_window();
                    });
                }
            }
        }
    }
}

/// Run what a chord is bound to, the way its button runs it: a page shown, the offer over the window answered with its own Accept
/// or Cancel, or an extension's action through the owner, which checks it as it checks the button.
pub(crate) fn run_bound(
    link: &Link,
    target: peersus_core::bindings::BindTarget,
    mut page: State<Page>,
    offer: State<Option<view::Offered>>,
    sink: Option<link::FolderSink>,
    said: State<Option<String>>,
) {
    use peersus_core::bindings::{AppAction, BindTarget};
    match target {
        BindTarget::App {
            action: AppAction::ShowSend,
        } => page.set(Page::Send),
        BindTarget::App {
            action: AppAction::ShowReceive,
        } => page.set(Page::Receive),
        BindTarget::App { action } => {
            // Nothing waits: the key has nothing to answer.
            let Some(offered) = offer.peek().clone() else {
                return;
            };
            let ids: Vec<String> = offered.files.iter().map(|file| file.id.clone()).collect();
            if action == AppAction::AcceptOffer {
                accept(
                    link,
                    &ids,
                    offered.auto_save,
                    offered.save_folder.as_deref(),
                    sink,
                    said,
                );
            } else {
                for transfer in ids {
                    link.send(Command::CancelTransfer { transfer });
                }
            }
        }
        BindTarget::Plugin {
            plugin,
            action,
            peer,
        } => link.send(Command::InvokeAction {
            plugin,
            action,
            peer,
        }),
    }
}
