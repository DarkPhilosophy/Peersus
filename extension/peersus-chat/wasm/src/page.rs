//! Local page and picker deliveries. A picker can wait on the person without blocking the guest's
//! peer streams and outbox. Its response is scoped to the page and room that started it.
use super::{
    Fault, Link, Local, MAX_ATTACHMENT, MAX_NOTICES, NOTICE_WAIT, Refusal, RoomKey, Runner, Value,
    Wire, api, host, json, store,
};
use crate::{
    media_sdk,
    sdk::{
        audio,
        elements::{self, Event},
    },
    ui::{self, Busy, Draft, Effect, Feedback, Locale, Media, Page, VOICE_NAME},
};

const LANGUAGE: &str = "peersus.language";
pub(super) const NOTIFY: &str = "peersus.notify";
/// The notification service's event that one of Chat's was tapped, and its method that says which.
pub(super) use crate::sdk::notification::OPENED;
const DRAFT_LIFE_MS: u64 = 240_000;

enum Stage {
    Pick(Media),
    Import {
        media: Media,
        file: String,
        name: String,
    },
    Save,
}
impl Stage {
    /// Which picker `peersus.files.choose` opens for this step.
    fn kind(&self) -> &'static str {
        match self {
            Self::Pick(Media::Photo) => "image",
            Self::Pick(Media::Audio) => "audio",
            _ => "any",
        }
    }
}
pub(super) struct FileJob {
    id: u64,
    page: String,
    visit: u64,
    room: RoomKey,
    stage: Stage,
}

