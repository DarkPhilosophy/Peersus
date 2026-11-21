//! Generic plugin contribution controls. No plugin name or OS-specific behavior lives here.
use crate::field::{Profile, field};
use crate::view::action_words;
use crate::{
    avatar_view, i18n,
    link::Link,
    shelf,
    theme::Palette,
    view::{Presence, View},
};
use freya::animation::*;
use freya::prelude::*;
use peersus_api::contributions::{Action, Kind, Scope, Setting};
#[cfg(test)]
use peersus_core::model::ActionFailure;
use peersus_core::{
    model::{ActionState, Command, PeerId, PluginInfo, PluginStatus, Standing},
    plugins::{PluginId, Value, Values},
};
use zeroize::Zeroize;

/// The most the pages an extension draws in its own settings are tall together, in logical pixels; past it, they scroll inside
/// its card.
pub(crate) const SETTINGS_PAGES_HEIGHT: f32 = 560.;

#[derive(PartialEq)]
pub struct Controls {
    pub link: Link,
    pub view: View,
    pub plugin: PluginInfo,
    pub pictures: shelf::Held,
    pub palette: Palette,
}

fn runnable(plugin: &PluginInfo) -> bool {
    plugin.standing == Standing::Ready && plugin.status == PluginStatus::Running
}

fn action_ready(view: &View, plugin: &PluginInfo, action: &Action, peer: Option<&PeerId>) -> bool {
    if !runnable(plugin) {
        return false;
    }
    let held = if let Some(id) = peer {
        let Some(device) = view
            .devices
            .iter()
            .find(|device| device.id == id.0 && device.presence == Presence::Online)
        else {
            return false;
        };
        device.grants.get(&plugin.id)
    } else {
        view.grants.get(&plugin.id)
    };
    action.capability.as_ref().is_none_or(|name| {
        peersus_api::permission::Permission::new(plugin.id.as_str(), name)
            .is_some_and(|permission| held.is_some_and(|grants| grants.allows(&permission)))
    })
}

/// Whether a field was just left: it had focus and no longer has it. Nothing else (entering, staying in, staying out) is leaving.
fn left_focus(before: bool, now: bool) -> bool {
    before && !now
}

/// The byte count is the host's; success or failure is the action's separate terminal reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunningFileProgress {
    Waiting,
    Sending(u8),
    Finishing,
}

impl RunningFileProgress {
    fn of(bytes: Option<(u64, u64)>) -> Self {
        match bytes {
            None | Some((_, 0)) => Self::Waiting,
            Some((sent, total)) if sent >= total => Self::Finishing,
            Some((sent, total)) => {
                Self::Sending(((u128::from(sent) * 100) / u128::from(total)) as u8)
            }
        }
    }
}

