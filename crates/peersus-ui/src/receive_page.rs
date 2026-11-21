//! The Receive page: pairing by code or camera, approving a device, and the offers of files and their transfers.
use super::*;

#[derive(PartialEq)]
pub(crate) struct ReceivePage {
    pub(crate) link: Link,
    pub(crate) view: View,
    pub(crate) palette: Palette,
    pub(crate) form: view::Form,
    /// The width the window has, in pixels, from which the portal's size is decided.
    pub(crate) room: f32,
    /// The platform's camera, for reading the code of another device; absent where there is none.
    pub(crate) camera: Option<link::CameraControl>,
    /// Where the window is on its way to after a file was accepted: this page carries out the last step, to its list of files.
    pub(crate) jump: State<view::ReceiveJump>,
}

/// Everything that reading another device's code with the camera needs, kept in one place so that Receive and Peers do
/// the same thing the same way: whether the camera is on, the picture of what it sees, and how to start and stop it.
///
/// A code that is read is joined once and the camera is closed; a refusal is said in the circle. The camera is closed when
/// the component that uses this is left, so that it does not stay on with nobody to read it.
pub(crate) struct CodeScan {
    pub(crate) looking: State<view::Looking>,
    pub(crate) picture: State<Option<Arc<preview::Shown>>>,
    pub(crate) open: EventHandler<()>,
    pub(crate) stop: EventHandler<()>,
}

pub(crate) fn use_code_scan(
    link: &Link,
    camera: Option<link::CameraControl>,
    room: f32,
    owner_said: State<Option<String>>,
) -> CodeScan {
    let mut looking: State<view::Looking> = use_state(view::Looking::default);
    // What the camera sees, as the circle shows it: the latest picture, empty until the first frame and again after.
    let mut picture: State<Option<Arc<preview::Shown>>> = use_state(|| None);
    // The scan in progress, kept where Stop can reach it: stopping ends it from here, whatever the watcher is doing.
    let scan: std::rc::Rc<std::cell::RefCell<Option<scanner::Scanner>>> =
        use_hook(Default::default);
    // The disc's size on the glass in pixels, which is the side of the picture made for it: a picture larger than the disc
    // is wasted work and a smaller one is stretched.
    let side = qr::disc_physical(
        view::Form::portal_extent(qr::PORTAL_OUTER, room),
        *Platform::get().current_window().scale_factor.peek(),
    )
    .round() as u32;
    let open = EventHandler::new({
        let (camera, link, scan) = (camera.clone(), link.clone(), scan.clone());
        move |()| {
            let Some(camera) = camera.clone() else { return };
            if looking() == view::Looking::Scanning {
                return;
            }
            let scanner = scanner::Scanner::new();
            *scan.borrow_mut() = Some(scanner.clone());
            (camera.open)(scanner.clone());
            looking.set(view::Looking::Scanning);
            picture.set(None);
            let link = link.clone();
            // Two minutes is far more than reading a code takes, and it ends a camera that nobody is pointing.
            let (mut ticks, mut shown) = link::await_scan_shown(
                scanner,
                camera,
                link::LOOK_EVERY,
                link::CAMERA_PATIENCE,
                side,
            );
            spawn(async move {
                while let Some(tick) = ticks.recv().await {
                    looking.set(tick.looking);
                    if let Some(invitation) = tick.join {
                        // The owner's answer is waited for off this loop, so that the next tick of the camera is not held up by it.
                        let (joined, mut said) = (link.join(invitation), owner_said);
                        said.set(None);
                        spawn(async move {
                            if let Err(reason) = joined.await {
                                said.set(Some(reason));
                            }
                        });
                    }
                    if tick.close {
                        picture.set(None);
                        break;
                    }
                }
            });
            // The pictures are waited on without a timer: a `watch` wakes its reader when the value is replaced, which
            // needs no clock, and the reader stops when the scan has ended and the sender is gone.
            spawn(async move {
                while shown.changed().await.is_ok() {
                    let latest = shown.borrow_and_update().clone();
                    if let Some(latest) = latest {
                        picture.set(Some(latest));
                    }
                }
            });
        }
    });
    // Stop: the person ends the scan, and the camera is closed with nothing joined. The watcher's own end is the same
    // thing from the other side; the scanner is stopped here as well, so that what a frame in flight finds is dropped.
    let stop = EventHandler::new({
        let (camera, scan) = (camera.clone(), scan.clone());
        move |()| {
            let tick = view::stop(looking());
            if let Some(scanner) = scan.borrow_mut().take() {
                scanner.stop();
            }
            if tick.close
                && let Some(camera) = camera.clone()
            {
                (camera.close)();
            }
            looking.set(tick.looking);
            picture.set(None);
        }
    });
    // The page is left with the camera open: it is closed, so that it does not stay on with nobody to read it.
    if let Some(camera) = camera {
        let scan = scan.clone();
        use_drop(move || {
            if let Some(scanner) = scan.borrow_mut().take() {
                scanner.stop();
            }
            (camera.close)();
        });
    }
    CodeScan {
        looking,
        picture,
        open,
        stop,
    }
}

/// What the circle of Receive holds while the camera is on: the picture of what it sees, filling the disc, a dot on each
/// corner of every code the camera has found, and a button to stop.
///
/// The dots are what tells the person how to aim. A code that is found but not yet read (too far, too near, at a slant)
/// is marked all the same, so that they can see it is seen and move until it reads; no code marked means none is found,
/// and that is to move until one is. The picture is the camera's own, turned upright, and only its centre is shown, which
/// is also where a code must be to be marked: a code half out of the circle is not marked, since a mark with a corner
/// missing would say it is where it is not.
#[derive(PartialEq)]
pub(crate) struct CameraCircle {
    pub(crate) picture: Arc<preview::Shown>,
    /// The width the window has, in pixels, from which the disc's size is decided, as for the portal.
    pub(crate) room: f32,
    pub(crate) palette: Palette,
    pub(crate) on_stop: EventHandler<()>,
}

impl Component for CameraCircle {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let extent = view::Form::portal_extent(qr::PORTAL_OUTER, self.room);
        let disc = qr::PORTAL_DISC * extent / qr::PORTAL_OUTER;
        // Made on every draw and not kept: a handle kept by a memo that captured the first picture would go on showing it,
        // which is the mistake `qr::Drawn` was written against. It costs one copy of a picture about as large as the disc,
        // a few times a second, which is what the picture is.
        let handle = qr::portal_handle(&self.picture.picture);
        let mut camera = rect()
            .width(Size::px(disc))
            .height(Size::px(disc))
            .corner_radius(disc / 2.);
        if let Some(handle) = handle {
            camera = camera.child(
                image(handle)
                    .sampling_mode(SamplingMode::Nearest)
                    .width(Size::px(disc))
                    .height(Size::px(disc)),
            );
        }
        // A dot on every corner of every code that was found, in the green of "good".
        const DOT: f32 = 12.;
        for code in &self.picture.codes {
            for &corner in code {
                let (left, top) = preview::dot_origin(corner, self.picture.picture.side, disc, DOT);
                camera = camera.child(
                    rect()
                        .position(Position::new_absolute().left(left).top(top))
                        .width(Size::px(DOT))
                        .height(Size::px(DOT))
                        .corner_radius(DOT / 2.)
                        .background(palette.good)
                        .border(
                            Border::new()
                                .fill(palette.scanner_outline)
                                .width(2.)
                                .alignment(BorderAlignment::Inner),
                        ),
                );
            }
        }
        let on_stop = self.on_stop.clone();
        // Stop sits at the bottom of the circle, over the picture, where a thumb finds it.
        camera.child(
            rect()
                .position(
                    Position::new_absolute()
                        .left(disc / 2. - 44.)
                        .top(disc - 64.),
                )
                .child(
                    Button::new()
                        .on_press(move |_| on_stop.call(()))
                        .child(crate::i18n::tr("Stop")),
                ),
        )
    }
}

