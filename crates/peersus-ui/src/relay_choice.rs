//! Ordered relay fallbacks shared by desktop and Android. Direct paths always take priority.
//! Access tokens use a separate command and are never placed in the preference patch.
use freya::prelude::*;
use peersus_core::{
    Settings,
    config::SettingsPatch,
    model::RelayProbe,
    model::SecretText,
    relay::{PROJECT_RELAY_URL, RelayOrder, RelayPolicy, RelayTarget},
};

use crate::{i18n, link::Link, theme::Palette};
use peersus_core::model::Command;

fn policy_patch(policy: RelayPolicy) -> SettingsPatch {
    let enabled = policy.has_enabled_targets();
    SettingsPatch {
        relay_policy: Some(policy),
        ..crate::view::relay_patch(enabled)
    }
}

/// Move the entry at `from` to position `to`, the others keeping their order; its enabled state goes with it. Out-of-range positions
/// change nothing.
fn move_target(policy: &RelayPolicy, from: usize, to: usize) -> RelayPolicy {
    let mut next = policy.clone();
    if from >= next.targets.len() || to >= next.targets.len() || from == to {
        return next;
    }
    let moved = next.targets.remove(from);
    next.targets.insert(to, moved);
    let (low, high) = (from.min(to), from.max(to));
    next.disabled = policy
        .disabled
        .iter()
        .map(|&position| match position {
            p if p == from => to,
            p if p < low || p > high => p,
            p if from < to => p - 1,
            p => p + 1,
        })
        .collect();
    next
}

/// Change the address of a personal relay in place: it keeps its rank and its enabled state. The built-in relays (the project relay
/// and the public pool) have fixed addresses and are left as they are.
fn edit_target(policy: &RelayPolicy, index: usize, url: String) -> RelayPolicy {
    let mut next = policy.clone();
    if let Some(target @ RelayTarget::Custom(_)) = next.targets.get_mut(index) {
        *target = RelayTarget::Custom(url);
    }
    next
}

/// A new personal relay goes to the end of the list, enabled: one more fallback, after the ones already chosen.
fn add_custom(policy: &RelayPolicy, url: String) -> RelayPolicy {
    let mut next = policy.clone();
    next.targets.push(RelayTarget::Custom(url));
    next
}

/// Remove the entry at `index` if it is a personal relay; the project relay and the public pool are only ever disabled. Disabled
/// positions after it move up with their targets.
fn remove_target(policy: &RelayPolicy, index: usize) -> RelayPolicy {
    let mut next = policy.clone();
    if next.targets.get(index).is_some_and(RelayTarget::removable) {
        next.targets.remove(index);
        next.disabled = next
            .disabled
            .into_iter()
            .filter(|position| *position != index)
            .map(|position| position - usize::from(position > index))
            .collect();
    }
    next
}

fn order_words(order: RelayOrder) -> (&'static str, &'static str) {
    match order {
        RelayOrder::Automatic => (
            "Automatic",
            "All enabled relays are offered together; the nearest that answers is used.",
        ),
        RelayOrder::Priority => (
            "Priority",
            "The list order is the preference; the first relay is tried again after a fallback.",
        ),
        RelayOrder::Sequential => (
            "Sequential",
            "Down the list in order; a working relay is kept until it fails.",
        ),
        RelayOrder::Latency => (
            "Latency",
            "The enabled relays are measured and the fastest that answers is used.",
        ),
    }
}

fn kind_words(target: &RelayTarget) -> &'static str {
    match target {
        RelayTarget::Custom(_) => "My own relay",
        RelayTarget::Project(_) => "Project relay",
        RelayTarget::Public => "Public relays",
    }
}

/// The space between relay rows; dragging moves a row by its height plus this.
const ROW_GAP: f32 = 4.;

/// What the person types for a relay is its server (`relay.example.org`, optionally with `:port`); the scheme every relay is reached
/// over is added here, so it never has to be typed.
fn relay_address(typed: &str) -> String {
    let server = typed.trim().trim_end_matches('/');
    if server.contains("://") {
        server.to_owned()
    } else {
        format!("https://{server}")
    }
}