impl Runner {
    pub(super) fn configure_pages<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        config: &Value,
    ) {
        let allowed = |name: &str| {
            config["missing_permissions"]
                .as_array()
                .is_some_and(|missing| {
                    !missing
                        .iter()
                        .any(|p| p["owner"] == "host" && p["name"] == name)
                })
        };
        self.features.select = allowed("files.select");
        self.features.export = allowed(api::EXPORT);
        self.features.notify = allowed("notify.show");
        self.features.record = allowed(audio::RECORD_CAPABILITY);
        let offered = |feature: &str| {
            config[elements::PAGE_FEATURES_KEY]
                .as_array()
                .is_some_and(|features| features.iter().any(|item| item == feature))
        };
        self.features.visibility = offered(elements::FEATURE_VISIBILITY);
        self.features.press = offered(elements::FEATURE_PRESS);
        self.features.back = offered(elements::FEATURE_BACK);
        // Sounds are decoded by the helper a WASM guest runs in; a native library has no decoder.
        self.features.listen = cfg!(target_family = "wasm");
        self.locale = if allowed("language.read") {
            link.call(LANGUAGE, "get", json!({}), 1000)
                .ok()
                .and_then(|v| v["language"].as_str().map(Locale::from_tag))
                .unwrap_or(Locale::En)
        } else {
            Locale::En
        };
        let primary = config["settings"][ui::PRIMARY] == true;
        if self.primary != Some(primary) {
            let pages = if primary {
                json!([{"id":ui::PRIMARY,"page":ui::PAGE,"placement":"primary"}])
            } else {
                json!([])
            };
            if link
                .call("peersus.pages", "set", json!({"pages":pages}), 1000)
                .is_ok()
            {
                self.primary = Some(primary);
                if !primary
                    && let Some(mut old) = self.pages.remove(ui::PRIMARY)
                    && let Some(draft) = old.draft.take()
                {
                    link.abort(draft.stage.stage);
                }
            }
        }
    }

    fn context(&self) -> ui::Context<'_> {
        ui::Context {
            engine: &self.engine,
            directory: &self.peers,
            locale: self.locale,
            features: self.features,
            thumbs: &self.thumbs,
            listening: self.listening.as_ref(),
            waves: &self.waves,
        }
    }

    /// Make the picture of one more photo of `room`, the newest not tried yet: read the photo this device holds, decode it in
    /// this process, and have the application keep the small picture for the page. One a turn, so a conversation full of photos
    /// fills in a little at a time and never holds up the page.
    /// A photo that cannot be read or kept now is tried again later; only one that is no picture is not tried again.
    fn make_thumb<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        room: &RoomKey,
        now: u64,
    ) {
        let Some((asset, bytes)) = self.thumbs.wanted(&self.engine, room, now) else {
            return;
        };
        let Ok(photo) = link.read_whole(&asset, self.revision, bytes) else {
            self.thumbs.unread(asset, now);
            return;
        };
        let Some((width, height, rgba)) = media_sdk::decode_picture(&photo, ui::THUMB_EDGE) else {
            self.thumbs.made(asset, None);
            return;
        };
        let gone = self.thumbs.make_room();
        if !gone.is_empty() {
            link.forget_pictures(gone);
        }
        let id = self.thumbs.id();
        if link.keep_picture(&id, width, height, &rgba).is_err() {
            self.thumbs.unread(asset, now);
            return;
        }
        let thumb = ui::Thumb {
            id,
            width: width as u16,
            height: height as u16,
        };
        self.thumbs.made(asset, Some(thumb));
    }

    /// Measure the waveform of one more sound of `room` whose message carries none, from the WAV this device holds. One a turn.
    /// A sound that cannot be read now is tried again later.
    fn make_wave<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        room: &RoomKey,
        now: u64,
    ) {
        let Some((asset, bytes)) = self.waves.wanted(&self.engine, room, now) else {
            return;
        };
        match link.read_whole(&asset, self.revision, bytes) {
            Ok(sound) => self.waves.made(asset, ui::measure_wav(&sound)),
            Err(_) => self.waves.unread(asset, now),
        }
    }

    /// Keep the pictures `view` shows for itself in step with it: the chosen photo before it is sent, and the photo opened over
    /// the page. Each is made once, in this process, and let go of as soon as it is no longer shown.
    pub(super) fn sync_pictures<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        name: &str,
        view: &mut Page,
    ) {
        let chosen = view
            .draft
            .as_ref()
            .filter(|draft| draft.selected == ui::Media::Photo && draft.media_type.is_some())
            .map(|draft| (draft.stage.stage, draft.stage.bytes));
        if view.draft_picture.as_ref().map(|shown| shown.of) != chosen.map(|(stage, _)| stage) {
            if let Some(thumb) = view.draft_picture.take().and_then(|shown| shown.thumb) {
                link.forget_pictures(vec![thumb.id]);
            }
            if let Some((stage, bytes)) = chosen {
                let photo = link.read_stage(stage, bytes).ok();
                view.draft_picture = Some(ui::Shown {
                    of: stage,
                    thumb: keep(link, format!("draft/{name}"), photo, ui::THUMB_EDGE),
                });
            }
        }
        let opened = view.opened_photo().and_then(|key| {
            let row = self.engine.data.rows.get(&key)?;
            let bytes = row.body.as_ref()?.attachments.first()?.bytes;
            Some((key, row.asset.clone()?, bytes))
        });
        if view.view_picture.as_ref().map(|shown| &shown.of) != opened.as_ref().map(|(key, ..)| key)
        {
            if let Some(thumb) = view.view_picture.take().and_then(|shown| shown.thumb) {
                link.forget_pictures(vec![thumb.id]);
            }
            if let Some((key, asset, bytes)) = opened {
                let photo = link.read_whole(&asset, self.revision, bytes).ok();
                view.view_picture = Some(ui::Shown {
                    thumb: keep(link, format!("view/{name}"), photo, ui::VIEW_EDGE),
                    of: key,
                });
            }
        }
    }

    /// Tell the person of messages that arrived: at once when their conversation is not open on a page; otherwise after
    /// [`NOTICE_WAIT`], unless it was seen there by then (the window in front shows it, and its being seen is told).
    pub(super) fn notice<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        fresh: Vec<String>,
        now: u64,
    ) {
        for key in fresh {
            let open = self.engine.data.rows.get(&key).is_some_and(|row| {
                self.pages
                    .values()
                    .any(|page| page.room() == Some(&row.room))
            });
            if !open {
                self.show_notice(link, &key);
                continue;
            }
            if self.notices.len() >= MAX_NOTICES
                && let Some((oldest, _)) = self.notices.pop_front()
            {
                self.show_notice(link, &oldest);
            }
            self.notices.push_back((key, now));
        }
    }

    /// Notify the messages that waited [`NOTICE_WAIT`] in an open conversation without being seen.
    pub(super) fn notify_waiting<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        now: u64,
    ) {
        while self
            .notices
            .front()
            .is_some_and(|(_, at)| now.saturating_sub(*at) >= NOTICE_WAIT)
        {
            if let Some((key, _)) = self.notices.pop_front() {
                self.show_notice(link, &key);
            }
        }
    }

    /// One notification of the message `key`, reaching outside the window while it is not in front: none when the person
    /// did not let Chat notify, or the message is gone or already seen.
    fn show_notice<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, key: &str) {
        if !self.features.notify {
            return;
        }
        let Some((room, text)) = self
            .engine
            .data
            .rows
            .get(key)
            .filter(|row| !row.seen)
            .and_then(|row| Some((row.room.clone(), self.context().notice(row)?)))
        else {
            return;
        };
        // It stays until the person taps or closes it, and a tap opens its conversation: one per conversation, the newest.
        let _ = link.call(
            NOTIFY,
            "show",
            json!({"text": text, "reach": "outside", "stays": true, "opens": room.namespace()}),
            2000,
        );
    }

    /// The person tapped notifications of Chat: their conversations open, on every page Chat shows now and on the one the
    /// application opens for it next. A word that names no conversation of this device is ignored.
    pub(super) fn notices_opened<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>) {
        let Ok(answer) = link.call(NOTIFY, OPENED, json!({}), 2000) else {
            return;
        };
        let rooms = answer["opened"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|word| RoomKey::try_from(word.as_str()?.to_owned()).ok());
        for room in rooms {
            if self.local(link, Local::Open(room.clone())).is_err() {
                continue;
            }
            // As when the person goes to another conversation themselves: a draft prepared in the one left is let go of.
            let names: Vec<String> = self.pages.keys().cloned().collect();
            for name in names {
                let Some(mut view) = self.pages.remove(&name) else {
                    continue;
                };
                let visit = view.visit;
                view.open(room.clone());
                if view.visit != visit
                    && let Some(draft) = view.draft.take()
                {
                    link.abort(draft.stage.stage);
                }
                self.sync_pictures(link, &name, &mut view);
                self.pages.insert(name, view);
            }
            self.opening = Some(room);
        }
    }

    pub(super) fn page_event<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        args: &Value,
        now: u64,
    ) -> Result<Value, Fault> {
        let name = args["page"]
            .as_str()
            .filter(|name| matches!(*name, ui::PAGE | ui::PRIMARY))
            .ok_or(Fault::Invalid)?;
        let event = store::parse::<Event>(args["event"].clone()).map_err(|_| Fault::Invalid)?;
        if event.validate().is_err() {
            return Err(Fault::Invalid);
        }
        let force = matches!(event, Event::Open);
        if (force || matches!(event, Event::Tick { .. })) && self.due <= now {
            self.refresh(link, now);
        }
        if matches!(event, Event::Open | Event::Tick { .. })
            && let Some(room) = self.pages.get(name).and_then(|page| page.room()).cloned()
        {
            self.make_thumb(link, &room, now);
            self.make_wave(link, &room, now);
        }
        let mut view = match self.pages.remove(name) {
            Some(view) => view,
            // A page the application opened for a tapped notification starts in its conversation.
            None => {
                let mut view = Page::default();
                if let Some(room) = self.opening.take() {
                    view.open(room);
                }
                view
            }
        };
        if Page::passing(&event) {
            // The pointer over a box this page does not press changes nothing.
            self.pages.insert(name.to_owned(), view);
            return Ok(Value::Null);
        }
        let stamp = view.visit;
        view.now = now;
        if let Some(effect) = view.event(self.context(), event) {
            self.apply_effect(link, name, &mut view, effect);
        }
        if view.visit != stamp
            && let Some(draft) = view.draft.take()
        {
            link.abort(draft.stage.stage);
        }
        self.sync_pictures(link, name, &mut view);
        // A recording under way shows what the microphone hears: its newest levels, read at each of the page's ticks.
        view.live = if view.recording.is_some() {
            link.call(audio::SERVICE, audio::LEVELS, json!({}), 1000)
                .ok()
                .and_then(|value| serde_json::from_value::<audio::Levels>(value).ok())
                .or(view.live.take())
        } else {
            None
        };
        let page = view.render(self.context(), force);
        self.pages.insert(name.to_owned(), view);
        Ok(page)
    }

    fn apply_effect<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        name: &str,
        view: &mut Page,
        effect: Effect,
    ) {
        let fail = |page: &mut Page, error| page.feedback = Some(Feedback::Failure(error));
        let result = match effect {
            Effect::Open(room) => {
                match self.local(link, Local::Open(room.clone())) {
                    Ok(_) => view.open(room),
                    Err(e) => fail(view, e),
                }
                return;
            }
            Effect::Text(room, text) => {
                let reply = view.replying.clone();
                view.submitted(
                    self.local(link, Local::Text { room, text, reply })
                        .map(|_| ()),
                );
                return;
            }
            Effect::Group(aliases, title) => {
                match self
                    .local(link, Local::Group { aliases, title })
                    .and_then(|value| store::parse(value["room"].clone()))
                {
                    Ok(room) => view.open(room),
                    Err(error) => view.group_refused(error),
                }
                return;
            }
            Effect::Choose(room, media) => {
                if !self.features.select {
                    view.feedback = Some(Feedback::Permissions);
                    return;
                }
                if let Err(error) =
                    self.begin_file(link, name, view, room, Stage::Pick(media), 600_000)
                {
                    view.feedback = Some(error);
                }
                return;
            }
            Effect::SendDraft(room) => {
                if let Some(draft) = view.draft.take() {
                    let reply = view.replying.clone();
                    match self.local(
                        link,
                        Local::Attach {
                            room,
                            draft: &draft,
                            reply,
                        },
                    ) {
                        Ok(_) => view.replying = None,
                        Err(error) => {
                            view.draft = Some(draft);
                            fail(view, error);
                        }
                    }
                }
                return;
            }
            Effect::RemoveDraft => {
                if let Some(draft) = view.draft.take() {
                    link.abort(draft.stage.stage);
                }
                return;
            }
            Effect::Save(room, row) => {
                if !self.features.export {
                    view.feedback = Some(Feedback::Permissions);
                    return;
                }
                let Some(row) = self.engine.data.rows.get(&row).filter(|r| r.room == room) else {
                    fail(view, Fault::Missing);
                    return;
                };
                let Some((asset, file)) = row
                    .asset
                    .as_ref()
                    .zip(row.body.as_ref().and_then(|b| b.attachments.first()))
                else {
                    fail(view, Fault::Missing);
                    return;
                };
                let Ok(key) = store::key(asset.clone()) else {
                    fail(view, Fault::Storage);
                    return;
                };
                let args = api::Save {
                    key,
                    revision: self.revision,
                    suggested_name: file.name.clone(),
                };
                if self.file_job.is_some() {
                    view.feedback = Some(Feedback::PickerBusy);
                    return;
                }
                match serde_json::to_value(args)
                    .map_err(|_| Refusal::Other)
                    .and_then(|args| link.request(api::FILES, api::SAVE, args, 600_000))
                {
                    Ok(id) => {
                        self.file_job = Some(FileJob {
                            id,
                            page: name.to_owned(),
                            visit: view.visit,
                            room,
                            stage: Stage::Save,
                        });
                        view.busy = Some(Busy::Saving);
                    }
                    Err(error) => view.feedback = Some(file_error(error)),
                }
                return;
            }
            Effect::Delete(room, id, mode) => self.local(link, Local::Delete { room, id, mode }),
            Effect::Clear(room, remote, purge) => {
                let result = self.local(
                    link,
                    Local::Clear {
                        room: room.clone(),
                        remote,
                        purge,
                    },
                );
                if result.is_ok() {
                    view.reset_after_clear(purge);
                    self.clear_other_pages(link, &room, purge);
                }
                result
            }
            Effect::React(room, row, emoji) => self.local(link, Local::React { room, row, emoji }),
            Effect::Retry(peer) => self.local(link, Local::Retry { peer }),
            Effect::OfferGroup(room, member) => {
                self.local(link, Local::OfferGroup { room, member })
            }
            Effect::Seen(room, row) => self.local(
                link,
                Local::Observe {
                    room,
                    row,
                    visible: true,
                    foreground: true,
                },
            ),
            Effect::Listen(room, row) => self.listen(link, &room, row, 0).map(|()| Value::Null),
            Effect::Record(room) => {
                if !self.features.record {
                    view.feedback = Some(Feedback::Permissions);
                    return;
                }
                // Up to the person's answer to the system's question, where it asks one.
                let started = link.call(
                    audio::SERVICE,
                    audio::RECORD,
                    json!({"max_seconds": audio::MAX_RECORD_SECONDS}),
                    20_000,
                );
                match started {
                    // Counted from when the microphone started, not from the system's question before it.
                    Ok(_) => view.recording = Some((room, link.clock().unwrap_or(view.now))),
                    Err(Refusal::Blocked) => view.feedback = Some(Feedback::Permissions),
                    Err(_) => view.feedback = Some(Feedback::Recording),
                }
                return;
            }
            Effect::CancelRecord => {
                view.recording = None;
                let _ = link.call(audio::SERVICE, audio::CANCEL, json!({}), 2000);
                return;
            }
            Effect::FinishRecord(room, send) => {
                view.recording = None;
                let recorded = link
                    .call(audio::SERVICE, audio::FINISH, json!({}), 10_000)
                    .ok()
                    .and_then(|value| serde_json::from_value::<audio::Recorded>(value).ok());
                let Some(recorded) = recorded else {
                    view.feedback = Some(Feedback::Recording);
                    return;
                };
                let Ok(now) = link.clock() else {
                    link.abort(recorded.stage);
                    fail(view, Fault::Storage);
                    return;
                };
                // Sent as any chosen audio file is: from a sealed stage, kept as the message's own copy.
                let draft = Draft {
                    selected: Media::Audio,
                    media_type: Some("audio/wav"),
                    name: VOICE_NAME.into(),
                    stage: api::Sealed {
                        stage: recorded.stage,
                        bytes: recorded.bytes,
                        sha256: recorded.sha256,
                    },
                    expires: now.saturating_add(DRAFT_LIFE_MS),
                    wave: crate::wire::wave(&recorded.levels),
                };
                // Stopped: kept above the composer, to send or delete.
                if !send {
                    if let Some(old) = view.draft.replace(draft) {
                        link.abort(old.stage.stage);
                    }
                    return;
                }
                let reply = view.replying.clone();
                match self.local(
                    link,
                    Local::Attach {
                        room,
                        draft: &draft,
                        reply,
                    },
                ) {
                    Ok(_) => view.replying = None,
                    Err(error) => {
                        if let Some(old) = view.draft.replace(draft) {
                            link.abort(old.stage.stage);
                        }
                        fail(view, error);
                    }
                }
                return;
            }
        };
        if let Err(error) = result {
            fail(view, error);
        }
    }

    fn clear_other_pages<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        room: &RoomKey,
        purge: bool,
    ) {
        for view in self
            .pages
            .values_mut()
            .filter(|view| view.room() == Some(room))
        {
            view.reset_after_clear(purge);
            if let Some(draft) = view.draft.take() {
                link.abort(draft.stage.stage);
            }
        }
        // A save is an exact, user-chosen export and is not an outbox operation. On a clear,
        // cancel it if its OS dialog or copy hasn't already won. A completed export is separate.
        if self
            .file_job
            .as_ref()
            .is_some_and(|j| &j.room == room && matches!(j.stage, Stage::Save))
            && let Some(job) = self.file_job.take()
        {
            link.cancel(job.id);
        }
    }

    fn begin_file<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        name: &str,
        view: &mut Page,
        room: RoomKey,
        stage: Stage,
        timeout: u32,
    ) -> Result<(), Feedback> {
        if self.file_job.is_some() {
            return Err(Feedback::PickerBusy);
        }
        let id = link
            .request(
                api::FILES,
                "choose",
                json!({"max_bytes":MAX_ATTACHMENT,"kind":stage.kind()}),
                timeout,
            )
            .map_err(file_error)?;
        self.file_job = Some(FileJob {
            id,
            page: name.to_owned(),
            visit: view.visit,
            room,
            stage,
        });
        view.busy = Some(Busy::Picking);
        Ok(())
    }

    pub(super) fn expire_drafts<H: Wire + host::Data>(&mut self, link: &mut Link<'_, H>, now: u64) {
        for page in self.pages.values_mut() {
            if page
                .draft
                .as_ref()
                .is_some_and(|draft| draft.expires <= now)
                && let Some(draft) = page.draft.take()
            {
                link.abort(draft.stage.stage);
                page.feedback = Some(Feedback::DraftExpired);
            }
        }
    }

    pub(super) fn file_result<H: Wire + host::Data>(
        &mut self,
        link: &mut Link<'_, H>,
        message: &Value,
        now: u64,
    ) {
        if self
            .file_job
            .as_ref()
            .is_none_or(|job| message["id"].as_u64() != Some(job.id))
        {
            return;
        }
        let Some(job) = self.file_job.take() else {
            return;
        };
        let current = self
            .pages
            .get(&job.page)
            .is_some_and(|page| page.visit == job.visit && page.room() == Some(&job.room));
        let mut page = current.then(|| self.pages.remove(&job.page)).flatten();
        if let Some(view) = page.as_mut() {
            view.busy = None;
        }
        match job.stage {
            Stage::Pick(media) => match store::outcome(&message["result"]) {
                Ok(value) => {
                    if let Some(file) = value["file"]
                        .as_str()
                        .filter(|f| !f.is_empty() && f.len() <= 128)
                    {
                        let filename = value["name"]
                            .as_str()
                            .filter(|n| !n.is_empty())
                            .map(safe_filename);
                        if let (Some(view), Some(name)) = (page.as_mut(), filename) {
                            match link.request(
                                api::FILES,
                                api::STAGE_SELECTED,
                                json!({"file":file}),
                                15_000,
                            ) {
                                Ok(id) => {
                                    self.file_job = Some(FileJob {
                                        id,
                                        page: job.page.clone(),
                                        visit: job.visit,
                                        room: job.room,
                                        stage: Stage::Import {
                                            media,
                                            file: file.into(),
                                            name,
                                        },
                                    });
                                    view.busy = Some(Busy::Importing);
                                }
                                Err(error) => {
                                    release(link, file);
                                    view.feedback = Some(file_error(error));
                                }
                            }
                        } else {
                            release(link, file);
                        }
                    } else if let Some(view) = page.as_mut() {
                        view.feedback = Some(Feedback::Cancelled);
                    }
                }
                Err(error) => {
                    if let Some(view) = page.as_mut() {
                        view.feedback = Some(file_error(error));
                    }
                }
            },
            Stage::Import { media, file, name } => {
                release(link, &file);
                match store::outcome(&message["result"])
                    .and_then(|v| store::parse::<api::Sealed>(v).map_err(|_| Refusal::Other))
                {
                    Ok(stage) => {
                        let ready = (1..=u64::from(MAX_ATTACHMENT)).contains(&stage.bytes);
                        if let Some(view) = page.as_mut().filter(|_| ready) {
                            let bytes =
                                link.read_source(api::Source::Stage { stage: stage.stage }, 0, 32);
                            match bytes {
                                Ok(bytes) => {
                                    let media_type = media_kind(media, &bytes);
                                    if media_type.is_none() {
                                        view.feedback = Some(Feedback::NoMedia);
                                    }
                                    let draft = Draft {
                                        selected: media,
                                        media_type,
                                        name,
                                        stage,
                                        expires: now.saturating_add(DRAFT_LIFE_MS),
                                        wave: None,
                                    };
                                    if let Some(old) = view.draft.replace(draft) {
                                        link.abort(old.stage.stage);
                                    }
                                }
                                Err(error) => {
                                    link.abort(stage.stage);
                                    view.feedback = Some(Feedback::Failure(error));
                                }
                            }
                        } else {
                            link.abort(stage.stage);
                        }
                    }
                    Err(error) => {
                        if let Some(view) = page.as_mut() {
                            view.feedback = Some(file_error(error));
                        }
                    }
                }
            }
            Stage::Save => {
                if let Some(view) = page.as_mut() {
                    view.feedback = Some(match store::outcome(&message["result"]) {
                        Ok(value) if value["saved"] == true => Feedback::Exported,
                        Ok(_) => Feedback::Cancelled,
                        Err(error) => file_error(error),
                    });
                }
            }
        }
        if let Some(mut view) = page {
            self.sync_pictures(link, &job.page, &mut view);
            self.pages.insert(job.page, view);
        }
    }
}

