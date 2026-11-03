//! Ordered relay fallbacks shared by desktop and Android. Direct paths always take priority.
//! Access tokens use a separate command and are never placed in the preference patch.
use freya::prelude::*;
use peersus_core::{
    Settings,
    config::SettingsPatch,
    model::SecretText,
    relay::{PROJECT_RELAY_URL, RelayPolicy, RelayTarget},
};

use crate::{i18n, link::Link, theme::Palette};

fn policy_patch(policy: RelayPolicy) -> SettingsPatch {
    let enabled = policy.has_enabled_targets();
    SettingsPatch {
        relay_policy: Some(policy),
        ..crate::view::relay_patch(enabled)
    }
}

fn move_target(policy: &RelayPolicy, from: usize, to: usize) -> RelayPolicy {
    let mut next = policy.clone();
    next.swap_targets(from, to);
    next
}

fn save_target(policy: &RelayPolicy, target: RelayTarget) -> RelayPolicy {
    let mut next = policy.clone();
    let existing = next.targets.iter().position(|current| {
        matches!(
            (current, &target),
            (RelayTarget::Custom(_), RelayTarget::Custom(_))
                | (RelayTarget::Project(_), RelayTarget::Project(_))
                | (RelayTarget::Public, RelayTarget::Public)
        )
    });
    if let Some(index) = existing {
        next.targets[index] = target;
        next.disabled.remove(&index);
    } else {
        next.targets.push(target);
    }
    next
}

fn remove_custom_target(policy: &RelayPolicy) -> RelayPolicy {
    let mut next = policy.clone();
    if let Some(index) = next
        .targets
        .iter()
        .position(|target| matches!(target, RelayTarget::Custom(_)))
    {
        next.targets.remove(index);
        next.disabled = next
            .disabled
            .into_iter()
            .filter_map(|position| {
                if position == index {
                    None
                } else {
                    Some(position - usize::from(position > index))
                }
            })
            .collect();
    }
    next
}

/// Ordered fallbacks, address editing and the custom target's separate credential controls.
#[derive(PartialEq)]
pub struct Picker {
    pub link: Link,
    pub settings: Settings,
    pub diagnostics: Option<peersus_core::relay::RelayDiagnostics>,
    pub palette: Palette,
    /// A phone: the buttons go one under the other, so that none runs past the edge of the screen.
    pub compact: bool,
}

/// A choice is applied by the owner, and what it answers is shown: "transfers are going" or "not a usable address" are things the
/// person must be able to read, not a button that seems to do nothing.
fn apply_choice(mut said: State<Option<String>>, link: Link, patch: SettingsPatch) {
    said.set(None);
    spawn(async move {
        if let Err(sentence) = link.patch_settings(patch).await {
            said.set(Some(sentence));
        }
    });
}

/// Hand over the token typed (or an empty one, to forget the saved one) and say how it went. The field is emptied as soon as the token
/// is handed over, so that it is not kept on the screen, and "saved" is told only once the owner has said it was.
fn keep_token(
    mut said: State<Option<String>>,
    mut kept: State<Option<&'static str>>,
    mut token: State<String>,
    link: Link,
    text: String,
    done: &'static str,
) {
    said.set(None);
    kept.set(None);
    let Some(secret) = SecretText::new(&text) else {
        said.set(Some(
            i18n::tr("The relay token must be one line of at most 512 characters").to_owned(),
        ));
        return;
    };
    token.set(String::new());
    spawn(async move {
        match link.set_relay_token(secret).await {
            Ok(()) => kept.set(Some(done)),
            Err(sentence) => said.set(Some(sentence)),
        }
    });
}