#[derive(PartialEq)]
struct ActionButton {
    link: Link,
    view: View,
    plugin: PluginInfo,
    action: Action,
    peer: Option<PeerId>,
    palette: Palette,
}
impl Component for ActionButton {
    fn render(&self) -> impl IntoElement {
        let mut confirming = use_state(|| false);
        let mut draft = use_state(String::new);
        let mut invalid = use_state(|| false);
        let mut submitted = use_state(|| false);
        // Whether a key was saved in this session: the field then shows bullets as its placeholder, as if it held the key. It is only a
        // placeholder, never text in the field, so it cannot be sent, copied or pasted over and it vanishes as soon as the person types.
        let mut saved = use_state(|| false);
        // The field's own identity, so that losing focus can be seen from here. A person leaving the field is as good as pressing Enter.
        let field_id = use_hook(AccessibilityId::new_unique);
        let focus = use_focus(field_id);
        let mut was_focused = use_state(|| false);
        let mut request: State<Option<u64>> = use_state(|| None);
        let closing = self.link.clone();
        let closing_action = (
            self.plugin.id.clone(),
            self.action.id.clone(),
            self.peer.clone(),
        );
        let inline = self.action.input == peersus_api::contributions::ActionInput::SecretText;
        use_drop(move || {
            draft.write().zeroize();
            if inline && *submitted.peek() {
                closing.send(Command::CancelAction {
                    plugin: closing_action.0.clone(),
                    action: closing_action.1.clone(),
                    peer: closing_action.2.clone(),
                });
            } else if let Some(id) = *request.peek()
                && closing
                    .client()
                    .snapshot()
                    .extension_prompts
                    .iter()
                    .any(|prompt| prompt.id == id)
            {
                closing.send(Command::AnswerExtensionPrompt {
                    id,
                    accepted: false,
                    secret: None,
                    remove: false,
                });
            }
        });
        let action = self.action.clone();
        let status = self
            .view
            .actions
            .iter()
            .find(|status| {
                status.plugin == self.plugin.id
                    && status.action == action.id
                    && status.peer == self.peer
            })
            .map(|status| status.state);
        let running = status == Some(ActionState::Running);
        let pending = self
            .view
            .extension_prompts
            .iter()
            .find(|prompt| running && self.peer.is_none() && prompt.plugin == self.plugin.id);
        let pending_id = pending.map(|prompt| prompt.id);
        if *request.peek() != pending_id {
            draft.write().zeroize();
            invalid.set(false);
            if !inline {
                submitted.set(false);
            }
            request.set(pending_id);
        }
        let colour = match status {
            Some(ActionState::Done) => self.palette.good,
            Some(ActionState::Failed { .. }) => self.palette.bad,
            _ => self.palette.muted,
        };
        let id = pending_id;
        let link = self.link.clone();
        let plugin = self.plugin.id.clone();
        let action_id = self.action.id.clone();
        let ready = action_ready(&self.view, &self.plugin, &action, self.peer.as_ref());
        let submit = EventHandler::new(move |()| {
            if *submitted.peek() || draft.read().is_empty() || (id.is_none() && !ready) {
                return;
            }
            let Ok(secret) = peersus_core::model::ExtensionSecret::try_from(draft.read().clone())
            else {
                invalid.set(true);
                return;
            };
            draft.write().zeroize();
            invalid.set(false);
            submitted.set(true);
            if inline {
                let save = link.invoke_with_secret(plugin.clone(), action_id.clone(), secret, id);
                spawn(async move {
                    let accepted = save.await.is_ok();
                    invalid.set(!accepted);
                    if accepted {
                        saved.set(true);
                    }
                    submitted.set(false);
                });
            } else if let Some(id) = id {
                link.send(Command::AnswerExtensionPrompt {
                    id,
                    accepted: true,
                    secret: Some(secret),
                    remove: false,
                });
            }
        });
        // Register on every render, including the button before a configuration prompt exists.
        // Refresh the callback so blur answers the current prompt rather than its predecessor.
        use_side_effect_with_deps(&submit, move |on_leave| {
            let now = focus.read().is_focused();
            let before = std::mem::replace(&mut *was_focused.write(), now);
            if left_focus(before, now) {
                on_leave.call(());
            }
        });
        if inline || pending.is_some() {
            let can_submit = !submitted()
                && !draft.read().is_empty()
                && (pending.is_some() || (!running && ready));
            let masked = if saved() && draft.read().is_empty() {
                "••••••••".to_owned()
            } else {
                self.action.label.clone()
            };
            // Readable while it is being typed or pasted, hidden as soon as the field is left; the keyboard is still told it is a secret.
            let profile = if focus.read().is_focused() {
                Profile {
                    mask: None,
                    ..Profile::SECRET
                }
            } else {
                Profile::SECRET
            };
            let input = field(profile, draft)
                .a11y_id(field_id)
                .placeholder(masked)
                .width(Size::fill())
                .enabled(!submitted() && (pending.is_some() || (!running && ready)));
            return crate::field::submit(
                input,
                crate::field::Submit {
                    on_submit: submit,
                    button: true,
                    enabled: can_submit,
                    colour: if invalid() { self.palette.bad } else { colour }.into(),
                },
            )
            .into_element();
        }
        let compact = self.peer.is_some();
        let action_label = if compact {
            action.short_label.as_deref().unwrap_or(&action.label)
        } else {
            &action.label
        };
        let caption = if confirming() {
            i18n::format(
                i18n::language(),
                "Confirm {label}",
                &[("label", action_label)],
            )
        } else {
            i18n::tr(action_label).to_owned()
        };
        let enabled = action_ready(&self.view, &self.plugin, &action, self.peer.as_ref());
        let (link, plugin, peer) = (self.link.clone(), self.plugin.id.clone(), self.peer.clone());
        Button::new()
            .enabled(enabled || running)
            .width(if compact { Size::auto() } else { Size::fill() })
            .layout_variant(if compact {
                ButtonLayoutVariant::Compact
            } else {
                ButtonLayoutVariant::Normal
            })
            .child(
                label()
                    .text(caption)
                    .font_size(if compact { 12. } else { 16. })
                    .color(colour),
            )
            .on_press(move |_| {
                if running {
                    link.send(Command::CancelAction {
                        plugin: plugin.clone(),
                        action: action.id.clone(),
                        peer: peer.clone(),
                    });
                    return;
                }
                if action.destructive && !confirming() {
                    confirming.set(true);
                    return;
                }
                confirming.set(false);
                link.send(Command::InvokeAction {
                    plugin: plugin.clone(),
                    action: action.id.clone(),
                    peer: peer.clone(),
                });
            })
            .into_element()
    }
}