impl Component for ReceivePage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let accent = palette.receive;
        let form = self.form;
        let mut join_text = use_state(String::new);
        // What the owner said when it would not take the code up: shown under the field, and gone with the next try.
        let mut owner_said: State<Option<String>> = use_state(|| None);
        let join_link = self.link.clone();
        let join = move |_| {
            // Only what plainly is an invitation goes to the owner, in the form it reads. If it
            // is not one, the field is left as it is, so that the person can see and mend it.
            let code = qr::invitation_to_join(&join_text.read());
            if let Some(code) = code {
                owner_said.set(None);
                join_text.set(String::new());
                let joined = join_link.join(code);
                spawn(async move {
                    if let Err(reason) = joined.await {
                        owner_said.set(Some(reason));
                    }
                });
            }
        };

        // The circle makes no code: a device that receives has nothing to show. It is the place where a code from the
        // other device is taken: pasted below, or, on a phone, read by its camera. Pairing is in Peers.
        let camera = self.camera.clone();
        let CodeScan {
            looking,
            picture,
            open: open_camera,
            stop: stop_camera,
        } = use_code_scan(&self.link, camera.clone(), self.room, owner_said);
        let shown_now = picture.read().clone();
        let words = |headline: &'static str, line: &'static str| -> Vec<Element> {
            vec![
                mono(i18n::tr("READY TO RECEIVE"), palette.portal_text, 12.).into(),
                label()
                    .text(i18n::tr(headline))
                    .color(palette.portal_text)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(i18n::tr(line))
                    .color(palette.portal_text)
                    .font_size(14.)
                    .into(),
            ]
        };
        let mut camera_in_circle = false;
        let centre: Vec<Element> = if camera.is_some() {
            match view::circle_for(looking(), shown_now.is_some()) {
                view::Circle::Words(headline, line) => words(headline, line),
                // One small line and nothing else, for the moment the camera takes to give its first frame.
                view::Circle::Starting(line) => vec![
                    label()
                        .text(i18n::tr(line))
                        .color(palette.portal_text)
                        .font_size(14.)
                        .into(),
                ],
                // The camera, and nothing written over it.
                view::Circle::Camera => match shown_now {
                    Some(picture) => {
                        camera_in_circle = true;
                        vec![
                            CameraCircle {
                                picture,
                                room: self.room,
                                palette,
                                on_stop: stop_camera.clone(),
                            }
                            .into(),
                        ]
                    }
                    None => Vec::new(),
                },
            }
        } else {
            vec![
                mono(i18n::tr("READY TO RECEIVE"), palette.portal_text, 12.).into(),
                label()
                    .text(i18n::tr("PASTE"))
                    .color(palette.portal_text)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(i18n::tr("THE CODE"))
                    .color(palette.portal_text)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(i18n::tr("from the other device"))
                    .color(palette.portal_text)
                    .font_size(14.)
                    .into(),
            ]
        };

        // The title and its two lines are kept on every screen, at a size that fits the one they are on: a
        // phone's is smaller, so that they do not take the room the portal needs.
        let hero = rect()
            .width(if form.page_puts_portal_beside_text() { Size::flex(1.) } else { Size::fill() })
            .spacing(if form.page_puts_portal_beside_text() { 16. } else { 8. })
            .child(headline("Ready when", "you are.", palette, accent, form))
            .child(
                label().text(crate::i18n::tr("Choose what you accept. Keep everything on your terms. Nothing arrives until you say so."))
                    .color(palette.muted)
                    .font_size(if form.page_puts_portal_beside_text() { 20. } else { 15. }),
            );

        let paste = field::field(field::Profile::ADDRESS, join_text)
            .placeholder(crate::i18n::tr("Paste the code"))
            .width(if form.page_puts_portal_beside_text() {
                Size::px(520.)
            } else {
                Size::fill()
            });
        let join_button = Button::new().on_press(join).child(crate::i18n::tr("Join"));
        let join_row: Element = if form.page_puts_portal_beside_text() {
            rect()
                .horizontal()
                .spacing(10.)
                .child(paste)
                .child(join_button)
                .into()
        } else {
            rect()
                .width(Size::fill())
                .spacing(10.)
                .child(paste)
                .child(join_button.width(Size::fill()))
                .into()
        };
        // With the camera in the circle a press on it does nothing: Stop is the one way to end a scan, and a touch that
        // restarted the camera would be a button that does something other than what it is.
        let on_press = (camera.is_some() && !camera_in_circle).then(|| open_camera.clone());
        let portal = portal(
            palette,
            accent,
            on_press,
            centre,
            view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
            self.link.client().snapshot().settings.reduced_motion,
        );
        let introduction: Element = if form.page_puts_portal_beside_text() {
            rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .child(hero)
                .child(portal)
                .into()
        } else {
            // A phone: the title small and short above the portal, which is centred under it, as on Send.
            title_above_circle(form, hero, portal).into()
        };
        let mut sections = rect()
            .width(Size::fill())
            .spacing(self.form.page_section_gap())
            .child(introduction)
            .child(
                rect()
                    .width(Size::fill())
                    .spacing(10.)
                    .child(section("HAVE A CODE FROM ANOTHER DEVICE?", palette))
                    .child(join_row)
                    .maybe_child(owner_said.read().clone().map(|reason| {
                        label()
                            .text(i18n::tr(&reason).to_owned())
                            .color(palette.bad)
                            .font_size(14.)
                    })),
            );
        // Pasting a code belongs to the Receive circle, before the bounded list of incoming files.
        // Where the page and its list begin on the glass, measured as they are laid out: the difference is how far down the page the list is,
        // which is where the page goes after a file was accepted (see `view::ReceiveJump`). Both are measured in the same layout, so the
        // difference does not depend on where the page is scrolled to.
        let mut origin: State<Option<f32>> = use_state(|| None);
        let mut list_top: State<Option<f32>> = use_state(|| None);
        let mut go_to: State<Option<f32>> = use_state(|| None);
        let listed = !self.view.transfers.is_empty();
        if listed {
            sections = sections.child(
                rect()
                    .width(Size::fill())
                    .on_sized(move |e: Event<SizedEventData>| {
                        list_top.set_if_modified(Some(e.area.min_y()))
                    })
                    .child(transfers_here(&self.link, &self.view, palette)),
            );
        }
        let sections = sections
            .on_sized(move |e: Event<SizedEventData>| origin.set_if_modified(Some(e.area.min_y())));
        let mut jump = self.jump;
        let (from, to) = (*origin.read(), *list_top.read());
        if jump() == view::ReceiveJump::ToList
            && listed
            && let (Some(from), Some(to)) = (from, to)
        {
            go_to.set(Some((to - from).max(0.)));
            jump.set(view::ReceiveJump::Nowhere);
        }
        Scrolling {
            palette,
            form: self.form,
            content: sections.into_element(),
            see_through: false,
            tops: None,
            go_to: Some(go_to),
        }
    }
}

/// Where a device is paired, at the top of Peers: the button that asks for a code, the line that says nothing
/// moves before an approval, and the code itself once it exists, to paste on the other device. Pairing is the
/// way a device becomes trusted for good, and it lives here, with the devices it adds to.
#[derive(PartialEq)]
pub(crate) struct PairingSection {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
    /// The width the window has, in pixels, from which the size of the portal with the code is decided.
    pub(crate) room: f32,
    pub(crate) form: view::Form,
    /// The platform's camera, for reading the code of the other device; absent where there is none.
    pub(crate) camera: Option<link::CameraControl>,
}