impl Component for Picker {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut policy = self.settings.network.effective_relay_policy();
        // Disabled built-ins remain visible without silently enabling a new service.
        for target in [
            RelayTarget::Project(PROJECT_RELAY_URL.to_owned()),
            RelayTarget::Public,
        ] {
            let present = policy.targets.iter().any(|existing| {
                matches!(
                    (existing, &target),
                    (RelayTarget::Project(_), RelayTarget::Project(_))
                        | (RelayTarget::Public, RelayTarget::Public)
                )
            });
            if !present {
                policy.disabled.insert(policy.targets.len());
                policy.targets.push(target);
            }
        }
        let saved_custom = policy
            .targets
            .iter()
            .find_map(|target| match target {
                RelayTarget::Custom(url) => Some(url.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let saved_project = policy
            .targets
            .iter()
            .find_map(|target| match target {
                RelayTarget::Project(url) => Some(url.clone()),
                _ => None,
            })
            .unwrap_or_else(|| PROJECT_RELAY_URL.to_owned());
        let address = use_state(move || saved_custom.clone());
        let project_address = use_state(move || saved_project.clone());
        let token = use_state(String::new);
        let mut said: State<Option<String>> = use_state(|| None);
        let kept: State<Option<&'static str>> = use_state(|| None);
        let mut advanced = use_state(|| false);
        let direct_wait = use_state({
            let value = policy.direct_wait_ms.to_string();
            move || value
        });
        let failover_wait = use_state({
            let value = policy.failover_wait_ms.to_string();
            move || value
        });
        let retry_primary = use_state({
            let value = policy.retry_primary_ms.to_string();
            move || value
        });
        let mut block = rect()
            .width(Size::fill())
            .spacing(10.)
            .child(
                label()
                    .text(i18n::tr("Relay"))
                    .color(palette.text)
                    .font_size(15.)
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                rect()
                    .width(Size::fill())
                    .padding(16.)
                    .spacing(6.)
                    .corner_radius(12.)
                    .background(palette.raised)
                    .child(
                        label()
                            .text(i18n::tr("Direct connection is always tried first."))
                            .color(palette.good)
                            .font_size(15.)
                            .font_weight(FontWeight::BOLD),
                    ),
            );

        if let Some(diagnostics) = self.diagnostics {
            use peersus_core::relay::RelayHealth;
            let state = match diagnostics.health {
                RelayHealth::Disabled => "No relay",
                RelayHealth::Connecting => "Connecting to home relay",
                RelayHealth::Connected => "Home relay connected",
                RelayHealth::Unavailable => "Home relay unavailable",
                RelayHealth::Refused => "Home relay refused access",
            };
            let position = diagnostics
                .active_target
                .map(|index| format!(" · #{}", index + 1))
                .unwrap_or_default();
            block = block.child(
                label()
                    .text(format!("{}{position}", i18n::tr(state)))
                    .color(
                        if matches!(
                            diagnostics.health,
                            RelayHealth::Refused | RelayHealth::Unavailable
                        ) {
                            palette.warning
                        } else {
                            palette.muted
                        },
                    )
                    .font_size(13.),
            );
        }

        if !policy.has_enabled_targets() && self.diagnostics.is_none() {
            block = block.child(
                label()
                    .text(i18n::tr("No relay"))
                    .color(palette.muted)
                    .font_size(13.),
            );
        }
        for (index, target) in policy.targets.iter().enumerate() {
            let title = match target {
                RelayTarget::Custom(url) => format!("{} · {url}", i18n::tr("My own relay")),
                RelayTarget::Project(url) => format!("{} · {url}", i18n::tr("Project relay")),
                RelayTarget::Public => i18n::tr("Public relays (n0)").to_owned(),
            };
            let mut controls = rect()
                .horizontal()
                .spacing(8.)
                .cross_align(Alignment::center());
            for (to, caption, enabled) in [
                (index.saturating_sub(1), "↑", index > 0),
                (index + 1, "↓", index + 1 < policy.targets.len()),
            ] {
                let next = move_target(&policy, index, to);
                let link = self.link.clone();
                controls = controls.child(
                    Button::new()
                        .enabled(enabled)
                        .on_press(move |_| {
                            apply_choice(said, link.clone(), policy_patch(next.clone()));
                        })
                        .child(caption),
                );
            }
            let current = policy.clone();
            let link = self.link.clone();
            controls = controls.child(Switch::new(WritableSetter::new(
                !policy.disabled.contains(&index),
                move |enabled: bool| {
                    let mut next = current.clone();
                    if enabled {
                        next.disabled.remove(&index);
                    } else {
                        next.disabled.insert(index);
                    }
                    apply_choice(said, link.clone(), policy_patch(next));
                },
            )));
            block = block.child(
                rect()
                    .width(Size::fill())
                    .spacing(8.)
                    .padding(14.)
                    .corner_radius(10.)
                    .background(palette.raised)
                    .maybe(!self.compact, |row| {
                        row.horizontal()
                            .content(Content::flex())
                            .cross_align(Alignment::center())
                    })
                    .child(
                        rect()
                            .width(if self.compact {
                                Size::fill()
                            } else {
                                Size::flex(1.)
                            })
                            .child(
                                label()
                                    .text(format!("{}. {title}", index + 1))
                                    .color(if policy.disabled.contains(&index) {
                                        palette.muted
                                    } else {
                                        palette.text
                                    })
                                    .font_size(13.),
                            ),
                    )
                    .child(controls),
            );
        }
        block = block.child(label()
            .text(i18n::tr("Application traffic waits for a direct path before using relay fallbacks in this order. Relay signalling may begin earlier. Fallback traffic is end-to-end encrypted, but connection metadata is visible. No application telemetry is sent."))
            .color(palette.warning).font_size(12.));
        block = block.child(
            Button::new()
                .on_press(move |_| advanced.set(!advanced()))
                .child(i18n::tr("Advanced relay options")),
        );
        // Address edits replace that service in place; adding a disabled service appends it.
        for (title, field, custom) in [
            ("Project relay", project_address, false),
            ("My own relay", address, true),
        ] {
            if !custom && !advanced() {
                continue;
            }
            let current = policy.clone();
            let link = self.link.clone();
            let save = move |_| {
                let url = field.read().trim().to_owned();
                if !peersus_core::config::valid_relay_url(&url) {
                    said.set(Some(i18n::tr("The relay address must start with https:// and be a single address without spaces").to_owned()));
                    return;
                }
                let target = if custom {
                    RelayTarget::Custom(url)
                } else {
                    RelayTarget::Project(url)
                };
                let next = save_target(&current, target);
                apply_choice(said, link.clone(), policy_patch(next));
            };
            block = block.child(
                rect()
                    .width(Size::fill())
                    .spacing(8.)
                    .child(
                        label()
                            .text(i18n::tr(title))
                            .color(palette.text)
                            .font_size(13.),
                    )
                    .child(
                        Input::new(field)
                            .placeholder(i18n::tr("https://relay.example.org"))
                            .width(Size::fill()),
                    )
                    .child(
                        Button::new()
                            .on_press(save)
                            .child(i18n::tr("Save and enable relay")),
                    ),
            );
        }
        if advanced() {
            if policy
                .targets
                .iter()
                .any(|target| matches!(target, RelayTarget::Custom(_)))
            {
                let current = policy.clone();
                let link = self.link.clone();
                block = block.child(
                    Button::new()
                        .on_press(move |_| {
                            apply_choice(
                                said,
                                link.clone(),
                                policy_patch(remove_custom_target(&current)),
                            );
                        })
                        .child(i18n::tr("Remove")),
                );
            }
            for (caption, state) in [
                ("Direct wait (ms)", direct_wait),
                ("Relay timeout (ms)", failover_wait),
                ("Retry preferred relay (ms)", retry_primary),
            ] {
                block = block.child(
                    rect()
                        .width(Size::fill())
                        .spacing(5.)
                        .child(
                            label()
                                .text(i18n::tr(caption))
                                .color(palette.muted)
                                .font_size(12.),
                        )
                        .child(Input::new(state).width(Size::fill())),
                );
            }
            let current = policy.clone();
            let link = self.link.clone();
            block = block.child(
                Button::new()
                    .on_press(move |_| {
                        let values = (
                            direct_wait.read().parse::<u32>(),
                            failover_wait.read().parse::<u32>(),
                            retry_primary.read().parse::<u32>(),
                        );
                        let (Ok(direct), Ok(failover), Ok(retry)) = values else {
                            said.set(Some(i18n::tr("Enter valid relay timings").to_owned()));
                            return;
                        };
                        let next = RelayPolicy {
                            direct_wait_ms: direct,
                            failover_wait_ms: failover,
                            retry_primary_ms: retry,
                            ..current.clone()
                        };
                        if !next.is_valid() {
                            said.set(Some(i18n::tr("Enter valid relay timings").to_owned()));
                            return;
                        }
                        apply_choice(said, link.clone(), policy_patch(next));
                    })
                    .child(i18n::tr("Save")),
            );
            if policy
                .targets
                .iter()
                .any(|target| matches!(target, RelayTarget::Custom(_)))
            {
                let (save_link, forget_link) = (self.link.clone(), self.link.clone());
                let save_token = move |_| {
                    keep_token(
                        said,
                        kept,
                        token,
                        save_link.clone(),
                        token.read().clone(),
                        "Token saved",
                    )
                };
                let forget_token = move |_| {
                    keep_token(
                        said,
                        kept,
                        token,
                        forget_link.clone(),
                        String::new(),
                        "Token forgotten",
                    )
                };
                block = block.child(rect().width(Size::fill()).spacing(8.)
                .child(Input::new(token).mode(InputMode::Hidden('•'))
                    .placeholder(i18n::tr("Access token (kept on this device only)")).width(Size::fill()))
                .child(rect().maybe(!self.compact, |row| row.horizontal()).spacing(8.)
                    .child(Button::new().on_press(save_token).child(i18n::tr("Save token")))
                    .child(Button::new().on_press(forget_token).child(i18n::tr("Forget token"))))
                .child(label().text(i18n::tr("The token is sent only to your custom relay, never to project or public relays."))
                    .color(palette.muted).font_size(13.)));
            }
        }
        if let Some(done) = kept() {
            block = block.child(
                label()
                    .text(i18n::tr(done))
                    .color(palette.good)
                    .font_size(13.),
            );
        }
        if let Some(sentence) = said.read().clone() {
            block = block.child(
                label()
                    .text(i18n::tr(&sentence).to_owned())
                    .color(palette.bad)
                    .font_size(13.),
            );
        }
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reordering_preserves_every_target_and_timing_and_applies_to_saved_settings() {
        let policy = RelayPolicy {
            targets: vec![
                RelayTarget::Custom("https://custom.example".into()),
                RelayTarget::Project(PROJECT_RELAY_URL.into()),
                RelayTarget::Public,
            ],
            direct_wait_ms: 1500,
            ..RelayPolicy::default()
        };
        let reordered = move_target(&policy, 2, 0);
        assert_eq!(
            reordered.targets,
            vec![
                RelayTarget::Public,
                RelayTarget::Project(PROJECT_RELAY_URL.into()),
                RelayTarget::Custom("https://custom.example".into()),
            ]
        );
        assert_eq!(reordered.direct_wait_ms, 1500);
        let saved = policy_patch(reordered.clone())
            .apply(&Settings::default())
            .unwrap();
        assert_eq!(saved.network.effective_relay_policy(), reordered);
        assert_eq!(
            policy.targets[0],
            RelayTarget::Custom("https://custom.example".into())
        );
        assert_eq!(move_target(&policy, 0, usize::MAX), policy);
    }

    #[test]
    fn editing_an_address_keeps_its_priority_and_public_is_not_duplicated() {
        let policy = RelayPolicy::default();
        let edited = save_target(
            &policy,
            RelayTarget::Project("https://other.example".into()),
        );
        assert_eq!(
            edited.targets,
            vec![
                RelayTarget::Project("https://other.example".into()),
                RelayTarget::Public,
            ]
        );
        assert_eq!(save_target(&edited, RelayTarget::Public), edited);
        let custom = save_target(
            &edited,
            RelayTarget::Custom("https://custom.example".into()),
        );
        let changed = save_target(
            &custom,
            RelayTarget::Custom("https://replacement.example".into()),
        );
        assert_eq!(changed.targets.len(), 3);
        assert_eq!(
            changed.targets[2],
            RelayTarget::Custom("https://replacement.example".into())
        );
        assert!(
            policy_patch(save_target(
                &policy,
                RelayTarget::Custom("http://bad.example".into())
            ))
            .apply(&Settings::default())
            .is_err()
        );
    }

    #[test]
    fn removing_custom_relay_preserves_remaining_disabled_priorities() {
        let policy = RelayPolicy {
            targets: vec![
                RelayTarget::Public,
                RelayTarget::Custom("https://custom.example".into()),
                RelayTarget::Project(PROJECT_RELAY_URL.into()),
            ],
            disabled: [1, 2].into_iter().collect(),
            ..RelayPolicy::default()
        };
        let removed = remove_custom_target(&policy);
        assert_eq!(
            removed.targets,
            vec![
                RelayTarget::Public,
                RelayTarget::Project(PROJECT_RELAY_URL.into())
            ]
        );
        assert_eq!(removed.disabled, [1].into_iter().collect());
        assert!(removed.is_valid());
        assert_eq!(remove_custom_target(&removed), removed);
        assert_eq!(policy.targets.len(), 3);
    }

    #[test]
    fn direct_only_and_enabling_fallbacks_respect_fixed_bind_validation() {
        let mut held = Settings::default();
        held.network.bind = Some("127.0.0.1".parse().unwrap());
        held.network.allow_relay = false;
        let enabled = policy_patch(RelayPolicy::default()).apply(&held).unwrap();
        assert_eq!(enabled.network.bind, None);
        assert_eq!(
            enabled.network.effective_relay_policy(),
            RelayPolicy::default()
        );
        let direct = policy_patch(RelayPolicy {
            targets: vec![],
            ..RelayPolicy::default()
        })
        .apply(&enabled)
        .unwrap();
        assert!(!direct.network.allow_relay);
        assert!(direct.network.effective_relay_policy().targets.is_empty());
    }
}