#[derive(PartialEq)]
struct ProgressAction {
    caption: String,
    running: bool,
    progress: Option<(u64, u64)>,
    enabled: bool,
    reduced: bool,
    palette: Palette,
    on_press: EventHandler<()>,
}

impl Component for ProgressAction {
    fn render(&self) -> impl IntoElement {
        let morph = use_animation_with_dependencies(
            &(self.running, self.reduced),
            |config, (running, reduced)| {
                config.on_creation(OnCreation::Run);
                config.on_change(OnChange::Rerun);
                AnimNum::new(
                    if *running { 200. } else { 44. },
                    if *running { 240. } else { 200. },
                )
                .time(if *reduced { 0 } else { 220 })
            },
        );
        let width: f32 = morph.get().value();
        let reveal = ((width - 44.) / 156.).clamp(0., 1.);
        let phase = RunningFileProgress::of(self.progress);
        let fraction = match phase {
            RunningFileProgress::Waiting => 0.,
            RunningFileProgress::Sending(percent) => f32::from(percent) / 100.,
            RunningFileProgress::Finishing => 1.,
        };
        let caption = if self.running {
            running_caption(phase)
        } else {
            self.caption.clone()
        };
        let press = self.on_press.clone();
        Button::new()
            .width(Size::px(width))
            .height(Size::px(44.))
            .with_corner_radius(22. - reveal * 10.)
            .enabled(self.running || self.enabled)
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::px(30.))
                    .main_align(Alignment::center())
                    .cross_align(Alignment::center())
                    .child(
                        rect()
                            .position(Position::new_absolute().left(0.).top(0.))
                            .width(Size::px((width - 24.).max(0.) * fraction))
                            .height(Size::px(30.))
                            .corner_radius(6.)
                            .background(self.palette.send)
                            .opacity(if self.running { 0.2 } else { 0. }),
                    )
                    .child(
                        rect().opacity(reveal).child(
                            label()
                                .text(caption)
                                .font_size(13.)
                                .max_lines(1)
                                .color(self.palette.text),
                        ),
                    ),
            )
            .on_press(move |event: Event<PressEventData>| {
                event.stop_propagation();
                press.call(());
            })
    }
}

/// What a running file action says on its control: how far it is, and that pressing it cancels.
fn running_caption(phase: RunningFileProgress) -> String {
    let cancel = i18n::tr("Cancel");
    match phase {
        RunningFileProgress::Waiting => format!("{} · {cancel}", i18n::tr("Working…")),
        RunningFileProgress::Sending(percent) => format!("{percent}% · {cancel}"),
        RunningFileProgress::Finishing => format!("{} · {cancel}", i18n::tr("Finishing…")),
    }
}

/// One action in the list that opens under the extension's button: its whole name from the left edge, on a row as wide as the widest
/// of the list (`Content::fit` in the list gives every row that width), and the whole row is what is pressed to run it, as in the
/// language list.
#[derive(PartialEq)]
struct DestinationRow {
    caption: String,
    enabled: bool,
    palette: Palette,
    on_press: EventHandler<()>,
}

impl Component for DestinationRow {
    fn render(&self) -> impl IntoElement {
        let (palette, enabled, on_press) = (self.palette, self.enabled, self.on_press.clone());
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::fill_minimum())
            .content(Content::fit())
            .padding((10., 14.))
            .corner_radius(8.)
            .background(if enabled && hovered() {
                palette.surface
            } else {
                palette.raised
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            // The press must not reach the layer under the list, which closes it before the choice is made.
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if enabled {
                    on_press.call(());
                }
            })
            .child(
                rect().width(Size::fill_minimum()).child(
                    label()
                        .text(self.caption.clone())
                        .font_size(15.)
                        // A name too long for the window goes on under itself, whole, and never past the window's edge.
                        .max_width(Size::window_percent(80.))
                        .color(if enabled { palette.text } else { palette.muted }),
                ),
            )
    }
}

