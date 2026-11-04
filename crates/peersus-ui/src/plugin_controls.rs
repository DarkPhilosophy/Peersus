//! Generic plugin contribution controls. No plugin name or OS-specific behavior lives here.
use crate::view::action_words;
use crate::{
    avatar_view, i18n,
    link::Link,
    shelf,
    theme::Palette,
    view::{Presence, View},
};
use freya::prelude::*;
#[cfg(test)]
use peersus_core::model::ActionFailure;
use peersus_core::{
    model::{ActionState, Command, PeerId, PluginInfo, PluginStatus, Standing},
    plugins::{PluginId, Value, Values},
};
use peersus_plugin_api::contributions::{Action, Kind, Scope, Setting};

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
        peersus_plugin_api::permission::Permission::new(plugin.id.as_str(), name)
            .is_some_and(|permission| held.is_some_and(|grants| grants.allows(&permission)))
    })
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
        let enabled = action_ready(&self.view, &self.plugin, &action, self.peer.as_ref())
            && status != Some(ActionState::Running);
        let (link, plugin, peer) = (self.link.clone(), self.plugin.id.clone(), self.peer.clone());
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
        let (cancel_link, cancel_plugin, cancel_action, cancel_peer) = (
            self.link.clone(),
            self.plugin.id.clone(),
            self.action.id.clone(),
            self.peer.clone(),
        );
        rect().width(if compact { Size::auto() } else { Size::fill() }).spacing(6.)
            .maybe(compact, |row| row.horizontal().cross_align(Alignment::center()).content(Content::flex()))
            .child(Button::new().enabled(enabled).width(if compact { Size::auto() } else { Size::fill() }).layout_variant(if compact { ButtonLayoutVariant::Compact } else { ButtonLayoutVariant::Normal })
                .child(label().text(caption).font_size(if compact { 12. } else { 16. })).on_press(move |_| {
                if action.destructive && !confirming() { confirming.set(true); return; }
                confirming.set(false);
                link.send(Command::InvokeAction { plugin: plugin.clone(), action: action.id.clone(), peer: peer.clone() });
            }))
            .maybe_child((status == Some(ActionState::Running)).then(|| Button::new().child(i18n::tr("Cancel")).on_press(move |_| {
                cancel_link.send(Command::CancelAction { plugin: cancel_plugin.clone(), action: cancel_action.clone(), peer: cancel_peer.clone() });
            })))
            .maybe_child(confirming().then(|| Button::new().child(i18n::tr("Keep")).on_press(move |_| confirming.set(false))))
            .maybe_child((self.peer.is_none() && !enabled && status != Some(ActionState::Running)).then(|| label().text(i18n::tr("Start the plugin, select an online device when required, and allow its permissions.")).color(self.palette.muted).font_size(12.)))
            .maybe_child(status.map(|_| label().text(i18n::tr(action_words(status))).color(self.palette.text).font_size(13.)))
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
        let mut controls = rect().width(Size::fill()).spacing(6.);
        // Serialized IPC intentionally cannot carry trusted selection contexts.
        for plugin in self
            .view
            .plugins
            .iter()
            .filter(|_| cfg!(target_os = "android"))
        {
            for action in plugin
                .manifest
                .contributions
                .actions
                .iter()
                .filter(|action| {
                    action.input == peersus_plugin_api::contributions::ActionInput::SelectedFile
                })
            {
                let running = self.view.actions.iter().any(|status| {
                    status.plugin == plugin.id
                        && status.action == action.id
                        && status.peer.is_none()
                        && status.state == ActionState::Running
                });
                let enabled = (self.path.is_some() != self.held.is_some())
                    && action_ready(&self.view, plugin, action, None)
                    && !running;
                let (link, id, action_id, path, held) = (
                    self.link.clone(),
                    plugin.id.clone(),
                    action.id.clone(),
                    self.path.clone(),
                    self.held.clone(),
                );
                let destructive = action.destructive;
                let caption =
                    if confirming.read().as_ref() == Some(&(id.clone(), action_id.clone())) {
                        i18n::format(
                            i18n::language(),
                            "Confirm {label}",
                            &[("label", &action.label)],
                        )
                    } else {
                        i18n::tr(&action.label).to_owned()
                    };
                controls = controls.child(
                    Button::new()
                        .width(Size::fill())
                        .enabled(enabled)
                        .child(caption)
                        .on_press(move |_| {
                            let key = (id.clone(), action_id.clone());
                            if destructive && confirming.peek().as_ref() != Some(&key) {
                                confirming.set(Some(key));
                                return;
                            }
                            confirming.set(None);
                            link.send(Command::InvokeSelectedAction {
                                plugin: id.clone(),
                                action: action_id.clone(),
                                path: path.clone(),
                                held: held.clone(),
                            })
                        }),
                );
                if !enabled && !running {
                    let hint = if self.path.is_some() == self.held.is_some() {
                        "Choose exactly one file for this action."
                    } else {
                        "Start the plugin, select an online device when required, and allow its permissions."
                    };
                    controls = controls.child(
                        label()
                            .text(i18n::tr(hint))
                            .color(self.palette.muted)
                            .font_size(12.),
                    );
                }
                if running {
                    let (link, id, action_id) =
                        (self.link.clone(), plugin.id.clone(), action.id.clone());
                    controls = controls.child(Button::new().child(i18n::tr("Cancel")).on_press(
                        move |_| {
                            link.send(Command::CancelAction {
                                plugin: id.clone(),
                                action: action_id.clone(),
                                peer: None,
                            })
                        },
                    ));
                }
                if let Some(status) = self.view.actions.iter().find(|status| {
                    status.plugin == plugin.id
                        && status.action == action.id
                        && status.peer.is_none()
                }) {
                    controls = controls.child(
                        label()
                            .text(i18n::tr(action_words(Some(status.state))))
                            .color(self.palette.text)
                            .font_size(13.),
                    );
                }
            }
        }
        controls
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
            link.send(Command::SetPluginSetting {
                plugin: plugin.clone(),
                peer: peer.clone(),
                setting: setting.id.clone(),
                value,
            })
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
                    .child(Input::new(draft).width(Size::fill()))
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
                rect()
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
                    peersus_plugin_api::platform::OperatingSystem::Android => "Android",
                    peersus_plugin_api::platform::OperatingSystem::Linux => "Linux",
                    peersus_plugin_api::platform::OperatingSystem::Macos => "macOS",
                    peersus_plugin_api::platform::OperatingSystem::Windows => "Windows",
                    peersus_plugin_api::platform::OperatingSystem::Ios => "iOS",
                    peersus_plugin_api::platform::OperatingSystem::Freebsd => "FreeBSD",
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
        for action in self
            .plugin
            .manifest
            .contributions
            .actions
            .iter()
            .filter(|action| {
                !action.needs_device
                    && action.input == peersus_plugin_api::contributions::ActionInput::None
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
                        .max_lines(1),
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
                        .max_lines(1),
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
                        .max_lines(1),
                );
            }
            let mut row = rect()
                .key(device.id.clone())
                .width(Size::auto())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
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
}