fn release<H: Wire + host::Data>(link: &mut Link<'_, H>, file: &str) {
    let _ = link.call(api::FILES, "release", json!({"file":file}), 2000);
}
fn file_error(error: Refusal) -> Feedback {
    match error {
        Refusal::Blocked => Feedback::Permissions,
        Refusal::Quota => Feedback::Failure(Fault::Quota),
        Refusal::Closed => Feedback::Failure(Fault::Storage),
        Refusal::TooLarge => Feedback::TooLarge,
        Refusal::Cancelled | Refusal::Other => Feedback::Cancelled,
    }
}
fn safe_filename(name: &str) -> String {
    let mut result = String::new();
    for c in name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
    {
        if result.len() + c.len_utf8() > 120 {
            break;
        }
        result.push(c);
    }
    if result.trim().is_empty() || matches!(result.as_str(), "." | "..") {
        "attachment".into()
    } else {
        result
    }
}
fn media_kind(kind: Media, bytes: &[u8]) -> Option<&'static str> {
    match kind {
        Media::File => Some("application/octet-stream"),
        Media::Photo if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some("image/png"),
        Media::Photo if bytes.starts_with(b"\xff\xd8\xff") => Some("image/jpeg"),
        Media::Photo if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => {
            Some("image/gif")
        }
        Media::Photo if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            Some("image/webp")
        }
        Media::Audio if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") => {
            Some("audio/wav")
        }
        Media::Audio if bytes.starts_with(b"ID3") => Some("audio/mpeg"),
        Media::Audio if bytes.starts_with(b"fLaC") => Some("audio/flac"),
        Media::Audio if bytes.starts_with(b"OggS") => Some("audio/ogg"),
        Media::Audio if bytes.get(4..12) == Some(b"ftypM4A ") => Some("audio/mp4"),
        _ => None,
    }
}

/// Decode `photo` in this process, no larger than `edge` on its longest side, and have the application keep it as the picture
/// `id` this extension's pages place. None when it is not a picture this process decodes, or the keep is refused.
fn keep<H: Wire + host::Data>(
    link: &mut Link<'_, H>,
    id: String,
    photo: Option<Vec<u8>>,
    edge: u32,
) -> Option<ui::Thumb> {
    let (width, height, rgba) = media_sdk::decode_picture(&photo?, edge)?;
    link.keep_picture(&id, width, height, &rgba).ok()?;
    Some(ui::Thumb {
        id,
        width: width as u16,
        height: height as u16,
    })
}