/// The side of pairing that TAKES a code instead of showing one: the code of the other device, read by the camera where
/// there is one (the circle) and pasted where there is not, and joined only if it plainly is an invitation. The same
/// way Receive takes a code, through the same hook, so what a scan does is decided in one place.
#[derive(PartialEq)]
pub(crate) struct CodeTaker {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
    pub(crate) accent: (u8, u8, u8),
    pub(crate) form: view::Form,
    pub(crate) room: f32,
    pub(crate) camera: Option<link::CameraControl>,
}

impl Component for CodeTaker {
    fn render(&self) -> impl IntoElement {
        let (palette, accent, form) = (self.palette, self.accent, self.form);
        let mut text = use_state(String::new);
        let mut refused = use_state(|| false);
        // What the owner said when it would not take the code up: shown under the field, and gone with the next try.
        let mut owner_said: State<Option<String>> = use_state(|| None);
        let join_link = self.link.clone();
        let join = move |_| {
            // Only what plainly is an invitation goes to the owner, in the form it reads. If it is not one, the field is
            // left as it is, so that the person can see and mend it, and is told so.
            let code = qr::invitation_to_join(&text.read());
            match code {
                Some(code) => {
                    refused.set(false);
                    owner_said.set(None);
                    text.set(String::new());
                    let joined = join_link.join(code);
                    spawn(async move {
                        if let Err(reason) = joined.await {
                            owner_said.set(Some(reason));
                        }
                    });
                }
                None => refused.set(true),
            }
        };
        let CodeScan {
            looking,
            picture,
            open,
            stop,
        } = use_code_scan(&self.link, self.camera.clone(), self.room, owner_said);
        let shown_now = picture.read().clone();
        let words = |headline: &'static str, line: &'static str| -> Vec<Element> {
            vec![
                mono(i18n::tr("CONNECT"), palette.portal_text, 12.).into(),
                label()
                    .text(i18n::tr(headline))
                    .color(palette.portal_text)
                    .font_size(34.)
                    .font_weight(FontWeight::BOLD)
                    .into(),
                label()
                    .text(i18n::tr(line))
                    .color(palette.portal_text)
                    .font_size(14.)
                    .into(),
            ]
        };
        let mut camera_in_circle = false;
        let centre: Vec<Element> = match view::circle_for(looking(), shown_now.is_some()) {
            view::Circle::Words(headline, line) => words(headline, line),
            view::Circle::Starting(line) => vec![
                label()
                    .text(i18n::tr(line))
                    .color(palette.portal_text)
                    .font_size(14.)
                    .into(),
            ],
            view::Circle::Camera => match shown_now {
                Some(picture) => {
                    camera_in_circle = true;
                    vec![
                        CameraCircle {
                            picture,
                            room: self.room,
                            palette,
                            on_stop: stop.clone(),
                        }
                        .into(),
                    ]
                }
                None => Vec::new(),
            },
        };
        let paste = field::field(field::Profile::ADDRESS, text)
            .placeholder(crate::i18n::tr("Paste the code"))
            .width(if form.page_puts_portal_beside_text() {
                Size::px(520.)
            } else {
                Size::fill()
            });
        let join_button = Button::new().on_press(join).child(crate::i18n::tr("Join"));
        let join_row: Element = if form.page_puts_portal_beside_text() {
            rect()
                .horizontal()
                .spacing(10.)
                .child(paste)
                .child(join_button)
                .into()
        } else {
            rect()
                .width(Size::fill())
                .spacing(10.)
                .child(paste)
                .child(join_button.width(Size::fill()))
                .into()
        };
        let mut taker = rect().width(Size::fill()).spacing(12.);
        // The circle is only where there is a camera to read with; elsewhere the code is pasted.
        if self.camera.is_some() {
            let on_press = (!camera_in_circle).then(|| open.clone());
            taker = taker.child(
                rect()
                    .width(Size::fill())
                    .cross_align(Alignment::center())
                    .child(portal(
                        palette,
                        accent,
                        on_press,
                        centre,
                        view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
                        self.link.client().snapshot().settings.reduced_motion,
                    )),
            );
        }
        taker = taker
            .child(section("OR PASTE THE CODE FROM THE OTHER DEVICE", palette))
            .child(join_row);
        if *refused.read() {
            taker = taker.child(
                label()
                    .text(crate::i18n::tr("That is not a code from another device."))
                    .color(palette.muted)
                    .font_size(14.),
            );
        }
        if let Some(reason) = owner_said.read().clone() {
            taker = taker.child(
                label()
                    .text(i18n::tr(&reason).to_owned())
                    .color(palette.bad)
                    .font_size(14.),
            );
        }
        taker
    }
}

