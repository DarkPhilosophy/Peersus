//! The Send page: choosing files, the devices to send them to, the circle that sends, and the code a device scans.
use super::*;

/// What the hint line says about where a send will go.
pub(crate) fn hint(state: &SendState) -> &'static str {
    match state {
        SendState::Ready(devices) if devices.len() > 1 => "Goes straight to the devices you picked",
        SendState::Ready(_) => "Goes straight to the device you picked",
        SendState::ByCode => "No device picked: a code will be made",
        SendState::ChosenUnavailable => {
            "A device you picked is not available. Pick again, or none to use a code"
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SendNotice {
    Message(&'static str),
    Refused(u32),
    Intake(intake::Summary),
}

impl SendNotice {
    pub(crate) fn intake(intake: &intake::Intake) -> Option<Self> {
        let summary = intake::Summary::from(intake);
        (summary != intake::Summary::default()).then_some(Self::Intake(summary))
    }

    pub(crate) fn words(self) -> String {
        let language = i18n::language();
        match self {
            Self::Message(message) => i18n::translate(language, message).to_owned(),
            Self::Refused(count) => i18n::format(
                language,
                "{refused} selected files could not be offered.",
                &[("refused", &count.to_string())],
            ),
            Self::Intake(summary) => summary.words_in(language).unwrap_or_default(),
        }
    }
}

/// What a files code was made of, kept by the page while nobody has scanned the code: the paths of a desktop choice, or a second set of the held
/// files of an Android one (see `link::Gathered::spare`). It is what is listed again when the code gives way to a device the person picks
/// (see `view::code_gives_way`); the files that wait behind the code itself are the owner's, and are let go of there.
pub(crate) struct BehindCode {
    pub(crate) generation: u64,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) held: Option<link::Gathered>,
}

/// The one place a page keeps a [`BehindCode`]: of the code it shows, and of no other.
pub(crate) type KeptBehindCode = std::rc::Rc<std::cell::RefCell<Option<BehindCode>>>;

/// What the page tells a code's presentation apart from its three states: where what the code was made of is kept once the code is shown, and
/// the generation of a code that was taken back, of which nothing more is said.
pub(crate) struct CodeKeeping {
    pub(crate) kept: KeptBehindCode,
    pub(crate) taken_back: State<Option<u64>>,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) held: Option<link::Gathered>,
    /// The names of the files the code is being made of, listed under the circle from the touch that asked for the code (see
    /// `view::names_under_circle`); they become the shown code's own when it is ready, and are let go of when no code came.
    pub(crate) making: State<Option<Vec<String>>>,
}

/// Presentation observes a process-owned producer; leaving the page cannot cancel it.
pub(crate) async fn show_files_code(
    production: link::CodeProduction,
    mut shown: State<Option<ShownCode>>,
    mut expired: State<Option<u64>>,
    mut notice: State<Option<SendNotice>>,
    keeping: CodeKeeping,
) {
    let CodeKeeping {
        kept,
        taken_back,
        paths,
        held,
        mut making,
    } = keeping;
    let ready = production.ready.await;
    // Whatever the answer, the code is no longer being made: its names go with the code that is shown, or with nothing.
    let names = making.write().take().unwrap_or_default();
    match ready {
        Ok(Ok(Some(link::ReadyCode {
            code,
            generation,
            expires_at,
            total,
            names: _,
        }))) => {
            expired.set(None);
            // Kept only when there is something to list again, and under the generation of the code it is of.
            *kept.borrow_mut() = (!paths.is_empty() || held.is_some()).then_some(BehindCode {
                generation,
                paths,
                held,
            });
            shown.set(Some(ShownCode {
                code,
                generation,
                expected: total,
                made_at: unix_now(),
                ends_at: expires_at,
                names,
            }));
            // The owner publishes nothing when a code runs out unscanned: the window stops drawing it itself, at the
            // deadline the reply carried, and only if it is still that code.
            let left = view::until_expiry(expires_at, unix_now());
            let mut told = link::after(left);
            spawn(async move {
                if (&mut told).await.is_ok() {
                    expired.set(Some(generation));
                }
            });
            let finished = production.finished.await;
            // A code that was taken back: what its producer could not queue behind it is listed again, and is no refusal to tell of.
            if *taken_back.peek() == Some(generation) {
                return;
            }
            match finished {
                Ok(refused) => {
                    if refused > 0 {
                        notice.set(Some(SendNotice::Refused(refused)));
                    }
                    if shown
                        .peek()
                        .as_ref()
                        .is_some_and(|code| code.generation == generation)
                    {
                        let expected = total.saturating_sub(refused);
                        if expected == 0 {
                            shown.set(None);
                        } else if let Some(code) = shown.write().as_mut() {
                            code.expected = expected;
                        }
                    }
                }
                Err(_) => notice.set(Some(SendNotice::Message(
                    "The file producer stopped before finishing the selection.",
                ))),
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(reason)) => notice.set(Some(SendNotice::Message(reason))),
        Err(_) => notice.set(Some(SendNotice::Message(
            "The file producer stopped before creating a code.",
        ))),
    }
}

/// A files code this window made and shows: the code as text, drawn only while the owner says it can still admit a device, and
/// the generation the owner gave it, which is how everything the owner reports about it (and the files that came of it) is found.
pub(crate) struct ShownCode {
    pub(crate) code: String,
    pub(crate) generation: u64,
    pub(crate) expected: u32,
    /// When the code was shown and when the owner ends it, in seconds on the clock of `unix_now`: what the circle's fuse is drawn from, so that
    /// the time the code has left is the owner's own deadline and not a guess of the window's.
    pub(crate) made_at: u64,
    pub(crate) ends_at: u64,
    /// The names of every file the code was made of, for the list under the circle while the code is drawn.
    pub(crate) names: Vec<String>,
}

/// Seconds since 1970, the clock a code's deadline is in. Only used to know how long a code has left; a clock that cannot be
/// read gives 0, which leaves the whole life of the code, and the owner refuses it at its own deadline anyway.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The paired devices `ids` as where files go.
pub(crate) fn to_devices(ids: &[String]) -> Vec<peersus_core::model::FileDestination> {
    ids.iter()
        .cloned()
        .map(|id| peersus_core::model::FileDestination::Peer { peer: PeerId(id) })
        .collect()
}

/// Pick files and send them straight to the paired device `peer`: the quick Send on a device's own card, which needs no page of its
/// own and no code. The same rules as Send to a picked device: on a platform with a picker of its own (Android) the files come back
/// open and go in commands of at most the most the owner accepts, and on a desktop the dialog's paths are judged by the same intake
/// (a folder, a file that is gone) and offered together. What was left out is said in `said`, as on the Send page.
pub(crate) fn choose_and_send(
    link: &Link,
    picker: Option<link::FilePicker>,
    peer: String,
    mut said: State<Option<SendNotice>>,
) {
    said.set(None);
    // A read-only window cannot offer files: it says so where a refusal would be said, and no
    // dialog or picker is opened.
    if link.read_only() {
        said.set(Some(SendNotice::Message(link::READ_ONLY)));
        return;
    }
    if let Some(picker) = picker {
        picker.picked.opened_streaming();
        (picker.open)(true);
        let batches = link::await_picked(
            picker.picked.clone(),
            link::LOOK_EVERY,
            link::PICKER_PATIENCE,
        );
        let handed_off = link.send_picked(vec![PeerId(peer)], batches);
        spawn(async move {
            if let Ok(refused) = handed_off.await
                && refused > 0
            {
                said.set(Some(SendNotice::Refused(refused)));
            }
        });
        return;
    }
    let link = link.clone();
    spawn(async move {
        #[cfg(not(target_os = "android"))]
        if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
            let intake = intake::accept(files.iter().map(|file| file.path().to_path_buf()));
            said.set(SendNotice::intake(&intake));
            // The quick send of a device's own card shows no batch under a circle: the answer is not wanted.
            drop(link.send_paths(to_devices(std::slice::from_ref(&peer)), intake.taken));
        }
        #[cfg(target_os = "android")]
        let _ = (&link, &peer, &said);
    });
}

#[derive(PartialEq)]
pub(crate) struct SendPage {
    pub(crate) link: Link,
    pub(crate) view: View,
    pub(crate) chosen: Vec<String>,
    pub(crate) pictures: shelf::Held,
    pub(crate) palette: Palette,
    pub(crate) form: view::Form,
    /// The width the window has, in pixels, from which each page decides how large its own portal is drawn:
    /// the code in it is not the same on every page, and what it needs is what decides.
    pub(crate) room: f32,
    pub(crate) on_choose: EventHandler<String>,
    /// The platform's own file picker, on a platform that has one in place of the toolkit's dialog.
    pub(crate) session_picker: Option<link::FilePicker>,
    /// The second now, Unix seconds, from the window's one clock: the circle counts down from it while the files wait.
    pub(crate) now: u64,
}

impl Component for SendPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let accent = palette.send;
        // The files code the window made and is showing, with the generation the owner gave it: what the circle is worked out
        // from (see `view::code_circle`). One state, so that the code and what it is the code OF are never apart.
        let mut shown: State<Option<ShownCode>> = use_state(|| None);
        // The generation of a code whose time ran out unscanned. A newer code is another generation and is not touched by it.
        let mut expired: State<Option<u64>> = use_state(|| None);
        // What the last batch of files left out, worded for the person; `None` when all were taken.
        let mut notice: State<Option<SendNotice>> = use_state(|| None);
        // The files that were chosen and wait to be sent, as the paths of a desktop. Choosing starts nothing and makes no code: the files are only
        // listed here, the person says where they go (the cards, and the button for a code), and sending them is a hold on the circle.
        let mut staged_paths: State<Vec<PathBuf>> = use_state(Vec::new);
        // The transfers as of this draw, for the handlers, which cannot see `self`: what a new choice of files reads to know what still goes.
        let mut transfers_for_handlers: State<Vec<view::Progress>> = use_state(Vec::new);
        // How many FILES were just handed over and not yet answered for by the owner: set when the circle sends, and let go of in the same step that
        // writes the batch from the owner's answer, or when the send is given up. Files, not transfers: a file sent to two devices is one file here, as it
        // is in the circle after the owner answers, so the number never changes between the gap and the answer.
        let mut handed_over: State<usize> = use_state(|| 0);
        // The number of the lap of the circle (files chosen, sent, ended) and the first second the page saw the batch as ended, with the lap it was
        // noted for: what the fade of the result is worked out from, in the window's own clock.
        let mut lap: State<view::Lap> = use_state(view::Lap::default);
        let mut result_noted: State<Option<(view::Lap, u64)>> = use_state(|| None);
        transfers_for_handlers.set_if_modified(self.view.transfers.clone());
        let link = self.link.clone();
        // The ids of the transfers the owner says it made of the files that were sent: the batch, from the owner's own answer (see `link::Sent`). The
        // list under the circle and the circle read this and nothing else to know which transfers are the batch; no guess from the transfers there were.
        let mut batch_ids: State<Vec<String>> = use_state(Vec::new);
        // What the code on show was made of, and the generations of the code the owner was asked to take back and of the one being taken back
        // (see `view::code_gives_way`). Asked once for a code; the second is cleared when the owner says the code was scanned first.
        let behind_code: KeptBehindCode = use_hook(Default::default);
        let mut asked_back: State<Option<u64>> = use_state(|| None);
        let mut taken_back: State<Option<u64>> = use_state(|| None);
        // Every way a file comes in ends here: chosen in the dialog, dropped on the window, or
        // pasted. The same rule judges them all, so a file is never treated by how it arrived.
        let take = EventHandler::new({
            let read_only = self.link.read_only();
            let behind_code = behind_code.clone();
            move |offered: Vec<PathBuf>| {
                // A read-only window takes no file however it arrives; it says so in the same
                // place a refused intake is said, and looks at nothing on the disk.
                if read_only {
                    notice.set(Some(SendNotice::Message(link::READ_ONLY)));
                    return;
                }
                let intake = intake::accept(offered);
                notice.set(SendNotice::intake(&intake));
                if intake.is_empty() {
                    return;
                }
                // Listed, and nothing more: a device that was picked and has gone is told in the circle, and nothing is sent before the
                // person holds it.
                // Files that are chosen now are a new transfer: the code of the one before (stopped, sent, or gone) is no longer what the circle
                // describes, and would keep saying "stopped 0/1" over files that have not been sent.
                // A new lap: the result of the one before is left behind at once (see `view::Lap`).
                let next_lap = lap.peek().next();
                lap.set(next_lap);
                shown.set(None);
                expired.set(None);
                behind_code.borrow_mut().take();
                let kept = view::ids_kept_on_new_choice(
                    &transfers_for_handlers.peek(),
                    &batch_ids.peek(),
                    None,
                );
                batch_ids.set(kept);
                // More files join what already waits (the model refuses a file that is already in the list); a choice made with nothing waiting is
                // the first, and is the whole list.
                let mut waiting = view::Waiting::new(staged_paths.peek().clone());
                waiting.add(intake.taken);
                staged_paths.set(waiting.files().to_vec());
            }
        });
        // The files of an Android selection, held by number in the registry and only LISTED: they wait here, in a guard that lets go of them if
        // the page is left, until the circle is used. Not state: a guard cannot be compared or copied, and what the page shows of it is
        // `staged_names`.
        let held_files: std::rc::Rc<std::cell::RefCell<Option<link::Gathered>>> =
            use_hook(Default::default);
        let mut staged_names: State<Vec<String>> = use_state(Vec::new);
        let mut code_names: State<Option<Vec<String>>> = use_state(|| None);
        let files_listed = !staged_paths.read().is_empty() || !staged_names.read().is_empty();
        // A paired device held near with Receive open (experimental; see `View::receiving_near`): the listed files go to it at once, with
        // nothing more pressed here. One that is near with another page open is only named, so that the person knows what is missing.
        let receiving: Option<String> = self
            .view
            .receiving_near(&self.chosen, files_listed)
            .filter(|_| !self.link.read_only())
            .map(|device| device.id.clone());
        let waited: Option<String> = self
            .view
            .near_waiting(&self.chosen, files_listed)
            .map(|device| device.name.clone());
        // Where a send by the circle goes is decided in the model and only worded here: to the paired devices the person picked, or else
        // behind a code. Nearness picks nothing for the circle.
        let chosen: Vec<String> = self.chosen.clone();
        let state = self.view.send_state(&chosen);
        let unavailable = matches!(state, SendState::ChosenUnavailable);
        // Where what is listed would go: the devices picked (as `send_state` has judged them), none, or one that went away.
        let aim = view::aim_of(&state);
        // A device that is not paired and was near already when the files were listed (experimental; see `View::brought_near`): the page
        // opens knowing who is near, so that files that arrive with it (a share) are not given to a device that only lay there.
        let near_at_first = self.view.strangers_near();
        let mut near_before: State<Vec<String>> = use_state(move || near_at_first);
        let still_near = self
            .view
            .near_before_kept(&near_before.read(), files_listed);
        near_before.set_if_modified(still_near.clone());
        let files_waiting = staged_paths.read().len().max(staged_names.read().len());
        let brought: Option<String> = self
            .view
            .brought_near(&self.chosen, &still_near, files_waiting)
            .filter(|_| !self.link.read_only())
            .map(|stranger| stranger.id.0.clone());
        // Hands the listed files over to `destinations` and, `with_code`, behind a code: what the circle does, and what bringing a device
        // near does. The listed files are taken out of their place first, so that nothing can send the same files twice; with nothing listed
        // it does nothing.
        let hand_over = EventHandler::new({
            let (link, held_files) = (link.clone(), held_files.clone());
            let behind_code = behind_code.clone();
            move |(destinations, with_code): (Vec<peersus_core::model::FileDestination>, bool)| {
                // An Android selection: ONE hand-over to the devices or the code, from the very files that were listed. `send_gathered` empties
                // the guard, so that ending it lets go of nothing twice.
                // What waits is ONE thing seen in two places (the paths of a desktop, the held files of an Android selection and their names): taken
                // together, and emptied together, so that the circle can never count files that were already taken.
                let gathered = held_files.borrow_mut().take();
                let paths = std::mem::take(&mut *staged_paths.write());
                // How many files go, said BEFORE the listing is emptied: the names of an Android selection, the paths of a desktop one.
                // Looked at without subscribing (`peek`): this also runs inside the effect of a device brought near, and an effect runs
                // again whenever something it READ changes. Read here and emptied two lines below, the names would call the effect again
                // for ever, on the thread that draws.
                let files_going = staged_names.peek().len().max(paths.len());
                // Files that go behind a code stay named under the circle while the code is made and drawn.
                if with_code && files_going > 0 {
                    code_names.set(Some(view::names_listed(&paths, &staged_names.peek())));
                }
                staged_names.set(Vec::new());
                if gathered.is_some() {
                    handed_over.set(files_going);
                }
                // An Android selection: ONE hand-over to the devices or the code, from the very files that were listed. `send_gathered` empties
                // the guard, so that ending it lets go of nothing twice.
                if let Some(gathered) = gathered {
                    // A second set of the files, for the page, before the first goes behind the code.
                    let spare = with_code.then(|| gathered.spare()).flatten();
                    let before = *taken_back.peek();
                    let (production, handed_off) =
                        link.send_gathered(destinations, with_code, gathered);
                    if let Some(production) = production {
                        spawn(show_files_code(
                            production,
                            shown,
                            expired,
                            notice,
                            CodeKeeping {
                                kept: behind_code.clone(),
                                taken_back,
                                paths: Vec::new(),
                                held: spare,
                                making: code_names,
                            },
                        ));
                    } else {
                        // No code is being made of them (a read-only window): nothing is behind one.
                        code_names.set(None);
                    }
                    spawn(async move {
                        let sent = handed_off.await;
                        // The code these files went behind was taken back meanwhile: they are listed again, and this answer is of nothing.
                        if with_code && *taken_back.peek() != before {
                            return;
                        }
                        match sent {
                            Ok(sent) => {
                                // The batch is what the owner says it made of these files: not what a screen can make out from the transfers that
                                // were there before. The count of files handed over is let go of in the same step.
                                batch_ids.set(sent.transfers.clone());
                                handed_over.set(0);
                                if sent.refused > 0 {
                                    notice.set(Some(SendNotice::Refused(sent.refused)));
                                }
                            }
                            Err(_) => handed_over.set(0),
                        }
                    });
                    return;
                }
                if paths.is_empty() {
                    return;
                }
                if with_code {
                    let production = link.code_paths(paths.clone());
                    spawn(show_files_code(
                        production,
                        shown,
                        expired,
                        notice,
                        CodeKeeping {
                            kept: behind_code.clone(),
                            taken_back,
                            paths,
                            held: None,
                            making: code_names,
                        },
                    ));
                } else {
                    let count = paths.len();
                    handed_over.set(count);
                    let answer = link.send_paths(destinations, paths);
                    spawn(async move {
                        match answer.await {
                            Ok(sent) => {
                                // The batch is what the owner says it made of these files (see `link::Sent`), the same as on Android; the count of
                                // files handed over is let go of in the same step, so there is no moment with both or with neither.
                                batch_ids.set(sent.transfers.clone());
                                handed_over.set(0);
                                if sent.refused > 0 {
                                    notice.set(Some(SendNotice::Refused(sent.refused)));
                                }
                            }
                            // The owner stopped before it answered: nothing is on its way, and a circle left saying so would say it for ever.
                            Err(_) => handed_over.set(0),
                        }
                    });
                }
            }
        });
        // Carries out what the circle says, once for a touch; with nowhere to go it does nothing.
        let on_circle = EventHandler::new({
            let (aim, hand_over) = (aim.clone(), hand_over.clone());
            move |act: view::CircleAct| match (&act, &aim) {
                (view::CircleAct::Send, view::Aim::Devices(devices)) => {
                    hand_over.call((to_devices(devices), false));
                }
                (view::CircleAct::MakeCode, view::Aim::Code) => {
                    hand_over.call((Vec::new(), true));
                }
                _ => {}
            }
        });
        // A device that is not paired was brought near while files were listed: they are offered to it at once, and its person is asked
        // about each. Once for a bringing near: handing them over empties the list, and with it the rule that named the device.
        use_side_effect_with_deps(&brought, {
            let hand_over = hand_over.clone();
            move |brought: &Option<String>| {
                if let Some(peer) = brought {
                    hand_over.call((
                        vec![peersus_core::model::FileDestination::Nearby {
                            peer: PeerId(peer.clone()),
                        }],
                        false,
                    ));
                    notice.set(Some(SendNotice::Message(
                        "Offered to the device that was brought near. Each file is that person's to accept.",
                    )));
                }
            }
        });
        // A paired device is held near with Receive open while files are listed: they go to it at once, as a send to a device that was
        // picked goes. Once: handing them over empties the list, and with it the rule that named the device; files listed after that go
        // again for as long as it stays so.
        use_side_effect_with_deps(&receiving, {
            let hand_over = hand_over.clone();
            move |receiving: &Option<String>| {
                if let Some(peer) = receiving {
                    hand_over.call((to_devices(std::slice::from_ref(peer)), false));
                    notice.set(Some(SendNotice::Message(
                        "Sent to the device held near with Receive open. Each file is that person's to accept.",
                    )));
                }
            }
        });
        let file_picker = self.session_picker.clone();
        // The files an Android picker or a share hands over are read to the end and LISTED into `held_files` (see above).
        // What happens to files the platform's picker is handing over, from the moment its selection is open: they are read to the end and
        // LISTED, and nothing is sent. The same for a picker the person opened and for files another application shared, so that the two can
        // never be treated differently.
        let begin_files = EventHandler::new({
            let (file_picker, held_files) = (file_picker.clone(), held_files.clone());
            let behind_code = behind_code.clone();
            move |()| {
                let Some(picker) = file_picker.clone() else {
                    return;
                };
                // Ten minutes is far more than choosing files takes, and it ends a wait nothing will answer.
                let mut batches = link::await_picked(
                    picker.picked.clone(),
                    link::LOOK_EVERY,
                    link::PICKER_PATIENCE,
                );
                let (picked, held_files) = (picker.picked.clone(), held_files.clone());
                let behind_code = behind_code.clone();
                spawn(async move {
                    let mut gathered = link::Gathered::new(picked);
                    let mut refused = 0_u32;
                    while let Some(taken) = batches.recv().await {
                        gathered.add(taken.files);
                        refused = refused.saturating_add(taken.refused);
                        if !taken.still_open {
                            break;
                        }
                    }
                    if refused > 0 {
                        notice.set(Some(SendNotice::Refused(refused)));
                    }
                    // A pick that ended with nothing taken (backed out of, or every file refused) leaves what was listed before as it was, as the
                    // desktop does; only a listing with files replaces it, and the earlier guard lets go of its files as it is dropped.
                    if gathered.names().is_empty() {
                        return;
                    }
                    // More files JOIN the guard that already holds some (their numbers stay held, and they come first in the list); with nothing held
                    // this selection is the first and is the whole list. Nothing is let go of by adding, and nothing is held twice.
                    let names = {
                        let mut guard = held_files.borrow_mut();
                        match guard.as_mut() {
                            Some(earlier) => earlier.absorb(gathered),
                            None => *guard = Some(gathered),
                        }
                        guard
                            .as_ref()
                            .map(link::Gathered::names)
                            .unwrap_or_default()
                    };
                    // A new selection is a new transfer: the code of the one before no longer describes the circle.
                    shown.set(None);
                    expired.set(None);
                    behind_code.borrow_mut().take();
                    let kept = view::ids_kept_on_new_choice(
                        &transfers_for_handlers.peek(),
                        &batch_ids.peek(),
                        None,
                    );
                    batch_ids.set(kept);
                    staged_names.set(names);
                });
            }
        });
        // Files another application shared are already a selection in the registry. Told once, here, as the page draws; a share that is
        // not taken stays held and nobody would ever be asked who gets it.
        if let Some(picker) = &file_picker
            && !unavailable
            && picker.picked.take_external().is_some()
        {
            let begin = begin_files.clone();
            spawn(async move { begin.call(()) });
        }
        let choose = EventHandler::new({
            let take = take.clone();
            let begin_files = begin_files.clone();
            let read_only = self.link.read_only();
            move |()| {
                if unavailable {
                    return;
                }
                // The dialog or the platform's picker is an effect of its own: a read-only
                // window never opens either, and says so where a refused intake is said.
                if read_only {
                    take.call(Vec::new());
                    return;
                }
                // A platform with no dialog of the toolkit's (Android) gives its own picker: files come back as open
                // files that are kept and sent from where they are, never as paths and never copied.
                if let Some(picker) = file_picker.clone() {
                    picker.picked.opened_streaming();
                    (picker.open)(true);
                    begin_files.call(());
                    return;
                }
                let take = take.clone();
                // The dialog runs on its own thread; awaiting it here does not stop the window
                // from drawing while it is open.
                spawn(async move {
                    #[cfg(not(target_os = "android"))]
                    if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
                        take.call(files.iter().map(|file| file.path().to_path_buf()).collect());
                    }
                    #[cfg(target_os = "android")]
                    let _ = &take;
                });
            }
        });
        // What the person can do with the files that wait, ONE implementation for both ways a list is held (the paths of a desktop selection, the held
        // files of an Android one with their names): each action takes from both, together, so the list can never be half changed.
        let file_actions = FileActions {
            remove: EventHandler::new({
                let held_files = held_files.clone();
                move |index: usize| {
                    let mut from_guard = held_files.borrow_mut();
                    if let Some(gathered) = from_guard.as_mut() {
                        // The guard lets go of the file in the registry; what the page shows of it follows from what is left.
                        gathered.remove(index);
                        let names = gathered.names();
                        if names.is_empty() {
                            *from_guard = None;
                        }
                        staged_names.set(names);
                    } else {
                        let mut waiting = view::Waiting::new(staged_paths.peek().clone());
                        waiting.remove(index);
                        staged_paths.set(waiting.files().to_vec());
                    }
                }
            }),
            clear: EventHandler::new({
                let held_files = held_files.clone();
                move |()| {
                    // The guard lets go of every held file as it is dropped.
                    held_files.borrow_mut().take();
                    staged_names.set(Vec::new());
                    staged_paths.set(Vec::new());
                }
            }),
            // The picker that opens for the first choice opens to add more, and what it brings joins the list (see `take` and `begin_files`).
            add_more: choose.clone(),
        };

        // A row of cards side by side on a desktop, which goes on under itself when the next card no longer fits the width (a window narrower than
        // the cards it holds, or more saved devices than the window is wide); on a phone one under another, each as wide as the screen.
        let mut devices = if self.form.device_card_width().is_some() {
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::wrap_spacing(10.))
                .spacing(14.)
        } else {
            rect().width(Size::fill()).spacing(10.)
        };
        for device in &self.view.devices {
            let on_choose = self.on_choose.clone();
            devices = devices.child(DeviceCard {
                id: device.id.clone(),
                name: device.name.clone(),
                picture: device
                    .picture
                    .and_then(|digest| self.pictures.get(&digest))
                    .map(avatar_view::Shared),
                presence: device.presence.clone(),
                status: device.status_line(),
                picked: chosen.contains(&device.id),
                palette,
                form: self.form,
                on_choose,
            });
        }
        let devices: Element = if self.view.devices.is_empty() {
            label()
                .text(crate::i18n::tr(
                    "No saved devices yet. Send by code, and keep the device when asked.",
                ))
                .color(palette.muted)
                .font_size(15.)
                .into()
        } else if let Some(name) = &waited {
            rect()
                .width(Size::fill())
                .spacing(10.)
                .child(
                    label()
                        .text(i18n::format(
                            i18n::language(),
                            "{device} is near. Open Receive on it and these files go to it.",
                            &[("device", name)],
                        ))
                        .color(palette.muted)
                        .font_size(14.),
                )
                .child(devices)
                .into()
        } else {
            devices.into()
        };

        let beside = self.form.page_puts_portal_beside_text();
        let hero = rect()
            .width(if beside { Size::flex(1.) } else { Size::fill() })
            .spacing(if beside { 16. } else { 8. })
            .child(headline("Make files", "fly.", palette, accent, self.form))
            .child(
                label().text(crate::i18n::tr("One action. One destination. Move files between your devices without a labyrinth of menus."))
                    .color(palette.muted)
                    .font_size(if beside { 20. } else { 15. }),
            );

        // What the circle shows and does is ONE decision (`view::circle_view`), made from everything that bears on it at once: the files listed, where
        // they would go, the code that was made and what the owner reports of it, and the transfers of the batch chosen now. The page reads the answer and
        // chooses nothing of its own: not the words, not whether a touch does anything, not what it does.
        let code_state = shown.read().as_ref().map(|made| {
            if expired
                .read()
                .is_some_and(|generation| generation == made.generation)
            {
                view::CodeCircle::Gone
            } else {
                view::code_circle_selection(
                    made.generation,
                    self.view.files_code,
                    &self.view.transfers,
                    made.expected as usize,
                )
            }
        });
        // A code nobody has scanned gives way to the devices the person picks (see `view::code_gives_way`): the owner is asked, once, to take it
        // back, and when it has, what the code was made of is listed again and the circle sends it to those devices. A code that was scanned
        // first stays as it is. What is kept of a code that was scanned or ran out is let go of: it will not be listed again.
        {
            let shown_generation = shown.peek().as_ref().map(|made| made.generation);
            let kept_generation = behind_code.borrow().as_ref().map(|kept| kept.generation);
            let asked = *asked_back.peek();
            let over = code_state.as_ref().is_some_and(|code| {
                !matches!(code, view::CodeCircle::Code | view::CodeCircle::Gone)
            }) || shown_generation.is_some() && *expired.peek() == shown_generation;
            if let Some(generation) =
                shown_generation.filter(|shown| kept_generation == Some(*shown))
            {
                if over {
                    behind_code.borrow_mut().take();
                } else if view::code_gives_way(code_state.as_ref(), &aim)
                    && asked != Some(generation)
                {
                    asked_back.set(Some(generation));
                    taken_back.set(Some(generation));
                    let answer = self.link.withdraw_code(generation);
                    let (behind_code, held_files) = (behind_code.clone(), held_files.clone());
                    spawn(async move {
                        if !answer.await.unwrap_or(false) {
                            taken_back.set(None);
                            return;
                        }
                        let kept = behind_code.borrow_mut().take();
                        let Some(kept) = kept.filter(|kept| kept.generation == generation) else {
                            return;
                        };
                        // The same as files chosen now: a new lap, no code, and nothing handed over.
                        let next_lap = lap.peek().next();
                        lap.set(next_lap);
                        shown.set(None);
                        expired.set(None);
                        handed_over.set(0);
                        if let Some(held) = kept.held {
                            let names = {
                                let mut guard = held_files.borrow_mut();
                                match guard.as_mut() {
                                    Some(earlier) => earlier.absorb(held),
                                    None => *guard = Some(held),
                                }
                                guard
                                    .as_ref()
                                    .map(link::Gathered::names)
                                    .unwrap_or_default()
                            };
                            staged_names.set(names);
                        } else {
                            let mut waiting = view::Waiting::new(staged_paths.peek().clone());
                            waiting.add(kept.paths);
                            let files = waiting.files().to_vec();
                            staged_paths.set(files);
                        }
                    });
                }
            }
        }
        let batch_now = view::batch_of(
            &self.view.transfers,
            &batch_ids.read(),
            shown.read().as_ref().map(|made| made.generation),
        );
        let batch_transfers = &batch_now;
        let listed_count = if staged_names.read().is_empty() {
            staged_paths.read().len()
        } else {
            staged_names.read().len()
        };
        let circle = view::circle_view(&view::CircleInputs {
            listed: listed_count,
            aim: aim.clone(),
            send_instantly: self.view.settings.send_instantly,
            code: code_state.clone(),
            batch: batch_transfers,
            handed_over: *handed_over.read(),
        });
        let picked = match &aim {
            view::Aim::Devices(devices) => devices.len(),
            _ => 0,
        };
        // Read through the cache that knows what the drawing was made for, so that a window turned or resized after
        // the code was made gets one drawn for its new size; a memo that captured the size never saw it change.
        let drawn = use_state(|| std::rc::Rc::new(std::cell::RefCell::new(qr::Drawn::default())));
        let scale = *Platform::get().current_window().scale_factor.peek();
        let disc = qr::disc_physical(
            view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
            scale,
        );
        // The picture is drawn only while the circle says it is the code, and never from a code that was spent.
        let picture = match (circle.shows_code, shown.read().as_ref()) {
            (true, Some(made)) => drawn
                .read()
                .borrow_mut()
                .of(&made.code, disc)
                .map(|handle| qr::picture_from(handle, scale)),
            _ => None,
        };
        // While the files wait for the other device, the time this device still waits for the answer, from the window's clock.
        let circle_clock =
            code_state
                .as_ref()
                .zip(shown.read().as_ref())
                .and_then(|(code, made)| {
                    view::circle_time_in(
                        i18n::language(),
                        code,
                        made.generation,
                        &self.view.transfers,
                        self.now,
                    )
                });
        // How long the result of an ended batch has been on the circle, from the window's own clock: the page notes the first second it saw the batch
        // as ended (the owner keeps no end time of a transfer), for the lap it belongs to, and the model says how clear it still is (see
        // `view::result_opacity`). A newer lap (files chosen, or a send) leaves an older result behind at once.
        let ended_now = matches!(
            view::stage_of(&view::CircleInputs {
                listed: listed_count,
                aim: aim.clone(),
                send_instantly: self.view.settings.send_instantly,
                code: code_state.clone(),
                batch: batch_transfers,
                handed_over: *handed_over.read(),
            }),
            view::Stage::Ended { .. }
        );
        {
            let noted = *result_noted.peek();
            match (ended_now, noted) {
                (true, None) => result_noted.set(Some((*lap.peek(), self.now))),
                (false, Some(_)) => result_noted.set(None),
                _ => {}
            }
        }
        let (result_shown, clearness) = match *result_noted.read() {
            Some((noted_lap, ended_at)) => {
                let ended_ms = ended_at.saturating_mul(1_000);
                let now_ms = self.now.saturating_mul(1_000);
                (
                    view::shows_result(noted_lap, *lap.read(), ended_ms, now_ms),
                    view::result_opacity(ended_ms, now_ms),
                )
            }
            None => (true, 1.),
        };
        let words = view::circle_view_words_in(
            i18n::language(),
            &circle,
            code_state.as_ref(),
            picked,
            result_shown,
        );
        // The words of a result fade toward the colour of the circle, in the clearness the model gives; every other state is drawn clear.
        let ink = if ended_now && result_shown {
            glyph::dim(palette.portal_text, palette.portal, clearness)
        } else {
            palette.portal_text
        };
        // Extension destinations use the existing selection without taking it from
        // the peer/QR flow. Admission failures and cancellation leave it available.
        let selected_path = {
            let paths = staged_paths.read();
            (paths.len() == 1 && staged_names.read().is_empty()).then(|| paths[0].clone())
        };
        let selected_held = held_files
            .borrow()
            .as_ref()
            .and_then(|files| files.files())
            .filter(|files| files.len() == 1 && staged_paths.read().is_empty())
            .and_then(|files| files.into_iter().next());
        let extension_destinations = plugin_controls::SelectedControls {
            link: self.link.clone(),
            view: self.view.clone(),
            path: selected_path,
            held: selected_held,
            palette,
        };
        let mut centre: Vec<Element> = match picture {
            Some(picture) => vec![picture],
            None if words.asks_for_files => vec![
                mono(words.kicker.clone(), palette.portal_text, 12.).into(),
                label()
                    .text(words.headline.clone())
                    .color(palette.portal_text)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(words.line.clone())
                    .color(palette.portal_text)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(i18n::tr(if unavailable {
                        hint(&state)
                    } else {
                        self.form.circle_invitation()
                    }))
                    .color(palette.portal_text)
                    .font_size(14.)
                    .into(),
            ],
            None => [
                mono(words.kicker.clone(), ink, 12.).into(),
                label()
                    .text(words.headline.clone())
                    .color(ink)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(words.line.clone())
                    .color(ink)
                    .font_size(14.)
                    .into(),
            ]
            .into_iter()
            .chain(circle_clock.map(|text| {
                let colour = if text.ends_with("0:00") {
                    palette.bad
                } else {
                    palette.portal_text
                };
                mono(text, colour, 12.).into()
            }))
            .collect(),
        };
        if listed_count == 1 && !circle.shows_code {
            centre.push(extension_destinations.into());
        }
        // The sparks and the fuse of the code on show: made here, on every draw and in the same order as every other hook of this page. The fuse is a
        // smooth line from what the code has left when it is first seen to nothing at the owner's deadline (see `use_fuse`), the same as on the Pair page,
        // and not a number read from the clock once a second: the sparks fly from its tip, and a tip that moves a step a second made them shake.
        let reduced = self.link.client().snapshot().settings.reduced_motion;
        let code_sparks = use_sparks(reduced, circle.shows_code);
        let on_show = shown
            .read()
            .as_ref()
            .filter(|_| circle.shows_code)
            .map(|made| (made.generation, made.made_at, made.ends_at));
        let (burns, left, remaining) = match on_show {
            Some((generation, made_at, ends_at)) => (
                Some(generation),
                view::code_fuse(made_at, ends_at, self.now),
                ends_at.saturating_sub(self.now),
            ),
            None => (None, 0., 0),
        };
        let code_fuse = use_fuse(burns, left, remaining, reduced);
        // A circle that is shut (a code drawn, or a transfer going) answers no touch at all; one that is open does what the decision says.
        // The circle of the Send page: pressed to choose files when none are listed, and when they are, made to do what the page says
        // (`HoldToSend`): a touch makes a code when no device is picked, and it is held until it fills to send to the devices that are.
        // `life` is the share, from 1 to 0, of the life a code on show still has: the fuse burns round it, outside the disc and never in it,
        // so that the code inside is as it was drawn and scans as it did; `None` when the circle shows no code.
        let disc = Portal {
            palette,
            accent,
            on_press: (!unavailable && circle.does.is_some()).then(|| choose.clone()),
            children: centre,
            extent: view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
            hold: circle.does.filter(|_| !unavailable).map(|does| HoldToSend {
                does,
                on_act: on_circle.clone(),
            }),
            // How far the files have come while they go: the circle fills as they move, as it fills under a finger, in the same colour and way.
            going: view::circle_fill(&circle.batch, 0.),
            // The time the code has left, as the fuse that burns round it: smooth, from the owner's own deadline (`use_fuse`), and only while the code
            // is what the circle shows. Nothing is written in the code or on the disc, so what is scanned is what was drawn.
            life: burns.map(|_| code_fuse),
            // The sparks of its tip, the same as the fuse of a code on the Pair page (`use_sparks`): in flight only while a code burns.
            sparks: code_sparks,
            reduced,
        };

        // The files that were chosen and are only listed, as the circle's own context: no button of its own, no history. The circle is the only
        // control: with no device picked a touch on it makes a code; with devices picked it is held until it fills and sends to them.
        // The list that belongs to the circle, from the moment files are chosen: a row for each, with where it stands (prepared, going, done,
        // failed). One list, for the files that are listed and for the transfers made of them; never the history, which is the Transfers tab's.
        let listed_names: Vec<String> = {
            let made = shown.read();
            view::names_under_circle(
                view::names_listed(&staged_paths.read(), &staged_names.read()),
                code_names.read().as_deref(),
                made.as_ref()
                    .filter(|_| circle.shows_code)
                    .map(|made| made.names.as_slice()),
            )
        };
        let files_list = {
            // The batch that is chosen now, and nothing before it: the transfers that were there when the files were chosen (sent, cancelled,
            // failed) are the Transfers tab's, and what is still going stays until it ends (see `view::of_this_run`).
            let batch = batch_now.clone();
            let prepared: Vec<String> = view::list_under_circle(&listed_names, &batch_now)
                .into_iter()
                .filter_map(|row| match row {
                    view::ListRow::Prepared(name) => Some(name),
                    view::ListRow::Transfer(_) => None,
                })
                .collect();
            Transfers {
                context: true,
                items: batch,
                prepared,
                // The files behind a code are not the page's to take from or add to: they are only named.
                actions: files_listed.then(|| file_actions.clone()),
                ..transfers(&self.link, &self.view, palette)
            }
        };
        let mut page = rect()
            .width(Size::fill())
            .spacing(self.form.page_section_gap());
        if self.form.send_is_the_circle_alone() {
            // A phone held upright: the title small above, then the circle, which is itself the button that chooses.
            // The saved devices are under it, and the pairing is in Peers, a tap away at the bottom.
            page = page
                .cross_align(Alignment::center())
                .child(title_above_circle(self.form, hero, disc));
            if let Some(said) = *notice.read() {
                page = page.child(
                    label()
                        .text(said.words())
                        .color(palette.muted)
                        .font_size(14.),
                );
            }
            // What is going on or has gone is the circle's own business, so it comes right under it, before the devices: where a transfer
            // can be followed and stopped without scrolling past a list of devices.
            page = page.child(files_list);
            // The devices whose pairing was finished, under that: to send to one of them is a tap, and they are the only ones here. A
            // device only seen in the history is not, until it has been paired.
            page = page
                .child(section("OR PICK A SAVED DEVICE", palette))
                .child(devices);
        } else {
            page = page.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::center())
                    .content(Content::flex())
                    .child(hero)
                    .child(disc),
            );
            // What the last batch left out, in a sentence: a folder dropped in, a file that is gone.
            if let Some(said) = *notice.read() {
                page = page.child(
                    label()
                        .text(said.words())
                        .color(palette.muted)
                        .font_size(14.),
                );
            }
            page = page
                .child(files_list)
                .child(section("OR PICK A SAVED DEVICE", palette))
                .child(devices);
        }
        // Dropping files anywhere on the page, and pasting with Ctrl+V (or Command+V), are two more
        // ways in, and end in the same `take` as the dialog.
        let on_drop = take.clone();
        let on_paste = take;
        let paste_link = self.link.clone();
        frame(
            palette,
            self.form,
            rect()
                .width(Size::fill())
                .on_file_drop(move |e: Event<FileEventData>| on_drop.call(e.file_paths.clone()))
                .on_global_key_down(move |e: Event<KeyboardEventData>| {
                    if intake::is_paste(e.code, e.modifiers) {
                        let on_paste = on_paste.clone();
                        // Reading the clipboard is an effect too: a read-only window never asks
                        // it for anything and answers the paste with the read-only sentence.
                        if paste_link.read_only() {
                            on_paste.call(Vec::new());
                            return;
                        }
                        // Reading the clipboard may block while its owner answers, so it is done on
                        // the program's own executor (the window's loop is not a tokio runtime) and
                        // the window keeps drawing while it waits.
                        let reading = paste_link.blocking(clipboard_files::read);
                        spawn(async move {
                            if let Ok(found) = reading.await
                                && !found.is_empty()
                            {
                                on_paste.call(found);
                            }
                        });
                    }
                })
                .child(page),
        )
    }
}