/// Optional destinations supplied by extensions for the file already listed on Send.
#[derive(PartialEq)]
pub struct SelectedControls {
    pub link: Link,
    pub view: View,
    pub path: Option<std::path::PathBuf>,
    pub held: Option<(u64, String)>,
    pub palette: Palette,
}
impl Component for SelectedControls {
    fn render(&self) -> impl IntoElement {
        let mut confirming: State<Option<(PluginId, String)>> = use_state(|| None);
        let mut open = use_state(|| false);
        let mut query = use_state(String::new);
        let close = EventHandler::new(move |()| {
            open.set(false);
            query.set(String::new());
            confirming.set(None);
        });
        crate::back::use_back(open(), close.clone());
        let filter = query.read().to_lowercase();
        let mut sources = 0;
        let mut choices = 0;
        let mut source = "";
        let mut active = None;
        let mut rows = crate::contributed_page::menu_rows();
        let mut listed = 0;
        let placing = crate::contributed_page::use_placing();
        // Many destinations are searched, from the first row of the list.
        let offered = self
            .view
            .plugins
            .iter()
            .flat_map(|plugin| plugin.manifest.contributions.actions.iter())
            .filter(|action| action.input == peersus_api::contributions::ActionInput::SelectedFile)
            .count();
        if open() && offered > 6 {
            rows = rows.child(
                field(Profile::SEARCH, query)
                    .width(Size::fill())
                    .placeholder(i18n::tr("Search")),
            );
        }
        // A desktop names the chosen file by its path, in a command any window may send. Where the file is held and not a path,
        // the platform runs the action through the owner itself: that command is never written to a frame.
        let through_platform = try_consume_context::<crate::link::SelectedAction>();
        let mut controls = rect().width(Size::fill()).cross_align(Alignment::center());
        for plugin in self.view.plugins.iter() {
            let mut counted = false;
            for action in plugin
                .manifest
                .contributions
                .actions
                .iter()
                .filter(|action| {
                    action.input == peersus_api::contributions::ActionInput::SelectedFile
                })
            {
                if !counted {
                    sources += 1;
                    source = plugin
                        .package
                        .as_ref()
                        .map_or(plugin.id.as_str(), |p| p.name.as_str());
                    counted = true;
                }
                choices += 1;
                let status = self.view.actions.iter().find(|status| {
                    status.plugin == plugin.id
                        && status.action == action.id
                        && status.peer.is_none()
                });
                let running = status.is_some_and(|status| status.state == ActionState::Running);
                if !running && !open() {
                    continue;
                }
                let name = action.short_label.as_deref().unwrap_or(&action.label);
                if !running && !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                    continue;
                }
                let enabled = (self.path.is_some() != self.held.is_some())
                    && action_ready(&self.view, plugin, action, None)
                    && !running;
                let (link, id, action_id, path, held, through_platform) = (
                    self.link.clone(),
                    plugin.id.clone(),
                    action.id.clone(),
                    self.path.clone(),
                    self.held.clone(),
                    through_platform.clone(),
                );
                let destructive = action.destructive;
                let caption = if let Some(status) =
                    status.filter(|status| status.state != ActionState::Running)
                {
                    format!(
                        "{} · {}",
                        i18n::tr(name),
                        i18n::tr(action_words(Some(status.state)))
                    )
                } else if confirming.read().as_ref() == Some(&(id.clone(), action_id.clone())) {
                    i18n::format(i18n::language(), "Confirm {label}", &[("label", name)])
                } else {
                    i18n::tr(name).to_owned()
                };
                let dismiss = close.clone();
                let on_press = EventHandler::new(move |()| {
                    if running {
                        dismiss.call(());
                        link.send(Command::CancelAction {
                            plugin: id.clone(),
                            action: action_id.clone(),
                            peer: None,
                        });
                        return;
                    }
                    let key = (id.clone(), action_id.clone());
                    if destructive && confirming.peek().as_ref() != Some(&key) {
                        confirming.set(Some(key));
                        return;
                    }
                    dismiss.call(());
                    match &through_platform {
                        Some(run) => {
                            (run.0)(id.clone(), action_id.clone(), path.clone(), held.clone())
                        }
                        None => {
                            if let Some(path) = path.clone() {
                                link.send(Command::InvokeSelectedPathAction {
                                    plugin: id.clone(),
                                    action: action_id.clone(),
                                    path,
                                });
                            }
                        }
                    }
                });
                let progress = status.and_then(|status| status.progress);
                if running && !open() {
                    // The action in progress is the one control, in the place of the button that opens the list.
                    active = Some(ProgressAction {
                        caption,
                        running,
                        progress,
                        enabled,
                        reduced: self.view.settings.reduced_motion,
                        palette: self.palette,
                        on_press,
                    });
                } else {
                    listed += 1;
                    rows = rows.child(DestinationRow {
                        caption: if running {
                            running_caption(RunningFileProgress::of(progress))
                        } else {
                            caption
                        },
                        enabled: running || enabled,
                        palette: self.palette,
                        on_press,
                    });
                }
            }
        }
        if choices == 0 {
            return controls;
        }
        if let Some(active) = active {
            return controls.child(active);
        }
        let title = if sources == 1 {
            source
        } else {
            i18n::tr("Destinations")
        };
        let button =
            Button::new()
                .child(title.to_owned())
                .on_press(move |e: Event<PressEventData>| {
                    e.stop_propagation();
                    open.set(!open());
                });
        // The list hangs under the button as every other list of the application does (the chooser of an extension's pages), over
        // the page, as wide as its widest row. A press anywhere else closes it: a layer as large as the window under the list, which
        // the list is drawn over. It closes on the press and not on the touch's first contact, so that a row pressed is not closed
        // before it was chosen.
        let list = crate::contributed_page::dropdown(
            button,
            open().then_some((rows, listed)),
            false,
            true,
            self.palette,
            placing,
        );
        if open() {
            let dismiss = close.clone();
            controls = controls.child(
                rect()
                    .layer(Layer::Overlay)
                    .position(Position::new_global().top(0.).left(0.))
                    .width(Size::window_percent(100.))
                    .height(Size::window_percent(100.))
                    .on_press(move |_| dismiss.call(())),
            );
        }
        controls.child(list)
    }
}
#[derive(PartialEq)]
struct SettingControl {
    link: Link,
    plugin: PluginId,
    peer: Option<PeerId>,
    setting: Setting,
    value: Value,
    palette: Palette,
    show_label: bool,
}
impl Component for SettingControl {
    fn render(&self) -> impl IntoElement {
        let draft = use_state({
            let value = self.value.clone();
            move || match value {
                Value::Text(s) => s,
                Value::Number(n) => n.to_string(),
                Value::Toggle(b) => b.to_string(),
            }
        });
        let mut error = use_state(|| false);
        let (link, plugin, peer, setting) = (
            self.link.clone(),
            self.plugin.clone(),
            self.peer.clone(),
            self.setting.clone(),
        );
        let send = move |value: Value| {
            let command = match (&setting.public_read, value) {
                (Some(resource), Value::Toggle(enabled)) => Command::SetPublicReadSetting {
                    plugin: plugin.clone(),
                    setting: setting.id.clone(),
                    resource: resource.clone(),
                    enabled,
                },
                (_, value) => Command::SetPluginSetting {
                    plugin: plugin.clone(),
                    peer: peer.clone(),
                    setting: setting.id.clone(),
                    value,
                },
            };
            link.send(command);
        };
        let input: Element = match (&self.setting.kind, &self.value) {
            (Kind::Toggle { .. }, Value::Toggle(on)) => {
                Switch::new(WritableSetter::new(*on, move |on| send(Value::Toggle(on)))).into()
            }
            (Kind::Choice { choices, .. }, Value::Text(selected)) => {
                let mut choices_row = rect().width(Size::fill()).spacing(4.);
                for choice in choices {
                    let (choice, send) = (choice.clone(), send.clone());
                    let label = if &choice == selected {
                        format!("✓ {choice}")
                    } else {
                        choice.clone()
                    };
                    choices_row = choices_row.child(
                        Button::new()
                            .child(label)
                            .on_press(move |_| send(Value::Text(choice.clone()))),
                    );
                }
                choices_row.into()
            }
            (kind @ (Kind::Number { .. } | Kind::Text { .. }), _) => {
                let kind = kind.clone();
                let number = matches!(kind, Kind::Number { .. });
                rect()
                    .width(Size::fill())
                    .spacing(6.)
                    .child(field(Profile::TEXT, draft).width(Size::fill()))
                    .child(Button::new().child(i18n::tr("Save")).on_press(move |_| {
                        let value = if number {
                            draft.read().parse::<i64>().ok().map(Value::Number)
                        } else {
                            Some(Value::Text(draft.read().clone()))
                        };
                        match value.filter(|value| value.check(&kind).is_ok()) {
                            Some(value) => {
                                error.set(false);
                                send(value);
                            }
                            None => error.set(true),
                        }
                    }))
                    .maybe_child(error().then(|| {
                        label()
                            .text(i18n::tr("Enter a value within the declared limits."))
                            .color(self.palette.bad)
                            .font_size(12.)
                    }))
                    .into()
            }
            _ => label()
                .text(i18n::tr("Invalid saved setting. Restore a valid value."))
                .color(self.palette.bad)
                .into(),
        };
        let width = if self.setting.scope == Scope::Global {
            Size::fill()
        } else if self.show_label || !matches!(self.setting.kind, Kind::Toggle { .. }) {
            Size::px(144.)
        } else {
            Size::auto()
        };
        let mut control = rect().width(width).spacing(5.);
        if self.setting.scope == Scope::Global {
            control = control
                .child(
                    label()
                        .text(i18n::tr(&self.setting.label).to_owned())
                        .color(self.palette.text)
                        .font_size(14.),
                )
                .maybe_child(self.setting.help.clone().map(|help| {
                    label()
                        .text(i18n::tr(&help).to_owned())
                        .color(self.palette.muted)
                        .font_size(12.)
                }));
            control = control.maybe_child(self.setting.public_read.as_ref().and_then(|scope| scope.url()).map(|url| {
                label().text(i18n::format(
                    i18n::language(),
                    "Turn on to allow downloads from {address}. The server sees the requested resources and this device’s network address. No files, saved credentials or cookies are sent.",
                    &[("address", url.as_str())],
                )).color(self.palette.muted).font_size(12.)
            }));
        } else if self.show_label {
            control = control.child(
                label()
                    .text(i18n::tr(&self.setting.label).to_owned())
                    .color(self.palette.text)
                    .font_size(11.),
            );
        }
        control.child(input)
    }
}