fn probe_words(probe: Option<&RelayProbe>) -> String {
    match probe {
        None => String::new(),
        Some(RelayProbe::Trying) => "…".to_owned(),
        Some(RelayProbe::Answered { ms }) => format!("{ms} ms"),
        Some(RelayProbe::Unreachable) => i18n::tr("No answer").to_owned(),
    }
}

/// Ordered fallbacks, address editing and the custom target's separate credential controls.
#[derive(PartialEq)]
pub struct Picker {
    pub link: Link,
    pub settings: Settings,
    pub diagnostics: Option<peersus_core::relay::RelayDiagnostics>,
    /// What the last test of each relay found, by its probe key.
    pub probes: std::collections::BTreeMap<String, RelayProbe>,
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
        let new_address = use_state(String::new);
        let edited_address = use_state(String::new);
        let token = use_state(String::new);
        let mut said: State<Option<String>> = use_state(|| None);
        let kept: State<Option<&'static str>> = use_state(|| None);
        // The relay whose removal waits for a second press.
        let mut removing: State<Option<usize>> = use_state(|| None);
        let mut selected: State<Option<usize>> = use_state(|| None);
        // Drag reordering: the relay held and how far it has moved, where the press began, and one row's height.
        let mut drag: State<Option<(usize, f32)>> = use_state(|| None);
        let mut drag_from = use_state(|| 0f32);
        let mut row_height = use_state(|| 56f32);
        let filter = use_state(String::new);
        let mut relay_failover = use_state(String::new);
        let direct_wait = use_state({
            let value = policy.direct_wait_ms.to_string();
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
            let active = diagnostics
                .active_target
                .and_then(|index| policy.targets.get(index))
                .map(|target| format!(" · {}", target.display_name()))
                .unwrap_or_default();
            block = block.child(
                label()
                    .text(format!("{}{active}", i18n::tr(state)))
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

        // How the enabled relays are chosen among: a selector of the four ways, with what the chosen one does said under it.
        let modes = RelayOrder::ALL.into_iter().map(|order| {
            let current = policy.clone();
            let link = self.link.clone();
            MenuItem::new()
                .selected(policy.order == order)
                .on_press(move |_| {
                    let next = RelayPolicy {
                        order,
                        ..current.clone()
                    };
                    apply_choice(said, link.clone(), policy_patch(next));
                })
                .child(i18n::tr(order_words(order).0))
                .into_element()
        });
        block = block
            .child(
                label()
                    .text(i18n::tr("Order"))
                    .color(palette.muted)
                    .font_size(13.),
            )
            .child(
                Select::new()
                    .selected_item(i18n::tr(order_words(policy.order).0))
                    .children(modes),
            )
            .child(
                label()
                    .text(i18n::tr(order_words(policy.order).1))
                    .color(palette.muted)
                    .font_size(12.),
            );

        // The relays are always in view, in their order, each with its controls on its own row. Editing a relay opens its settings
        // right under its row, so the list and its order stay visible while it is changed. A long list scrolls inside a bounded height
        // and can be filtered.
        let chosen = selected()
            .filter(|index| *index < policy.targets.len())
            .unwrap_or(0);
        let enabled_count = policy.targets.len() - policy.disabled.len().min(policy.targets.len());
        let summary = format!(
            "{} · {} / {}",
            i18n::tr("Relays"),
            enabled_count,
            policy.targets.len()
        );
        // How fast each relay answers is measured when the section is shown, so the list carries the numbers without a press.
        let probe_link = self.link.clone();
        use_hook(move || probe_link.send(Command::ProbeRelays { only: None }));
        let mut picker = rect()
            .width(Size::fill())
            .spacing(8.)
            .padding(10.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.5)
                    .alignment(BorderAlignment::Inner),
            )
            .child(
                label()
                    .text(summary)
                    .color(palette.text)
                    .font_weight(FontWeight::BOLD)
                    .max_lines(1),
            );
        // The chosen relay's own settings.
        let mut detail_panel: Option<Element> = None;
        if let Some(target) = selected().and_then(|index| policy.targets.get(index)) {
            let index = chosen;
            let token_target = policy.token_target();
            let probe = probe_words(self.probes.get(&target.probe_key()));
            let mut card = rect()
                .width(Size::fill())
                .spacing(10.)
                .padding(14.)
                .corner_radius(12.)
                .background(palette.raised);
            if let Some(url) = target.url() {
                card = card.child(
                    label()
                        .text(url.to_owned())
                        .color(palette.muted)
                        .font_size(12.),
                );
            }
            // Reachability of this relay alone.
            let link = self.link.clone();
            let probe_key = target.probe_key();
            card = card.child(
                rect()
                    .horizontal()
                    .spacing(8.)
                    .cross_align(Alignment::center())
                    .child(
                        Button::new()
                            .on_press(move |_| {
                                link.send(Command::ProbeRelays {
                                    only: Some(probe_key.clone()),
                                })
                            })
                            .child(i18n::tr("Ping")),
                    )
                    .child(
                        label()
                            .text(probe)
                            .color(palette.muted)
                            .font_size(13.)
                            .max_width(Size::Inner),
                    ),
            );
            // Its address: personal relays only; the built-in ones have fixed addresses.
            if target.removable() {
                let current = policy.clone();
                let link = self.link.clone();
                let mut edited = edited_address;
                card = card
                    .child(
                        crate::field::field(crate::field::Profile::ADDRESS, edited_address)
                            .placeholder(i18n::tr("relay.example.org"))
                            .width(Size::fill()),
                    )
                    .child(
                        Button::new()
                            .on_press(move |_| {
                                let url = relay_address(&edited.read());
                                if !peersus_core::config::valid_relay_url(&url) {
                                    said.set(Some(
                                        i18n::tr(
                                            "Enter the relay server, for example relay.example.org",
                                        )
                                        .to_owned(),
                                    ));
                                    return;
                                }
                                edited.set(String::new());
                                apply_choice(
                                    said,
                                    link.clone(),
                                    policy_patch(edit_target(&current, index, url)),
                                );
                            })
                            .child(i18n::tr("Change address")),
                    );
            }
            // Its access token.
            if token_target == Some(index) {
                let (save_link, forget_link) = (self.link.clone(), self.link.clone());
                card = card
                    .child(
                        crate::field::field(crate::field::Profile::SECRET, token)
                            .placeholder(i18n::tr("Access token (kept on this device only)"))
                            .width(Size::fill()),
                    )
                    .child(
                        rect()
                            .maybe(!self.compact, |row| row.horizontal())
                            .spacing(8.)
                            .child(
                                Button::new()
                                    .on_press(move |_| {
                                        keep_token(
                                            said,
                                            kept,
                                            token,
                                            save_link.clone(),
                                            token.read().clone(),
                                            "Token saved",
                                        )
                                    })
                                    .child(i18n::tr("Save token")),
                            )
                            .child(
                                Button::new()
                                    .on_press(move |_| {
                                        keep_token(
                                            said,
                                            kept,
                                            token,
                                            forget_link.clone(),
                                            String::new(),
                                            "Token forgotten",
                                        )
                                    })
                                    .child(i18n::tr("Forget token")),
                            ),
                    )
                    .child(
                        label()
                            .text(i18n::tr(
                                "The token is sent only to this relay, never to the others.",
                            ))
                            .color(palette.muted)
                            .font_size(12.),
                    );
            }
            // Its own timings.
            let own = policy.timings.contains_key(&target.probe_key());
            let timings = policy.timings_of(index);
            let key = target.probe_key();
            card = card
                .child(
                    label()
                        .text(i18n::tr(if own {
                            "This relay's own timings"
                        } else {
                            "Uses the list-wide timings"
                        }))
                        .color(palette.muted)
                        .font_size(13.),
                )
                .child(
                    rect()
                        .width(Size::fill())
                        .spacing(5.)
                        .child(
                            label()
                                .text(format!(
                                    "{} ({})",
                                    i18n::tr("Relay timeout (ms)"),
                                    timings.failover_wait_ms
                                ))
                                .color(palette.muted)
                                .font_size(12.),
                        )
                        .child(
                            crate::field::field(crate::field::Profile::TEXT, relay_failover)
                                .placeholder(timings.failover_wait_ms.to_string())
                                .width(Size::fill()),
                        ),
                );
            let (save_current, save_link, save_key) =
                (policy.clone(), self.link.clone(), key.clone());
            let (reset_current, reset_link, reset_key) = (policy.clone(), self.link.clone(), key);
            let mut timing_buttons = rect().horizontal().spacing(8.).child(
                Button::new()
                    .on_press(move |_| {
                        let failover = relay_failover
                            .read()
                            .trim()
                            .parse::<u32>()
                            .unwrap_or(timings.failover_wait_ms);
                        // Retrying the preferred relay is a list-wide choice, set in the advanced options.
                        let retry = save_current.retry_primary_ms;
                        let mut next = save_current.clone();
                        next.timings.insert(
                            save_key.clone(),
                            peersus_core::relay::RelayTimings {
                                failover_wait_ms: failover,
                                retry_primary_ms: retry,
                            },
                        );
                        if !next.is_valid() {
                            said.set(Some(i18n::tr("Enter valid relay timings").to_owned()));
                            return;
                        }
                        relay_failover.set(String::new());
                        apply_choice(said, save_link.clone(), policy_patch(next));
                    })
                    .child(i18n::tr("Save")),
            );
            if own {
                timing_buttons = timing_buttons.child(
                    Button::new()
                        .on_press(move |_| {
                            let mut next = reset_current.clone();
                            next.timings.remove(&reset_key);
                            apply_choice(said, reset_link.clone(), policy_patch(next));
                        })
                        .child(i18n::tr("Use list-wide timings")),
                );
            }
            card = card.child(timing_buttons);
            // Removing it: personal relays only, after a second press.
            if target.removable() {
                if removing() == Some(index) {
                    let current = policy.clone();
                    let link = self.link.clone();
                    card = card.child(
                        label()
                            .text(i18n::tr("Remove this relay?"))
                            .color(palette.bad)
                            .font_size(13.),
                    );
                    card = card.child(
                        rect()
                            .horizontal()
                            .spacing(8.)
                            .cross_align(Alignment::center())
                            .child(
                                Button::new()
                                    .on_press(move |_| {
                                        removing.set(None);
                                        selected.set(None);
                                        apply_choice(
                                            said,
                                            link.clone(),
                                            policy_patch(remove_target(&current, index)),
                                        );
                                    })
                                    .child(i18n::tr("Remove")),
                            )
                            .child(
                                Button::new()
                                    .on_press(move |_| removing.set(None))
                                    .child(i18n::tr("Cancel")),
                            ),
                    );
                } else {
                    card = card.child(
                        Button::new()
                            .on_press(move |_| removing.set(Some(index)))
                            .child(
                                label()
                                    .text(format!("✕  {}", i18n::tr("Remove")))
                                    .color(palette.bad)
                                    .max_width(Size::Inner),
                            ),
                    );
                }
            }
            detail_panel = Some(card.into_element());
        }
        let list_panel: Option<Element>;
        {
            let filter_text = filter.read().trim().to_lowercase();
            // While a relay is dragged by its handle: which one, and how far the finger has moved. The dragged row follows the finger
            // and the rows it passes slide out of its way, so the new order is seen before it is let go.
            let dragging = *drag.read();
            let step = (*row_height.read()).max(1.) + ROW_GAP;
            let drop_at = |from: usize, moved: f32| -> usize {
                let to = from as f32 + (moved / step).round();
                to.clamp(0., (policy.targets.len().saturating_sub(1)) as f32) as usize
            };
            let mut rows = rect().width(Size::fill()).spacing(ROW_GAP);
            // Adding is the first row of the list, shaped like every relay row: the address is typed right in it and saved on its right.
            {
                let current = policy.clone();
                let link = self.link.clone();
                let mut typed = new_address;
                let count = policy.targets.len();
                rows = rows.child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .content(Content::flex())
                        .cross_align(Alignment::center())
                        .spacing(8.)
                        .padding((8., 10.))
                        .corner_radius(8.)
                        .background(palette.surface)
                        .child(rect().padding((6., 4.)).child(label().text("+").color(palette.good).font_size(20.).font_weight(FontWeight::BOLD).max_width(Size::Inner)))
                        .child(
                            rect().width(Size::flex(1.)).child(
                                crate::field::field(crate::field::Profile::ADDRESS, new_address)
                                    .placeholder(i18n::tr("New relay: relay.example.org"))
                                    .width(Size::fill()),
                            ),
                        )
                        .child(
                            Button::new()
                                .layout_variant(ButtonLayoutVariant::Compact)
                                .enabled(!new_address.read().trim().is_empty())
                                .on_press(move |_| {
                                    let url = relay_address(&typed.read());
                                    if !peersus_core::config::valid_relay_url(&url) {
                                        said.set(Some(i18n::tr("Enter the relay server, for example relay.example.org").to_owned()));
                                        return;
                                    }
                                    typed.set(String::new());
                                    selected.set(Some(count));
                                    apply_choice(said, link.clone(), policy_patch(add_custom(&current, url)));
                                })
                                .child(label().text(i18n::tr("Save")).max_width(Size::Inner)),
                        ),
                );
            }
            let mut shown = 0usize;
            for (index, target) in policy.targets.iter().enumerate() {
                let name = target.display_name();
                if !filter_text.is_empty()
                    && !name.to_lowercase().contains(&filter_text)
                    && !target
                        .url()
                        .is_some_and(|url| url.to_lowercase().contains(&filter_text))
                {
                    continue;
                }
                shown += 1;
                let enabled_here = !policy.disabled.contains(&index);
                let probe = probe_words(self.probes.get(&target.probe_key()));
                let detail = [
                    i18n::tr(kind_words(target)).to_owned(),
                    probe,
                    if enabled_here {
                        String::new()
                    } else {
                        i18n::tr("off").to_owned()
                    },
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
                // Every action on a relay is a visible control on its row: its switch, and an edit button that opens its settings.
                let current = policy.clone();
                let link = self.link.clone();
                let editing = selected() == Some(index);
                let address = target.url().map(str::to_owned).unwrap_or_default();
                let mut edited = edited_address;
                // Where this row is drawn while another (or this one) is dragged.
                let offset = match dragging {
                    Some((from, moved)) if from == index => moved,
                    Some((from, moved)) => {
                        let to = drop_at(from, moved);
                        if from < index && index <= to {
                            -step
                        } else if to <= index && index < from {
                            step
                        } else {
                            0.
                        }
                    }
                    None => 0.,
                };
                let lifted = matches!(dragging, Some((from, _)) if from == index);
                let row = rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .cross_align(Alignment::center())
                    .spacing(8.)
                    .padding((8., 10.))
                    .corner_radius(8.)
                    .maybe(lifted, |row| {
                        row.shadow(Shadow::new().blur(12.).color(Color::from_argb(90, 0, 0, 0)))
                    })
                    .background(if editing || lifted {
                        palette.raised
                    } else {
                        palette.surface
                    })
                    .maybe(index == 0, |row| {
                        row.on_sized(move |e: Event<SizedEventData>| {
                            row_height.set_if_modified(e.area.height())
                        })
                    })
                    // The handle: hold and drag it to move the relay.
                    .child(
                        rect()
                            .padding((6., 4.))
                            .on_pointer_down(move |e: Event<PointerEventData>| {
                                // The handle owns this gesture: the page must not scroll while a relay is being moved.
                                e.stop_propagation();
                                drag_from.set(e.global_location().y as f32);
                                drag.set(Some((index, 0.)));
                            })
                            .child(
                                label()
                                    .text("≡")
                                    .color(palette.muted)
                                    .font_size(20.)
                                    .max_width(Size::Inner),
                            ),
                    )
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .child(
                                label()
                                    .text(format!("{}. {}", index + 1, name))
                                    .color(if enabled_here {
                                        palette.text
                                    } else {
                                        palette.muted
                                    })
                                    .font_weight(FontWeight::BOLD)
                                    .max_lines(1),
                            )
                            .child(
                                label()
                                    .text(detail)
                                    .color(palette.muted)
                                    .font_size(12.)
                                    .max_lines(1),
                            ),
                    )
                    .child(Switch::new(WritableSetter::new(
                        enabled_here,
                        move |enabled: bool| {
                            let mut next = current.clone();
                            if enabled {
                                next.disabled.remove(&index);
                            } else {
                                next.disabled.insert(index);
                            }
                            apply_choice(said, link.clone(), policy_patch(next));
                        },
                    )))
                    .child(
                        Button::new()
                            .layout_variant(ButtonLayoutVariant::Compact)
                            .on_press(move |_| {
                                selected.set(if editing { None } else { Some(index) });
                                removing.set(None);
                                // The address is edited in place, starting from what it is now.
                                edited.set(address.clone());
                            })
                            .child(
                                label()
                                    .text(i18n::tr(if editing { "Close" } else { "Edit" }))
                                    .max_width(Size::Inner),
                            ),
                    );
                // The offset shifts an element's children, so the row is the child of a plain holder that is offset: the whole row
                // moves, its background and its contents together. The held row is drawn above the others.
                rows = rows.child(
                    rect()
                        .width(Size::fill())
                        .offset_y(offset)
                        .maybe(lifted, |holder| holder.layer(Layer::Overlay))
                        .child(row),
                );
                if editing && let Some(detail) = detail_panel.take() {
                    rows = rows.child(detail);
                }
            }
            if shown == 0 {
                rows = rows.child(
                    label()
                        .text(i18n::tr("No relay"))
                        .color(palette.muted)
                        .font_size(13.),
                );
            }
            // Dragging continues wherever the finger goes and ends where it is let go.
            let finish = policy.clone();
            let finish_link = self.link.clone();
            let count = policy.targets.len();
            let rows = rows
                // Capture phase: the page's scroll area handles moves and lifts in this phase too, and must not take them first.
                .on_capture_global_pointer_move(move |e: Event<PointerEventData>| {
                    let held = *drag.peek();
                    if let Some((from, _)) = held {
                        // The row moves only within the list: from the first place to the last, never outside its area.
                        let step = (*row_height.peek()).max(1.) + ROW_GAP;
                        let last = count.saturating_sub(1) as f32;
                        let moved = (e.global_location().y as f32 - *drag_from.peek())
                            .clamp(-(from as f32) * step, (last - from as f32) * step);
                        drag.set(Some((from, moved)));
                    }
                })
                .on_capture_global_pointer_up(move |_| {
                    if let Some((from, moved)) = drag.write().take() {
                        let step = (*row_height.peek()).max(1.) + ROW_GAP;
                        let to = (from as f32 + (moved / step).round())
                            .clamp(0., (finish.targets.len().saturating_sub(1)) as f32)
                            as usize;
                        if to != from {
                            if *selected.peek() == Some(from) {
                                selected.set(Some(to));
                            }
                            apply_choice(
                                said,
                                finish_link.clone(),
                                policy_patch(move_target(&finish, from, to)),
                            );
                        }
                    }
                });
            list_panel = Some(
                rect()
                    .width(Size::fill())
                    .spacing(8.)
                    .child(
                        crate::field::field(crate::field::Profile::SEARCH, filter)
                            .placeholder(i18n::tr("Search relays"))
                            .width(Size::fill()),
                    )
                    .child(if shown > 8 {
                        ScrollView::new()
                            .width(Size::fill())
                            .height(Size::px(420.))
                            .child(rows)
                            .into_element()
                    } else {
                        rows.into_element()
                    })
                    .into_element(),
            );
        }

        if let Some(list) = list_panel {
            picker = picker.child(list);
        }
        block = block.child(picker);
        if !policy.has_enabled_targets() && self.diagnostics.is_none() {
            block = block.child(
                label()
                    .text(i18n::tr("No relay"))
                    .color(palette.muted)
                    .font_size(13.),
            );
        }
        block = block.child(label()
            .text(i18n::tr("Application traffic waits for a direct path before using relay fallbacks in this order. Relay signalling may begin earlier. Fallback traffic is end-to-end encrypted, but connection metadata is visible. No application telemetry is sent."))
            .color(palette.warning).font_size(12.));
        // The list-wide waits are always shown under a heading, never behind a button.
        block = block.child(
            label()
                .text(i18n::tr("Advanced relay options"))
                .color(palette.text)
                .font_weight(FontWeight::BOLD),
        );
        {
            // Only the waits that concern the whole list; each relay's own timeout is in its settings.
            for (caption, state, explanation) in [
                (
                    "Direct wait (ms)",
                    direct_wait,
                    "How long a direct connection is given before data may go through a relay.",
                ),
                (
                    "Retry preferred relay (ms)",
                    retry_primary,
                    "In Priority order, how long a fallback is used before the first relay is tried again.",
                ),
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
                        .child(
                            crate::field::field(crate::field::Profile::TEXT, state)
                                .width(Size::fill()),
                        )
                        .child(
                            label()
                                .text(i18n::tr(explanation))
                                .color(palette.muted)
                                .font_size(12.),
                        ),
                );
            }
            let current = policy.clone();
            let link = self.link.clone();
            block = block.child(
                Button::new()
                    .on_press(move |_| {
                        let values = (
                            direct_wait.read().parse::<u32>(),
                            retry_primary.read().parse::<u32>(),
                        );
                        let (Ok(direct), Ok(retry)) = values else {
                            said.set(Some(i18n::tr("Enter valid relay timings").to_owned()));
                            return;
                        };
                        let next = RelayPolicy {
                            direct_wait_ms: direct,
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
                RelayTarget::Custom("https://custom.example".into()),
                RelayTarget::Project(PROJECT_RELAY_URL.into()),
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
    fn moving_a_relay_carries_its_switch_and_shifts_the_ones_between() {
        let target = |n: usize| RelayTarget::Custom(format!("https://r{n}.example"));
        let policy = RelayPolicy {
            targets: (0..4).map(target).collect(),
            disabled: [0, 2].into_iter().collect(),
            ..RelayPolicy::default()
        };
        let down = move_target(&policy, 0, 2);
        assert_eq!(
            down.targets,
            vec![target(1), target(2), target(0), target(3)]
        );
        assert_eq!(down.disabled, [1, 2].into_iter().collect());
        let up = move_target(&policy, 3, 0);
        assert_eq!(up.targets, vec![target(3), target(0), target(1), target(2)]);
        assert_eq!(up.disabled, [1, 3].into_iter().collect());
        for (from, to) in [(0, 3), (2, 1), (1, 1)] {
            let moved = move_target(&policy, from, to);
            for (index, relay) in policy.targets.iter().enumerate() {
                let at = moved
                    .targets
                    .iter()
                    .position(|other| other == relay)
                    .unwrap();
                assert_eq!(
                    policy.disabled.contains(&index),
                    moved.disabled.contains(&at),
                    "{from}->{to} {index}"
                );
            }
        }
    }

    #[test]
    fn only_personal_addresses_change_in_place_and_a_new_relay_goes_last() {
        let policy = RelayPolicy::default();
        // The project relay and the public pool keep their fixed addresses.
        assert_eq!(
            edit_target(&policy, 0, "https://other.example".into()),
            policy
        );
        assert_eq!(
            edit_target(&policy, 1, "https://other.example".into()),
            policy
        );
        let one = add_custom(&policy, "https://one.example".into());
        let two = add_custom(&one, "https://two.example".into());
        assert_eq!(two.targets.len(), 4);
        assert_eq!(
            two.targets[3],
            RelayTarget::Custom("https://two.example".into())
        );
        let edited = edit_target(&two, 2, "https://moved.example".into());
        assert_eq!(
            edited.targets[2],
            RelayTarget::Custom("https://moved.example".into())
        );
        assert_eq!(edited.targets[..2], policy.targets[..]);
        assert!(edited.is_valid());
        assert!(
            policy_patch(add_custom(&policy, "http://bad.example".into()))
                .apply(&Settings::default())
                .is_err()
        );
    }

    #[test]
    fn only_personal_relays_are_removed_and_disabled_ranks_follow_their_targets() {
        let policy = RelayPolicy {
            targets: vec![
                RelayTarget::Public,
                RelayTarget::Custom("https://custom.example".into()),
                RelayTarget::Project(PROJECT_RELAY_URL.into()),
            ],
            disabled: [1, 2].into_iter().collect(),
            ..RelayPolicy::default()
        };
        let removed = remove_target(&policy, 1);
        assert_eq!(
            removed.targets,
            vec![
                RelayTarget::Public,
                RelayTarget::Project(PROJECT_RELAY_URL.into())
            ]
        );
        assert_eq!(removed.disabled, [1].into_iter().collect());
        assert!(removed.is_valid());
        // The project relay and the public pool can be disabled, never removed.
        assert_eq!(remove_target(&removed, 0), removed);
        assert_eq!(remove_target(&removed, 1), removed);
        assert_eq!(remove_target(&removed, 9), removed);
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