#[derive(PartialEq)]
pub(crate) struct DeviceCard {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) picture: Option<avatar_view::Shared>,
    pub(crate) presence: Presence,
    /// What its extensions say of it, one line under the presence (see `view::Device::status_line`).
    pub(crate) status: Option<String>,
    pub(crate) picked: bool,
    pub(crate) palette: Palette,
    pub(crate) form: view::Form,
    pub(crate) on_choose: EventHandler<String>,
}

impl Component for DeviceCard {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut hovered = use_state(|| false);
        let (reachable, id, on_choose) = (
            self.presence.reachable(),
            self.id.clone(),
            self.on_choose.clone(),
        );
        let (word, word_colour) = palette.of_presence(&self.presence);
        rect()
            .key(self.id.clone())
            .width(self.form.device_card_width().map_or(Size::fill(), Size::px))
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(14.)
            .corner_radius(14.)
            .opacity(if reachable { 1. } else { 0.55 })
            .background(if hovered() && reachable {
                palette.raised
            } else {
                palette.surface
            })
            .border(
                Border::new()
                    .fill(if self.picked {
                        palette.send
                    } else {
                        palette.border
                    })
                    .width(if self.picked { 2. } else { 1. })
                    .alignment(BorderAlignment::Inner),
            )
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| {
                if reachable {
                    on_choose.call(id.clone());
                }
            })
            .child(avatar_view::Avatar {
                picture: self.picture.as_ref().map(|shared| shared.0.clone()),
                name: self.name.clone(),
                size: 40.,
                palette,
            })
            .child(
                rect()
                    .child(
                        label()
                            .text(self.name.clone())
                            .color(palette.text)
                            .font_size(17.)
                            .font_weight(FontWeight::BOLD),
                    )
                    .child(mono(i18n::tr(word), word_colour, 11.))
                    .maybe_child(self.status.clone().map(|status| {
                        label()
                            .text(status)
                            .color(palette.muted)
                            .font_size(12.)
                            .max_lines(1)
                            .text_overflow(TextOverflow::Ellipsis)
                    })),
            )
    }
}