impl Controls {
    fn settings_for(&self, peer: Option<PeerId>, values: Option<&Values>, scope: Scope) -> Element {
        let empty = Values::default();
        let values = values.unwrap_or(&empty);
        let mut rows = rect().width(Size::fill()).spacing(12.);
        for setting in self
            .plugin
            .manifest
            .contributions
            .settings
            .iter()
            .filter(|setting| setting.scope == scope)
        {
            let value = values.effective(setting);
            rows = rows.child(crate::settings_navigation::anchor(
                crate::settings_search::Target::Setting {
                    plugin: self.plugin.id.clone(),
                    id: setting.id.clone(),
                    peer: peer.clone(),
                },
                // A global setting fills the card: its wrapper must say so too. Sized to its content, the wrapper and a
                // row that fills it depended on each other's width, so the help text was measured at one width and drawn
                // at another, and after any width change the card kept a height shorter than what it drew.
                rect()
                    .width(if scope == Scope::Global {
                        Size::fill()
                    } else {
                        Size::auto()
                    })
                    .key(format!("{}:{peer:?}:{value:?}", setting.id))
                    .child(SettingControl {
                        link: self.link.clone(),
                        plugin: self.plugin.id.clone(),
                        peer: peer.clone(),
                        setting: setting.clone(),
                        value,
                        palette: self.palette,
                        show_label: true,
                    }),
                self.palette,
            ));
        }
        rows.into()
    }
}