impl Component for PairingSection {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let accent = palette.send;
        // The code is only ever in the reply to the command, never in the snapshot, so it is awaited here.
        let mut invitation: State<Option<String>> = use_state(|| None);
        let mut failure: State<Option<String>> = use_state(|| None);
        // Which side is open: this device's code, the other device's code, or neither. One state, so that the two are never open
        // together.
        let mut mode: State<view::PairMode> = use_state(view::PairMode::default);
        let client = self.link.client().clone();
        // A code lives five minutes in the owner, which refuses it itself at its deadline (`Admission::new`). The window makes no new one by itself: a
        // code that nobody asked for would be a pairing left open that nobody is looking at. When its life is over it goes off the screen, and Pair is
        // the button that asks for another (`PairMode::pair_asks_for_a_code` is true again, with no code on show). `generation` tells the timer of a
        // code that was replaced since (by Pair again, or by another device or the command line) not to take the screen of the one that replaced it.
        let generation: State<u64> = use_state(|| 0);
        // When the code on show came: its fuse is measured from then.
        let mut made_at: State<Option<std::time::Instant>> = use_state(|| None);
        // Whether a code has been asked for and has not come yet.
        let mut asking: State<bool> = use_state(|| false);
        let create = EventHandler::new({
            let client = client.clone();
            move |()| {
                let client = client.clone();
                let mut generation = generation;
                let mine = *generation.peek() + 1;
                generation.set(mine);
                asking.set(true);
                spawn(async move {
                    let answer = client.command(Command::CreateInvitation {}).await;
                    // However it was answered, the code is no longer on its way.
                    asking.set(false);
                    match answer {
                        Ok(Reply::Invitation(code)) => {
                            failure.set(None);
                            made_at.set(Some(std::time::Instant::now()));
                            invitation.set(Some(code));
                            // At the end of its life: the code leaves the screen, if it is still the code on show. Nothing is asked for in its place.
                            let mut over = link::after(std::time::Duration::from_secs(
                                view::CODE_LIFE_SECONDS,
                            ));
                            spawn(async move {
                                if (&mut over).await.is_ok() && *generation.peek() == mine {
                                    invitation.set(None);
                                }
                            });
                        }
                        // Never swallowed: an error nobody can see is a button that seems to do nothing.
                        Ok(_) => {
                            failure.set(Some("The owner answered with something else.".to_owned()))
                        }
                        Err(error) => failure.set(Some(error.to_string())),
                    }
                });
            }
        });
        // Pair shows this device's code and takes the other side's off the screen, then asks for the code: but only when there is none on show. A
        // press on it while a code is showing changes nothing (see `PairMode::pair_asks_for_a_code`); when the code is gone, it asks again.
        let create = EventHandler::new({
            let create = create.clone();
            move |()| {
                // A code that has been asked for and has not come yet counts as one on its way: a second press before the owner answers asks for nothing.
                let on_its_way = invitation.peek().is_some() || *asking.peek();
                let asks = mode.peek().pair_asks_for_a_code(on_its_way);
                let next = mode.peek().after_pair();
                mode.set(next);
                if asks {
                    create.call(());
                }
            }
        });
        // The fuse and the sparks of its tip burn only while this device's code is on show (see `use_fuse`, `use_sparks`): from what the code has left
        // at that moment to nothing at the end of its life, begun again for each code (`generation` is what a new code changes), so that the fuse is
        // gone at the moment the code leaves the screen and the owner stops admitting it. With no code on show neither runs: a fuse that ran for the
        // life of a code nobody had asked for kept the whole window redrawing for five minutes each time this page was opened. With reduced motion
        // nothing moves: the fuse is drawn whole, and the code still leaves the screen on its time.
        let reduced = self.link.client().snapshot().settings.reduced_motion;
        let burning = (mode.read().shows_code() && invitation.read().is_some())
            .then(|| *made_at.read())
            .flatten();
        let (burns, left, remaining) = match burning {
            Some(made_at) => {
                let life = std::time::Duration::from_secs(view::CODE_LIFE_SECONDS);
                let left = life.saturating_sub(made_at.elapsed());
                (
                    Some(*generation.read()),
                    left.as_secs_f32() / life.as_secs_f32(),
                    left.as_secs(),
                )
            }
            None => (None, 0., 0),
        };
        let fuse = use_fuse(burns, left, remaining, reduced);
        let sparks = use_sparks(reduced, burns.is_some());
        // The drawing is kept between draws, together with what it was made for. It is read through the cache and
        // not through a memo that captures a size: a memo does not see the size change when the window is turned.
        let drawn = use_state(|| std::rc::Rc::new(std::cell::RefCell::new(qr::Drawn::default())));
        let disc = qr::disc_physical(
            view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
            *Platform::get().current_window().scale_factor.peek(),
        );
        let shown = mode
            .read()
            .shows_code()
            .then(|| invitation.read().clone())
            .flatten();
        let picture = shown.as_deref().and_then(|code| {
            drawn.read().borrow_mut().of(code, disc).map(|handle| {
                qr::picture_from(
                    handle,
                    *Platform::get().current_window().scale_factor.peek(),
                )
            })
        });
        let mut section_box = rect()
            .width(Size::fill())
            .spacing(12.)
            .child(
                // Two equal halves, as on the site; each says its word in the middle.
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(12.)
                    .child(rect().width(Size::flex(1.)).child(Solid {
                        text: "Pair",
                        accent,
                        palette,
                        enabled: true,
                        on_press: create,
                    }))
                    .child(rect().width(Size::flex(1.)).child(Solid {
                        text: "Connect",
                        accent: palette.receive,
                        palette,
                        enabled: true,
                        on_press: EventHandler::new(move |()| {
                            let next = mode.peek().after_connect();
                            mode.set(next);
                        }),
                    })),
            )
            .child(mono(i18n::tr(view::PAIR_EXPLAINS), palette.muted, 12.))
            .child(mono(i18n::tr(view::PAIR_WARNS), palette.muted, 12.));
        if let Some(said) = failure.read().clone() {
            section_box = section_box.child(
                label()
                    .text(i18n::tr(&said).to_owned())
                    .color(palette.muted)
                    .font_size(14.),
            );
        }
        if let (true, Some(picture)) = (mode.read().shows_code(), picture) {
            // The code as a picture in the portal, which is how a code this long is carried to another device. The ring round it is a fuse that
            // burns down over the whole life of the code; when it is gone the code leaves the screen, and Pair is pressed for another.
            let extent = view::Form::portal_extent(qr::PORTAL_OUTER, self.room);
            // With reduced motion nothing moves: the fuse is shown whole (`use_fuse` gives 1 itself) and the sparks stand at one phase (`use_sparks`
            // gives that frame itself); the code still leaves the screen on its time.
            let life: f32 = fuse;
            let frame = sparks;
            section_box = section_box.child(
                rect()
                    .width(Size::fill())
                    .cross_align(Alignment::center())
                    .child(portal_with_life(
                        palette,
                        accent,
                        vec![picture],
                        extent,
                        life,
                        frame,
                    )),
            );
        }
        if let Some(code) = shown {
            // The code to paste on the other device, as text that can be selected.
            section_box = section_box
                .child(section("OR PASTE THIS CODE ON THE OTHER DEVICE", palette))
                .child(
                    rect()
                        .width(Size::fill())
                        .padding(12.)
                        .corner_radius(10.)
                        .background(palette.surface)
                        .border(
                            Border::new()
                                .fill(palette.border)
                                .width(1.)
                                .alignment(BorderAlignment::Inner),
                        )
                        .child(
                            SelectableText::new()
                                .span(
                                    Span::new(code)
                                        .color(palette.text)
                                        .font_size(11.)
                                        .font_family("monospace"),
                                )
                                .width(Size::fill()),
                        ),
                );
        }
        // The other side of pairing: the code of the device that is pairing with this one, scanned or pasted. Without it a
        // device that was shown a code has no way to answer it from here.
        if mode.read().takes_code() {
            section_box = section_box
                .child(section("CONNECT TO THE OTHER DEVICE", palette))
                .child(CodeTaker {
                    link: self.link.clone(),
                    palette,
                    accent: palette.receive,
                    form: self.form,
                    room: self.room,
                    camera: self.camera.clone(),
                });
        }
        section_box
    }
}

#[derive(PartialEq)]
pub(crate) struct ApprovalCard {
    pub(crate) link: Link,
    pub(crate) approval: Approval,
    pub(crate) palette: Palette,
}

impl Component for ApprovalCard {
    fn render(&self) -> impl IntoElement {
        let (approval, palette) = (&self.approval, self.palette);
        let meta = format!(
            "{} · {}s",
            i18n::tr(if approval.answered {
                "ANSWERED, WAITING FOR THE OTHER DEVICE"
            } else {
                "PICK THE SAME NUMBER ON BOTH DEVICES"
            }),
            approval.seconds_left
        );
        let mut card = rect()
            .key(approval.request.clone())
            .width(Size::fill())
            .padding(20.)
            .spacing(12.)
            .corner_radius(14.)
            .background(palette.surface)
            // It floats over the page with the other cards of the stack.
            .shadow(Shadow::new().y(4.).blur(16.).color((0, 0, 0, 90)))
            .border(
                Border::new()
                    .fill(palette.receive)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .child(
                label()
                    .text(i18n::format(
                        i18n::language(),
                        "{name} wants to pair",
                        &[("name", &approval.name)],
                    ))
                    .color(palette.text)
                    .font_size(20.)
                    .font_weight(FontWeight::BOLD),
            )
            .child(mono(meta, palette.muted, 11.));
        if !approval.answered {
            let mut choices = rect().horizontal().spacing(10.);
            for number in approval.options {
                let link = self.link.clone();
                let request = approval.request.clone();
                choices = choices.child(
                    Button::new()
                        .filled()
                        .on_press(move |_| {
                            link.send(Command::ChoosePairing {
                                request: request.clone(),
                                number,
                            })
                        })
                        .child(
                            label()
                                .text(number.to_string())
                                .font_size(24.)
                                .font_weight(FontWeight::BOLD),
                        ),
                );
            }
            let link = self.link.clone();
            let request = approval.request.clone();
            choices = choices.child(
                Button::new()
                    .on_press(move |_| {
                        link.send(Command::RejectPairing {
                            request: request.clone(),
                        })
                    })
                    .child(crate::i18n::tr("Refuse")),
            );
            card = card.child(choices);
        }
        card
    }
}

/// The heading of a list of transfers, with Clear at its right end: it forgets the transfers that are over and keeps the ones
/// still going. It asks twice, because the record is gone for good, and is not there at all when nothing is over.
#[derive(PartialEq)]
pub(crate) struct TransfersHeader {
    pub(crate) link: Link,
    pub(crate) palette: Palette,
    pub(crate) can_clear: bool,
}

impl Component for TransfersHeader {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut asking = use_state(view::Confirm::default);
        let mut row = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex());
        row = row
            .child(section("TRANSFERS", palette))
            .child(rect().width(Size::flex(1.)));
        // Nothing over, nothing to clear: a button that does nothing is not shown, and a question left open is dropped with it.
        if !self.can_clear {
            if *asking.peek() != view::Confirm::Idle {
                asking.set(view::Confirm::Idle);
            }
            return row.into_element();
        }
        let confirming = *asking.read() == view::Confirm::Asking;
        let link = self.link.clone();
        let mut buttons = rect().horizontal().spacing(8.);
        if confirming {
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        let given_up = asking.peek().cancelled();
                        asking.set(given_up);
                    })
                    .child(crate::i18n::tr("Keep")),
            );
        }
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    let (next, now) = asking.peek().pressed();
                    asking.set(next);
                    if now {
                        link.send(Command::ClearHistory {});
                    }
                })
                .child(i18n::tr(if confirming { "Clear them?" } else { "Clear" })),
        );
        row.child(buttons).into_element()
    }
}