impl Component for Controls {
    fn render(&self) -> impl IntoElement {
        let mut body = rect().width(Size::fill()).spacing(12.);
        if let Some(platforms) = &self.plugin.manifest.platforms {
            let names: Vec<_> = platforms
                .iter()
                .map(|os| match os {
                    peersus_api::platform::OperatingSystem::Android => "Android",
                    peersus_api::platform::OperatingSystem::Linux => "Linux",
                    peersus_api::platform::OperatingSystem::Macos => "macOS",
                    peersus_api::platform::OperatingSystem::Windows => "Windows",
                    peersus_api::platform::OperatingSystem::Ios => "iOS",
                    peersus_api::platform::OperatingSystem::Freebsd => "FreeBSD",
                })
                .collect();
            body = body.child(
                label()
                    .text(i18n::format(
                        i18n::language(),
                        "Supported platforms: {platforms}",
                        &[("platforms", &names.join(" · "))],
                    ))
                    .color(self.palette.muted)
                    .font_size(12.),
            );
        } else {
            body = body.child(
                label()
                    .text(i18n::tr(
                        "Platform support not declared by this legacy plugin.",
                    ))
                    .color(self.palette.muted)
                    .font_size(12.),
            );
        }
        body = body.child(self.settings_for(
            None,
            self.view.settings.plugins.get(&self.plugin.id),
            Scope::Global,
        ));
        // The extension's pages placed in Settings: drawn here, in its own settings, by the same code as every extension page. All
        // of them together are as tall as they are up to `SETTINGS_PAGES_HEIGHT`, and scroll inside the card past that, so an
        // extension with long or many pages never makes its card, and the column it is in, as long as everything it draws. The
        // scroll view is as tall as its content (`auto`): its default fills the room, which is not a height inside a card.
        let pages: Vec<_> = crate::contributed_page::pages_here(
            &self.view,
            peersus_api::contributions::Placement::Settings,
        )
        .into_iter()
        .filter(|(plugin, _)| plugin.id == self.plugin.id)
        .collect();
        if !pages.is_empty() {
            let mut drawn = rect().width(Size::fill()).spacing(12.);
            for (plugin, page) in pages {
                drawn = drawn.child(crate::contributed_page::Body {
                    link: self.link.clone(),
                    view: self.view.clone(),
                    plugin,
                    page,
                    pictures: self.pictures.clone(),
                    palette: self.palette,
                    embedded: true,
                });
            }
            body = body.child(
                ScrollView::new()
                    .width(Size::fill())
                    .height(Size::auto())
                    .max_height(Size::px(SETTINGS_PAGES_HEIGHT))
                    .child(drawn),
            );
        }
        for action in self
            .plugin
            .manifest
            .contributions
            .actions
            .iter()
            .filter(|action| {
                !action.needs_device
                    && matches!(
                        action.input,
                        peersus_api::contributions::ActionInput::None
                            | peersus_api::contributions::ActionInput::SecretText
                    )
            })
        {
            body = body.child(crate::settings_navigation::anchor(
                crate::settings_search::Target::Action {
                    plugin: self.plugin.id.clone(),
                    id: action.id.clone(),
                    peer: None,
                },
                ActionButton {
                    link: self.link.clone(),
                    view: self.view.clone(),
                    plugin: self.plugin.clone(),
                    action: action.clone(),
                    peer: None,
                    palette: self.palette,
                },
                self.palette,
            ));
        }
        let settings: Vec<_> = self
            .plugin
            .manifest
            .contributions
            .settings
            .iter()
            .filter(|setting| setting.scope == Scope::Device)
            .collect();
        let actions: Vec<_> = self
            .plugin
            .manifest
            .contributions
            .actions
            .iter()
            .filter(|action| action.needs_device)
            .collect();
        for action in actions.iter().filter(|action| action.short_label.is_some()) {
            body = body.child(
                label()
                    .text(i18n::tr(&action.label).to_owned())
                    .color(self.palette.text)
                    .font_size(14.),
            );
        }
        // Explain each declaration once; device rows contain values and actions, not duplicate prose.
        for setting in &settings {
            body = body.child(
                rect()
                    .width(Size::fill())
                    .spacing(4.)
                    .child(
                        label()
                            .text(i18n::tr(&setting.label).to_owned())
                            .color(self.palette.text)
                            .font_size(14.),
                    )
                    .maybe_child(setting.help.as_ref().map(|help| {
                        label()
                            .text(i18n::tr(help).to_owned())
                            .color(self.palette.muted)
                            .font_size(12.)
                    })),
            );
        }
        if !actions.is_empty()
            && self.view.devices.iter().any(|device| {
                actions.iter().any(|action| {
                    !action_ready(
                        &self.view,
                        &self.plugin,
                        action,
                        Some(&PeerId(device.id.clone())),
                    )
                })
            })
        {
            body = body.child(label().text(i18n::tr("Start the plugin, select an online device when required, and allow its permissions.")).color(self.palette.muted).font_size(12.));
        }
        if settings.is_empty() && actions.is_empty() {
            return body;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Preserve a readable identity and intrinsic control widths at every interface size.
        // Narrow screens pan the same row instead of squeezing names and actions into letters.
        for device in &self.view.devices {
            let peer = PeerId(device.id.clone());
            let mut identity = rect()
                .width(Size::auto())
                .spacing(2.)
                .child(
                    label()
                        .text(device.name.clone())
                        .color(self.palette.text)
                        .font_size(14.)
                        .font_weight(FontWeight::BOLD)
                        .max_lines(1)
                        // As wide as the name: the row sizes itself to its content and pans sideways, so a width taken
                        // from it would be circular, and the name was cut or drawn outside the row.
                        .max_width(Size::Inner),
                )
                .child(
                    label()
                        .text(i18n::tr(match device.presence {
                            Presence::Online => "Online",
                            Presence::Sleeping => "Sleeping",
                            Presence::Offline => "Offline",
                        }))
                        .color(self.palette.muted)
                        .font_size(11.)
                        .max_lines(1)
                        .max_width(Size::Inner),
                );
            if self
                .view
                .devices
                .iter()
                .any(|other| other.id != device.id && other.name == device.name)
            {
                identity = identity.child(
                    label()
                        .text(crate::view::seen_words_in(
                            i18n::language(),
                            Some(device.paired_at),
                            now,
                        ))
                        .color(self.palette.muted)
                        .font_size(10.)
                        .max_lines(1)
                        .max_width(Size::Inner),
                );
            }
            let mut row = rect()
                .key(device.id.clone())
                .width(Size::auto())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                // The toolkit draws a horizontal scrollbar over the last 20 px of the scroller and gives it no room; the
                // scroller is as tall as this row, so without this the bar sat on the names and settings.
                .padding(Gaps::new(0., 0., 20., 0.))
                .spacing(4.)
                .child(avatar_view::Avatar {
                    picture: device.picture.and_then(|digest| self.pictures.get(&digest)),
                    name: device.name.clone(),
                    size: 24.,
                    palette: self.palette,
                })
                .child(identity);
            let empty = Values::default();
            let values = self
                .view
                .plugin_values
                .get(&peer)
                .and_then(|values| values.get(&self.plugin.id))
                .unwrap_or(&empty);
            for setting in &settings {
                let value = values.effective(setting);
                row = row.child(crate::settings_navigation::inline_anchor(
                    crate::settings_search::Target::Setting {
                        plugin: self.plugin.id.clone(),
                        id: setting.id.clone(),
                        peer: Some(peer.clone()),
                    },
                    rect()
                        .key(format!("{}:{peer:?}:{value:?}", setting.id))
                        .child(SettingControl {
                            link: self.link.clone(),
                            plugin: self.plugin.id.clone(),
                            peer: Some(peer.clone()),
                            setting: (*setting).clone(),
                            value,
                            palette: self.palette,
                            show_label: settings.len() > 1,
                        }),
                    self.palette,
                ));
            }
            for action in &actions {
                row = row.child(crate::settings_navigation::inline_anchor(
                    crate::settings_search::Target::Action {
                        plugin: self.plugin.id.clone(),
                        id: action.id.clone(),
                        peer: Some(peer.clone()),
                    },
                    ActionButton {
                        link: self.link.clone(),
                        view: self.view.clone(),
                        plugin: self.plugin.clone(),
                        action: (*action).clone(),
                        peer: Some(peer.clone()),
                        palette: self.palette,
                    },
                    self.palette,
                ));
            }
            body = body.child(
                ScrollView::new()
                    .width(Size::fill())
                    .height(Size::auto())
                    .direction(Direction::Horizontal)
                    .child(row),
            );
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn action_errors_provide_recovery_and_never_say_they_completed() {
        for reason in [
            ActionFailure::NotAllowed,
            ActionFailure::Unavailable,
            ActionFailure::Stopped,
            ActionFailure::TimedOut,
            ActionFailure::Busy,
            ActionFailure::Other,
        ] {
            assert_ne!(
                action_words(Some(ActionState::Failed { reason })),
                action_words(Some(ActionState::Done))
            );
        }
    }
    #[test]
    fn only_leaving_a_focused_field_counts_as_leaving() {
        // Entering, staying out and staying in are not leaving; only focused -> not focused is.
        assert!(!left_focus(false, true));
        assert!(!left_focus(false, false));
        assert!(!left_focus(true, true));
        assert!(left_focus(true, false));
    }
    #[cfg(feature = "headless")]
    #[test]
    fn file_action_waits_for_the_terminal_reply_even_after_all_bytes_arrive() {
        use freya_testing::TestingRunner;
        type Sample = (Option<(u64, u64)>, ActionState);
        fn button() -> impl IntoElement {
            let (progress, state) = use_consume::<State<Sample>>()();
            ProgressAction {
                caption: action_words(Some(state)).to_owned(),
                running: state == ActionState::Running,
                progress,
                enabled: true,
                reduced: true,
                palette: crate::theme::DARK,
                on_press: EventHandler::new(|()| {}),
            }
        }
        let (mut runner, mut sample) = TestingRunner::new(
            button,
            (300., 80.).into(),
            |runner| {
                runner
                    .provide_root_context(|| State::<Sample>::create((None, ActionState::Running)))
            },
            1.,
        );
        runner.animation_clock().disable();
        for (progress, state, caption) in [
            (Some((999, 1000)), ActionState::Running, "99% · Cancel"),
            (
                Some((1000, 1000)),
                ActionState::Running,
                "Finishing… · Cancel",
            ),
            (None, ActionState::Done, "Completed"),
        ] {
            sample.set((progress, state));
            runner.poll_n(std::time::Duration::from_millis(2), 12);
            runner.sync_and_update();
            runner.render_to_surface();
            let drawn: Vec<_> = runner.find_many(|node, element| {
                let label = Label::try_downcast(element)?;
                node.is_visible().then(|| label.text.into_owned())
            });
            assert!(drawn.iter().any(|text| text == caption), "{drawn:?}");
        }
    }

    #[test]
    fn acknowledged_bytes_do_not_claim_the_action_has_completed() {
        use RunningFileProgress::{Finishing, Sending, Waiting};
        let phase = RunningFileProgress::of;
        assert_eq!(phase(None), Waiting);
        assert_eq!(phase(Some((5, 0))), Waiting);
        assert_eq!(phase(Some((0, 10))), Sending(0));
        assert_eq!(phase(Some((5, 10))), Sending(50));
        assert_eq!(phase(Some((u64::MAX - 1, u64::MAX))), Sending(99));
        assert_eq!(phase(Some((10, 10))), Finishing);
        assert_eq!(phase(Some((15, 10))), Finishing);
    }
}