/// The heading of the list of transfers on Send and Receive. Where [`TransfersHeader`] forgets the record for good, this only puts the
/// list out of the way: while anything is going it folds the list into one line and opens it again (the transfer carries on), and once
/// everything is over it is X, which hides what is over from this page and nothing more. The record is untouched, and the Transfers
/// tab still lists all of it. What is hidden is kept here, by id, for as long as the page is open.
#[derive(PartialEq)]
pub(crate) struct ListHeader {
    pub(crate) action: view::HeaderAction,
    pub(crate) on_press: EventHandler<view::HeaderAction>,
}

impl Component for ListHeader {
    fn render(&self) -> impl IntoElement {
        let (action, on_press) = (self.action, self.on_press.clone());
        // The same small button for the three: only its words differ, and X is a letter and not a picture, so that it needs nothing drawn.
        let words = match action {
            view::HeaderAction::Minimize => "Minimize",
            view::HeaderAction::Expand => "Expand",
            view::HeaderAction::Hide => "\u{2715}",
        };
        rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)))
            .child(
                Button::new()
                    .on_press(move |_| on_press.call(action))
                    .child(crate::i18n::tr(words)),
            )
    }
}

/// The list of transfers, with what saving on its own is set to, from the settings the owner holds now. Complete, with Clear: the one
/// of Settings, which forgets the record on purpose.
pub(crate) fn transfers(link: &Link, view: &View, palette: Palette) -> Transfers {
    Transfers {
        link: link.clone(),
        items: view.transfers.clone(),
        palette,
        auto_save: view.settings.auto_save,
        save_folder: view.settings.save_folder.clone(),
        context: false,
        prepared: Vec::new(),
        actions: None,
    }
}

/// The list as it sits under the big circle on Send and Receive: what is going, and what is over until it is put away with X. Never
/// Clear: that forgets the record, and is in Settings and in the Transfers tab. Minimize while anything goes, X once all is over.
///
/// Send and Receive list only what was made since the window opened and what is still going (see `view::of_this_run`): one file sent is one row,
/// not the whole record, which is the Transfers tab's.
pub(crate) fn transfers_here(link: &Link, view: &View, palette: Palette) -> Transfers {
    let items = view::of_this_run(&view.transfers, &view.transfers_at_start);
    Transfers {
        context: true,
        items,
        ..transfers(link, view, palette)
    }
}

#[derive(PartialEq)]
pub(crate) struct Transfers {
    pub(crate) link: Link,
    pub(crate) items: Vec<Progress>,
    pub(crate) palette: Palette,
    /// Whether a file that is offered is saved on its own, and into which folder the person chose. From the settings.
    pub(crate) auto_save: bool,
    pub(crate) save_folder: Option<std::path::PathBuf>,
    /// Whether this is the list under the big circle on Send and Receive (Minimize and X, which only put it out of the way) and not the
    /// full list with Clear that forgets the record.
    pub(crate) context: bool,
    /// The names of the files that are listed to go and have not begun to: their rows, with the status prepared. Only the list under the circle of
    /// Send has any.
    pub(crate) prepared: Vec<String>,
    /// What the person can do with the files that wait, given by the page that owns them (see [`FileActions`]); `None` on every list that has no
    /// files of its own waiting.
    pub(crate) actions: Option<FileActions>,
}

/// What can be done with the files that are listed and wait for the circle, as three actions the page carries out on its one list of them: the list
/// that shows them keeps no rule about them. Compared by what they are for and not by what they do, since a handler cannot be compared.
#[derive(Clone)]
pub(crate) struct FileActions {
    /// The file at this place in the list is taken away.
    pub(crate) remove: EventHandler<usize>,
    /// Every file is taken away.
    pub(crate) clear: EventHandler<()>,
    /// The picker opens to add more files to the list.
    pub(crate) add_more: EventHandler<()>,
}

impl PartialEq for FileActions {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// The Accept button, with the line that says what stopped it. The same button on the offer over the window and on a transfer's
/// card, so that what Accept does, and what is said when it cannot, is decided in one place.
#[derive(PartialEq)]
pub(crate) struct AcceptButton {
    pub(crate) link: Link,
    pub(crate) ids: Vec<String>,
    pub(crate) auto_save: bool,
    pub(crate) save_folder: Option<std::path::PathBuf>,
    pub(crate) palette: Palette,
    /// The solid look, for the offer over the window; the card's own button is the plain one.
    pub(crate) filled: bool,
}

impl Component for AcceptButton {
    fn render(&self) -> impl IntoElement {
        let said: State<Option<String>> = use_state(|| None);
        let (link, ids, auto_save, chosen) = (
            self.link.clone(),
            self.ids.clone(),
            self.auto_save,
            self.save_folder.clone(),
        );
        let sink = try_consume_context::<link::FolderSink>();
        let carrying = try_consume_context::<Carrying>();
        let button = Button::new().on_press(move |_| {
            if Carrying::now(carrying) {
                return;
            }
            accept(
                &link,
                &ids,
                auto_save,
                chosen.as_deref(),
                sink.clone(),
                said,
            )
        });
        let button = if self.filled { button.filled() } else { button };
        // The column takes what is left beside the other button of its row and no more, so that the sentence that says why the folder was not
        // chosen wraps inside it: left to the width of its text it widened the row and pushed Cancel off the edge of the screen.
        rect()
            .width(Size::flex(1.))
            .spacing(6.)
            .maybe_child(
                said.read()
                    .clone()
                    .map(|words| label().text(words).color(self.palette.bad).font_size(13.)),
            )
            .child(button.child(crate::i18n::tr("Accept")))
    }
}

/// Answer an offer with Accept: where it goes is decided by the rule, and it is either saved there or the person is asked
/// for a folder, and only then is it accepted. Whatever stops it is said through `said`, in words, so that a button that cannot
/// do what it says is never silent: a folder dialog closed with no folder, the owner refusing the answer (the offer is gone, or
/// was answered already), a phone that has no dialog to choose a folder.
///
/// What the disk says of the chosen folder is looked at now, when the button is pressed and not when the page was drawn: a
/// folder that was there then can be gone.
pub(crate) fn accept(
    link: &Link,
    ids: &[String],
    auto_save: bool,
    chosen: Option<&std::path::Path>,
    sink: Option<link::FolderSink>,
    mut said: State<Option<String>>,
) {
    // Accepting a file looks at folders and writes where they say: a read-only window does
    // neither and says so in the same place a refusal is said.
    if link.read_only() {
        said.set(Some(link::READ_ONLY.to_owned()));
        return;
    }
    let state = chosen.map_or(view::Folder::Unusable, view::look_at);
    let (client, ids) = (link.client().clone(), ids.to_vec());
    // The folder is asked for once, for the first file: the others go where that one goes. Nothing to accept is nothing to do.
    let Some(first) = ids.first().cloned() else {
        return;
    };
    #[cfg(target_os = "android")]
    let (transfer, rest) = (first.clone(), ids.clone());
    #[cfg(not(target_os = "android"))]
    let _ = (&sink, &first);
    // Every file is accepted into `directory`. On a phone `directory` is the staging root, and each file gets a directory of its own beneath
    // it, which the platform empties into the folder the person chose once the file is whole; elsewhere all go to the one folder.
    let answer = move |directory: std::path::PathBuf, mut said: State<Option<String>>| async move {
        for id in ids {
            let directory = if cfg!(target_os = "android") {
                directory.join(&id)
            } else {
                directory.clone()
            };
            let _ = std::fs::create_dir_all(&directory);
            match client
                .command(Command::AcceptTransfer {
                    transfer: id,
                    directory,
                })
                .await
            {
                Ok(_) => said.set(None),
                Err(reason) => said.set(Some(reason.to_owned())),
            }
        }
    };
    match view::destination(auto_save, chosen, state) {
        view::Destination::Save(directory) => {
            said.set(None);
            spawn(answer(directory, said));
        }
        view::Destination::Ask { lost } => {
            #[cfg(not(target_os = "android"))]
            {
                said.set(lost.map(|_| {
                    i18n::tr("The folder chosen for incoming files is gone. Choose another.")
                        .to_owned()
                }));
                spawn(async move {
                    match rfd::AsyncFileDialog::new()
                        .set_title(i18n::tr("Save the file in"))
                        .pick_folder()
                        .await
                    {
                        Some(folder) => answer(folder.path().to_path_buf(), said).await,
                        None => said.set(Some(
                            i18n::tr("No folder was chosen, so the file was not accepted.")
                                .to_owned(),
                        )),
                    }
                });
            }
            // On a phone the folder is not a path the program can write: the person chooses it with the system's picker, the
            // file is received into the program's staging, and the system puts it in the chosen folder once it has arrived whole.
            #[cfg(target_os = "android")]
            {
                let _ = lost;
                let Some(sink) = sink else {
                    said.set(Some(
                        i18n::tr("Choosing a folder is not available on this device.").to_owned(),
                    ));
                    return;
                };
                // Saving on its own is on and the person chose a folder in Settings: the files go there without asking, which is what the
                // setting says. The same rule as the one for a path on a desktop: off, or no folder, and each file asks.
                if auto_save && let Some(tree) = (sink.saved)() {
                    said.set(None);
                    for id in &rest {
                        sink.picked.destined(id, tree.clone());
                    }
                    spawn(answer(sink.staging.clone(), said));
                    return;
                }
                // The system shows one folder picker at a time: a second Accept while one is open is told so, and does not
                // take the place of the first question (its answer would then be taken for the wrong file).
                if !sink.picked.opened(&transfer) {
                    said.set(Some(i18n::tr("A folder is being chosen for another file. Finish that first, then accept this one.").to_owned()));
                    return;
                }
                said.set(None);
                (sink.choose)();
                let answered = link::await_folder(
                    sink.picked.clone(),
                    transfer.clone(),
                    link::LOOK_EVERY,
                    link::PICKER_PATIENCE,
                );
                spawn(async move {
                    match answered.await {
                        Ok(Some(tree)) => {
                            // Received where the program can write, in a directory of its own for each file (two files of one name must not
                            // meet there); the system puts each in the folder chosen when it is whole. One choice serves every file ticked.
                            for id in &rest {
                                sink.picked.destined(id, tree.clone());
                            }
                            answer(sink.staging.clone(), said).await;
                        }
                        _ => said.set(Some(
                            i18n::tr("No folder was chosen, so the file was not accepted.")
                                .to_owned(),
                        )),
                    }
                });
            }
        }
    }
}

/// Whether the surface an offer is drawn on is being carried by the pointer, where it can be (the panel outside the window, on a
/// desktop). A pointer that took hold of the panel on one of its buttons and let go after moving it has moved the panel, and has
/// not pressed the button: the panel went with the pointer, so the button was still under it. True from the moment a hold became
/// a carry until the pointer goes down again. Absent wherever nothing is carried, which is the same as false.
#[derive(Clone, Copy)]
pub(crate) struct Carrying(pub(crate) State<bool>);

impl Carrying {
    pub(crate) fn now(carrying: Option<Self>) -> bool {
        carrying.is_some_and(|carrying| *carrying.0.peek())
    }
}

/// A file offered to this person, over the whole window, fixed at its bottom and above the navigation: on whatever screen they
/// are on, and not as a row under a circle where scanning a code left it unseen. It names who sends what and has the two
/// answers, Accept and Cancel. It is a decision with a clock on it (an offer unanswered in two minutes is refused).
#[derive(PartialEq)]
pub(crate) struct IncomingOffer {
    pub(crate) link: Link,
    pub(crate) offered: view::Offered,
    pub(crate) palette: Palette,
    /// The second now, Unix seconds, from the window's one clock: the bar is drawn again each second it changes, and the time
    /// left is worked out from it and from the deadline the owner reported, so that it is the same number a plugin reads.
    pub(crate) now: u64,
    /// What the buttons of the notification asked of the window, where the platform has such buttons (Android).
    pub(crate) buttons: Option<link::OfferButtons>,
    /// How round the corners are: none for the bar in the window, which runs from side to side of it; round for the panel that
    /// stands on its own outside the window.
    pub(crate) corner: f32,
}

impl Component for IncomingOffer {
    fn render(&self) -> impl IntoElement {
        let (palette, offer) = (self.palette, &self.offered.offer);
        // The files of the offer, and which of them are ticked: all of them at first, so that Accept with nothing touched takes the whole offer.
        // Kept in state, since the bar is drawn again every second by the window's clock and a tick must stay where the person put it.
        let files = &self.offered.files;
        let mut listing = use_state(|| false);
        // View files pressed on the notification brought the window forward: the list opens with it, since the person pressed it to see the
        // files. Asked at each drawing, and the window draws this bar every second; the answer is taken once, so a later closing stays closed.
        let said_on_accept: State<Option<String>> = use_state(|| None);
        if let Some(buttons) = &self.buttons {
            if (buttons.take_view)() && !*listing.peek() {
                listing.set(true);
            }
            // An Accept pressed on the notification or the panel that needed a window to ask for a folder: the window is here now, and asks.
            // Taken once, so a second drawing does not ask again.
            let pressed = (buttons.take_accepts)();
            if !pressed.is_empty() {
                let chosen = self.offered.save_folder.clone();
                let sink = try_consume_context::<link::FolderSink>();
                accept(
                    &self.link,
                    &pressed,
                    self.offered.auto_save,
                    chosen.as_deref(),
                    sink,
                    said_on_accept,
                );
            }
        }
        let carrying = try_consume_context::<Carrying>();
        let mut unticked: State<std::collections::BTreeSet<String>> =
            use_state(std::collections::BTreeSet::new);
        let ticked: Vec<String> = files
            .iter()
            .filter(|file| !unticked.read().contains(&file.id))
            .map(|file| file.id.clone())
            .collect();
        let all: Vec<String> = files.iter().map(|file| file.id.clone()).collect();
        let several = files.len() > 1;
        let (cancel_link, cancel_all) = (
            self.link.clone(),
            if all.is_empty() {
                vec![offer.id.clone()]
            } else {
                all.clone()
            },
        );
        // With the list open, the one name is the list said worse: it is left out, so that the bar does not name one file above and show
        // two below. The line that says how many wait stays, since it is true of the whole queue.
        let open = several && listing();
        let bar = rect()
            .width(Size::fill())
            .height(Size::auto())
            .padding(16.)
            .spacing(10.)
            .corner_radius(self.corner)
            .background(palette.surface)
            .border(
                Border::new()
                    .fill(palette.receive)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .child(
                label()
                    .text(view::offer_words_in(i18n::language(), offer))
                    .color(palette.text)
                    .font_size(16.)
                    .font_weight(FontWeight::BOLD),
            )
            .maybe_child((!open).then(|| {
                label()
                    .text(offer.name.clone())
                    .color(palette.muted)
                    .font_size(14.)
            }))
            .maybe_child(
                view::queue_words_in(i18n::language(), self.offered.waiting)
                    .map(|words| mono(words, palette.muted, 11.)),
            )
            .maybe_child(view::time_left(offer.expires_at, self.now).map(|seconds| {
                mono(
                    i18n::format(
                        i18n::language(),
                        "ENDS IN {time}",
                        &[("time", &view::clock_words(seconds))],
                    ),
                    if seconds <= 10 {
                        palette.bad
                    } else {
                        palette.muted
                    },
                    12.,
                )
            }));
        let mut bar = bar;
        if several && listing() {
            let mut list = rect().width(Size::fill()).spacing(6.);
            for file in files {
                let id = file.id.clone();
                let on = !unticked.read().contains(&id);
                let place = file
                    .folders
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(file.name.as_str()))
                    .collect::<Vec<_>>()
                    .join("/");
                list = list.child(
                    rect()
                        .key(id.clone())
                        .width(Size::fill())
                        .horizontal()
                        .content(Content::flex())
                        .spacing(10.)
                        .cross_align(Alignment::center())
                        .child(Switch::new(WritableSetter::new(on, move |value: bool| {
                            if Carrying::now(carrying) {
                                return;
                            }
                            let mut now = unticked.write();
                            if value {
                                now.remove(&id);
                            } else {
                                now.insert(id.clone());
                            }
                        })))
                        .child(
                            label()
                                .text(place)
                                .color(palette.text)
                                .font_size(14.)
                                .width(Size::flex(1.)),
                        )
                        .child(rect().width(Size::auto()).child(mono(
                            view::size_in_words(file.bytes),
                            palette.muted,
                            12.,
                        ))),
                );
            }
            bar = bar.child(list);
        }
        let accept_ids = if several {
            ticked.clone()
        } else {
            vec![offer.id.clone()]
        };
        let mut buttons = rect()
            .width(Size::fill())
            .horizontal()
            .content(Content::flex())
            .cross_align(Alignment::start())
            .spacing(10.);
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    if Carrying::now(carrying) {
                        return;
                    }
                    for transfer in &cancel_all {
                        cancel_link.send(Command::CancelTransfer {
                            transfer: transfer.clone(),
                        });
                    }
                })
                .child(crate::i18n::tr("Cancel")),
        );
        if several {
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        if !Carrying::now(carrying) {
                            listing.set(!listing());
                        }
                    })
                    .child(crate::i18n::tr("View files")),
            );
        }
        buttons = buttons.child(AcceptButton {
            link: self.link.clone(),
            ids: accept_ids,
            auto_save: self.offered.auto_save,
            save_folder: self.offered.save_folder.clone(),
            palette,
            filled: true,
        });
        bar.child(buttons)
    }
}

/// One transfer as a full card, with the buttons it has (cancel, accept): the same card on the list under the circle and for what is going on in the
/// Transfers tab, so what it shows and what its buttons do is decided in one place.
#[derive(PartialEq)]
pub(crate) struct TransferCard {
    pub(crate) link: Link,
    pub(crate) item: Progress,
    pub(crate) palette: Palette,
    pub(crate) auto_save: bool,
    pub(crate) save_folder: Option<std::path::PathBuf>,
}

impl Component for TransferCard {
    fn render(&self) -> impl IntoElement {
        let (item, palette) = (&self.item, self.palette);
        let (state, colour) = match &item.phase {
            Phase::Waiting => (i18n::tr("WAITING FOR APPROVAL").to_owned(), palette.muted),
            Phase::Moving => (format!("{}%", item.percent), palette.send),
            Phase::Done => (i18n::tr("DONE").to_owned(), palette.good),
            Phase::Cancelled => (i18n::tr("CANCELLED").to_owned(), palette.muted),
            Phase::Failed(reason) => (failed_words(reason), palette.bad),
        };
        let direction = i18n::tr(if item.incoming {
            "INCOMING"
        } else {
            "OUTGOING"
        });
        let mut card = rect()
            .key(item.id.clone())
            .width(Size::fill())
            .padding(16.)
            .spacing(8.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .child(
                rect()
                    .horizontal()
                    .spacing(8.)
                    .cross_align(Alignment::center())
                    .maybe_child((item.phase == Phase::Done).then(|| {
                        label()
                            .text("\u{2713}")
                            .color(palette.good)
                            .font_size(16.)
                            .font_weight(FontWeight::BOLD)
                    }))
                    .child(
                        label()
                            .text(item.name.clone())
                            .color(palette.text)
                            .font_size(16.)
                            .font_weight(FontWeight::BOLD)
                            .max_lines(1),
                    ),
            )
            .child(mono(format!("{direction} · {state}"), colour, 11.))
            // The bar in the colour of how it went: done green, failed red, otherwise the colour of its direction.
            .child(
                ProgressBar::new(f32::from(item.percent))
                    .width(Size::fill())
                    .progress_background(match item.phase {
                        Phase::Done => palette.good,
                        Phase::Failed(_) => palette.bad,
                        _ if item.incoming => palette.receive,
                        _ => palette.send,
                    }),
            );
        // Cancel on the left and Accept on the right, on one row: Accept is the answer the right hand presses, and its column takes what is
        // left beside Cancel, so that the sentence that says why a folder was not chosen wraps inside it and never pushes Cancel off the edge.
        let mut answers = rect()
            .width(Size::fill())
            .horizontal()
            .content(Content::flex())
            .cross_align(Alignment::start())
            .spacing(10.);
        if item.stoppable() {
            let link = self.link.clone();
            let id = item.id.clone();
            answers = answers.child(
                Button::new()
                    .on_press(move |_| {
                        link.send(Command::CancelTransfer {
                            transfer: id.clone(),
                        })
                    })
                    .child(crate::i18n::tr("Cancel")),
            );
        }
        if item.acceptable() {
            answers = answers.child(AcceptButton {
                link: self.link.clone(),
                ids: vec![item.id.clone()],
                auto_save: self.auto_save,
                save_folder: self.save_folder.clone(),
                palette,
                filled: false,
            });
        }
        if item.stoppable() || item.acceptable() {
            card = card.child(answers);
        }
        card
    }
}

/// One finished transfer of the history, as a row of ONE fixed height (`view::HISTORY_ROW_HEIGHT`): its name, and under it which way it went and
/// how it ended. No buttons and no bar, so that every row is the same height and a long history is a column of equal, cheap rows.
#[derive(PartialEq)]
pub(crate) struct HistoryRow {
    pub(crate) item: Progress,
    pub(crate) palette: Palette,
}

impl Component for HistoryRow {
    fn render(&self) -> impl IntoElement {
        let (item, palette) = (&self.item, self.palette);
        let (state, colour) = match &item.phase {
            Phase::Done => (i18n::tr("DONE").to_owned(), palette.good),
            Phase::Cancelled => (i18n::tr("CANCELLED").to_owned(), palette.muted),
            Phase::Failed(reason) => (failed_words(reason), palette.bad),
            // Not in the history: what is going is a card above it. Said plainly if one ever got here, never hidden.
            Phase::Waiting | Phase::Moving => (format!("{}%", item.percent), palette.send),
        };
        let direction = i18n::tr(if item.incoming {
            "INCOMING"
        } else {
            "OUTGOING"
        });
        rect()
            .width(Size::fill())
            .height(Size::px(view::HISTORY_ROW_HEIGHT))
            .padding((6., 12.))
            .spacing(2.)
            .corner_radius(10.)
            .background(palette.surface)
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .main_align(Alignment::center())
            .child(
                label()
                    .text(item.name.clone())
                    .color(palette.text)
                    .font_size(15.)
                    .font_weight(FontWeight::BOLD)
                    .max_lines(1),
            )
            .child(mono(format!("{direction} · {state}"), colour, 11.))
    }
}

impl Component for Transfers {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // Only what the list under the big circle needs, and kept here: the transfers are the owner's, and nothing of this touches them.
        // `folded` folds the list into a line while something goes; `put_away` holds the ids that X hid, so that what was hidden stays hidden
        // only as long as it is the same transfer (a new one has a new id, and shows).
        let mut folded = use_state(|| false);
        let mut put_away: State<Vec<String>> = use_state(Vec::new);
        if self.items.is_empty() && self.prepared.is_empty() {
            return rect().into_element();
        }
        // What the list draws is ONE kind of row. In the circle's context it is the batch as its own model orders it (`view::batch_rows`: a heading for
        // each device when it went to more than one, then that device's files), from what X has not hidden; elsewhere, the transfers in the order they
        // always had. The drawing below walks these and decides nothing about grouping.
        let shown: Vec<view::BatchRow> = if self.context {
            let visible: Vec<Progress> = view::not_put_away(&self.items, &put_away.read())
                .into_iter()
                .cloned()
                .collect();
            view::batch_rows(&visible)
        } else {
            view::transfer_list(&self.items)
                .cloned()
                .map(view::BatchRow::Transfer)
                .collect()
        };
        let header: Element = if self.context {
            match view::header_action(&self.items, &put_away.read(), *folded.read()) {
                Some(action) => {
                    let items = self.items.clone();
                    ListHeader {
                        action,
                        on_press: EventHandler::new(
                            move |action: view::HeaderAction| match action {
                                view::HeaderAction::Minimize => folded.set(true),
                                view::HeaderAction::Expand => folded.set(false),
                                // Only what is over is put away, and only when nothing is going: the model says which (see `put_away_now`).
                                view::HeaderAction::Hide => {
                                    let mut now = put_away.peek().clone();
                                    now.extend(view::put_away_now(&items));
                                    now.sort();
                                    now.dedup();
                                    put_away.set(now);
                                }
                            },
                        ),
                    }
                    .into_element()
                }
                None => rect().into_element(),
            }
        } else {
            TransfersHeader {
                link: self.link.clone(),
                palette,
                can_clear: self.items.iter().any(|item| item.phase.is_over()),
            }
            .into_element()
        };
        // Folded: while something goes, the list is one line, and the transfer carries on.
        if self.context
            && *folded.read()
            && let Some(line) = view::folded_line(&self.items)
        {
            return rect()
                .width(Size::fill())
                .spacing(10.)
                .child(header)
                .child(label().text(line).color(palette.muted).font_size(14.))
                .into_element();
        }
        // The Transfers tab has the whole record, which has no end: what is going stays as full cards above (few, and each has buttons), and what is
        // over (the history) is drawn as compact rows of ONE fixed height, only the newest of them (`view::HISTORY_ROWS_MADE`), so that the cost does
        // not grow with the history (`view::sections`, `view::HISTORY_ROW_HEIGHT`). The list under the circle is the batch and is small: it keeps
        // its cards.
        if !self.context {
            let parts = view::sections(&self.items);
            let history: Vec<Progress> = parts.history.iter().map(|item| (*item).clone()).collect();
            let mut going = rect().width(Size::fill()).spacing(10.);
            for item in parts.going {
                going = going.child(TransferCard {
                    link: self.link.clone(),
                    item: item.clone(),
                    palette,
                    auto_save: self.auto_save,
                    save_folder: self.save_folder.clone(),
                });
            }
            // The history has no end, so only the newest of it is made as rows (`view::HISTORY_ROWS_MADE`): the cost does not grow with it. They go
            // straight into the page's own column, which already scrolls.
            let mut history_list = rect().width(Size::fill()).spacing(view::HISTORY_ROW_GAP);
            for item in history.iter().take(view::HISTORY_ROWS_MADE) {
                history_list = history_list.child(rect().key(item.id.clone()).child(HistoryRow {
                    item: item.clone(),
                    palette,
                }));
            }
            return rect()
                .width(Size::fill())
                .spacing(10.)
                .child(header)
                .child(going)
                .child(history_list)
                .into_element();
        }
        let mut list = rect().width(Size::fill()).spacing(10.);
        for row in shown {
            let item = match row {
                view::BatchRow::Transfer(item) => item,
                // The heading of one device's part of the batch: who, and how that device stands by itself, so that a problem with one device is
                // read here and not guessed from the circle, which says only how the batch as a whole goes.
                view::BatchRow::Device { name, state, .. } => {
                    let (words, tone) = view::device_state_words_in(i18n::language(), &state);
                    let colour = palette.of_tone(tone);
                    list = list.child(
                        rect()
                            .width(Size::fill())
                            .horizontal()
                            .spacing(8.)
                            .cross_align(Alignment::center())
                            .child(
                                label()
                                    .text(name)
                                    .color(palette.text)
                                    .font_size(14.)
                                    .font_weight(FontWeight::BOLD),
                            )
                            .child(mono(words, colour, 11.)),
                    );
                    continue;
                }
            };
            list = list.child(TransferCard {
                link: self.link.clone(),
                item,
                palette,
                auto_save: self.auto_save,
                save_folder: self.save_folder.clone(),
            });
        }
        // The files that are listed and have not begun to go: a row each, with their status, in the same list. Never on a list that is not the
        // one under the circle (`prepared` is empty there).
        // What can be done with the files that wait, from the one list of them: more can be added, one can be taken away, all can be cleared. The
        // page gives these three actions (this list has no rule of its own about the files), and only when there is at least one file is anything
        // offered: with none, the circle is what chooses.
        if let Some(actions) = self.actions.clone().filter(|_| !self.prepared.is_empty()) {
            let add_more = actions.add_more.clone();
            let clear = actions.clear.clone();
            list = list.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(10.)
                    .cross_align(Alignment::center())
                    .child(
                        Button::new()
                            .on_press(move |_| add_more.call(()))
                            .child(i18n::tr("+ Add more files")),
                    )
                    .child(rect().width(Size::flex(1.)))
                    .child(
                        Button::new()
                            .on_press(move |_| clear.call(()))
                            .child(i18n::tr("Clear the list")),
                    ),
            );
        }
        for (index, name) in self.prepared.iter().enumerate() {
            let remove = self.actions.as_ref().map(|actions| actions.remove.clone());
            list = list.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .cross_align(Alignment::center())
                    .padding(16.)
                    .spacing(8.)
                    .corner_radius(12.)
                    .background(palette.surface)
                    .border(
                        Border::new()
                            .fill(palette.border)
                            .width(1.)
                            .alignment(BorderAlignment::Inner),
                    )
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(8.)
                            .child(
                                label()
                                    .text(name.clone())
                                    .color(palette.text)
                                    .font_size(16.)
                                    .font_weight(FontWeight::BOLD),
                            )
                            .child(mono(
                                i18n::tr("OUTGOING \u{b7} PREPARED").to_owned(),
                                palette.muted,
                                11.,
                            )),
                    )
                    .maybe_child(remove.map(|remove| {
                        Element::from(
                            Button::new()
                                .on_press(move |_| remove.call(index))
                                .child("\u{2715}"),
                        )
                    })),
            );
        }
        let list: Element = match view::transfer_list_height(self.items.len() + self.prepared.len())
        {
            Some(height) => rect()
                .width(Size::fill())
                .height(Size::px(height))
                .child(ScrollView::new().auto_hide_scrollbars(false).child(list))
                .into(),
            None => list.into(),
        };
        // Keep the controls outside the scrolling viewport. History stays in the model and every
        // row remains reachable, but more rows cannot grow the enclosing page indefinitely.
        rect()
            .width(Size::fill())
            .spacing(10.)
            .child(header)
            .child(list)
            .into_element()
    }
}
