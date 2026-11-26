//! Scoped desktop geometry and validated arrangements. No renderer or extension-specific policy.
use crate::aliases::Aliases;
use peersus_core::{
    layout::{self, Display, Placement},
    model::{Peer, PeerId},
    plugins::PluginId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const SERVICE: &str = "peersus.displays";
pub const ARRANGE: &str = "displays.arrange";
pub const DESCRIPTION: &str = "Change this computer's arrangement of paired desktops and where its pointer crosses between them.";
/// The permission that guards `inspect`: this computer's screens and the arrangement of paired desktops.
pub const WATCH: &str = "displays.watch";
pub const WATCH_DESCRIPTION: &str =
    "See this computer's screens and how your paired desktops are arranged around them.";
pub const LOCAL: &str = "this-device";

/// The desktops `inspect` and `arrange` read: this computer's screens, the paired devices, the screens each said it has and
/// where they are placed now.
pub(crate) struct Desktops<'a> {
    pub own: &'a [Display],
    pub peers: &'a [Peer],
    pub displays: &'a BTreeMap<PeerId, Vec<Display>>,
    pub placements: &'a [Placement],
}

impl Desktops<'_> {
    fn said(&self, peer: &PeerId) -> &[Display] {
        self.displays.get(peer).map_or(&[][..], Vec::as_slice)
    }
}

/// This computer's screens and the paired desktops `visible` lets the caller see (the person allowed it that device), each by
/// the caller's own alias. A device it may not see is neither listed nor placed: not its name, its screens or its place.
/// For an inactive relative placement, `relative_conflict` describes only conflicts visible in this arrangement.
pub(crate) fn inspect(
    aliases: &Aliases,
    caller: &PluginId,
    name: &str,
    desktops: &Desktops<'_>,
    visible: impl Fn(&Peer) -> bool,
) -> Value {
    let mut devices =
        vec![json!({"device":LOCAL,"name":name,"displays":desktops.own,"at":[0,0],"local":true})];
    let seen: Vec<&Peer> = desktops.peers.iter().filter(|peer| visible(peer)).collect();
    // Diagnose only the arrangement this caller may see. In particular, an unseen free
    // placement or an earlier unseen dock must not change a visible device's projection.
    let visible_placements = desktops.placements.iter().filter_map(|placement| {
        let peer = seen.iter().find(|peer| peer.id.0 == placement.device)?;
        Some((placement, desktops.said(&peer.id)))
    });
    let projections: BTreeMap<_, _> = if visible_placements
        .clone()
        .any(|(placement, _)| placement.relative_to_local.is_some())
    {
        let mut free = layout::place(LOCAL, desktops.own, (0, 0));
        for (placement, displays) in visible_placements
            .clone()
            .filter(|(placement, _)| placement.relative_to_local.is_none())
        {
            free.extend(layout::place(
                &placement.device,
                displays,
                (placement.x, placement.y),
            ));
        }
        let mut docks = layout::Docking::new(desktops.own, &free, &layout::crossings(&free), LOCAL);
        visible_placements
            .filter_map(|(placement, displays)| {
                let side = placement.relative_to_local?;
                let device = placement.device.as_str();
                Some((device, docks.project(device, displays, side)))
            })
            .collect()
    } else {
        BTreeMap::new()
    };
    devices.extend(seen.iter().map(|peer| {
        let device = aliases.alias(caller, &peer.id);
        let placement = desktops.placements.iter().find(|p| p.device == peer.id.0);
        let at = placement.map(|p| [p.x, p.y]);
        let displays = desktops.said(&peer.id);
        let mut row =
            json!({"device":device,"name":peer.name,"displays":displays,"at":at,"local":false});
        if let Some(side) = placement.and_then(|p| p.relative_to_local) {
            let projection = match projections.get(peer.id.0.as_str()) {
                Some(Ok(projected)) => projected.as_slice(),
                Some(Err(reason)) => {
                    row["relative_conflict"] = json!(match reason {
                        layout::DockError::SideInUse => "side-in-use",
                        layout::DockError::Overlap => "overlap",
                        layout::DockError::InvalidGeometry => "invalid-geometry",
                    });
                    &[]
                }
                None => &[],
            };
            let rects: Vec<_> = projection
                .iter()
                .map(|m| [m.x, m.y, i64::from(m.width), i64::from(m.height)])
                .collect();
            row["relative_to_local"] = json!(side);
            row["relative_displays"] = json!(rects);
        }
        row
    }));
    json!({"devices":devices,"placements":scoped(aliases,caller,&seen,desktops.placements)})
}

fn scoped(
    aliases: &Aliases,
    caller: &PluginId,
    seen: &[&Peer],
    placements: &[Placement],
) -> Vec<Placement> {
    placements
        .iter()
        .filter_map(|p| {
            let peer = seen.iter().find(|peer| peer.id.0 == p.device)?;
            Some(Placement {
                device: aliases.alias(caller, &peer.id),
                x: p.x,
                y: p.y,
                relative_to_local: p.relative_to_local,
            })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    expected: Vec<Placement>,
    placements: Vec<Placement>,
}

/// The new arrangement of the desktops the caller may see, checked against what it last read of them. The places of paired
/// devices it may not see are kept as they are (it cannot name, move or remove them) and checked with the rest for overlap.
pub(crate) fn arrange(
    args: &Value,
    aliases: &Aliases,
    caller: &PluginId,
    desktops: &Desktops<'_>,
    visible: impl Fn(&Peer) -> bool,
) -> Result<Vec<Placement>, &'static str> {
    let (peers, own, current) = (desktops.peers, desktops.own, desktops.placements);
    let (seen, hidden): (Vec<&Peer>, Vec<&Peer>) = peers.iter().partition(|peer| visible(peer));
    let change: Change =
        serde_json::from_value(args.clone()).map_err(|_| "Invalid desktop arrangement")?;
    if change.expected != scoped(aliases, caller, &seen, current) {
        return Err("Desktop arrangement changed elsewhere; reload before saving");
    }
    let kept: Vec<Placement> = current
        .iter()
        .filter(|p| hidden.iter().any(|peer| peer.id.0 == p.device))
        .cloned()
        .collect();
    if change.placements.len() + kept.len() > layout::MAX_PLACED {
        return Err("Too many desktops");
    }
    let mut taken = BTreeSet::new();
    let mut placed = Vec::with_capacity(change.placements.len() + kept.len());
    for placement in change.placements {
        let peer = aliases
            .resolve(caller, &placement.device, seen.iter().map(|peer| &peer.id))
            .ok_or("Unknown desktop")?;
        if !taken.insert(peer.clone())
            || placement.x.unsigned_abs() > layout::MAX_COORDINATE as u64
            || placement.y.unsigned_abs() > layout::MAX_COORDINATE as u64
        {
            return Err("Invalid desktop arrangement");
        }
        placed.push(Placement {
            device: peer.0.clone(),
            x: placement.x,
            y: placement.y,
            relative_to_local: placement.relative_to_local,
        });
    }
    placed.extend(kept);
    let fallback = [Display {
        id: "?".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    }];
    let said = |placement: &Placement| {
        peers
            .iter()
            .find(|peer| peer.id.0 == placement.device)
            .map_or(&[][..], |peer| desktops.said(&peer.id))
    };
    let mut free = layout::place(LOCAL, if own.is_empty() { &fallback } else { own }, (0, 0));
    for placement in placed.iter().filter(|p| p.relative_to_local.is_none()) {
        let displays = said(placement);
        free.extend(layout::place(
            &placement.device,
            if displays.is_empty() {
                &fallback
            } else {
                displays
            },
            (placement.x, placement.y),
        ));
    }
    if layout::validate_geometry(&free).is_err() {
        return Err("Desktop screens overlap or lie outside the usable plane");
    }

    let mut relative = placed
        .iter()
        .filter_map(|p| p.relative_to_local.map(|side| (p, side)))
        .peekable();
    if relative.peek().is_some() {
        let mut docks = layout::Docking::new(own, &free, &layout::crossings(&free), LOCAL);
        let mut claimed = HashSet::new();
        for (placement, side) in relative {
            if !claimed.insert(side) {
                return Err("Only one relative desktop can use the same side");
            }
            if docks
                .project(&placement.device, said(placement), side)
                .is_err()
            {
                // Free placements win. Keep an existing dock inactive instead of making
                // it impossible to edit or add free routes until that dock is removed.
                if current.iter().any(|old| {
                    old.device == placement.device && old.relative_to_local == Some(side)
                }) {
                    continue;
                }
                // Unlike inspect, validation includes hidden placements. Do not identify
                // what occupied the side or which geometry caused the refusal.
                return Err("A relative desktop cannot be attached there");
            }
        }
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(id: &str, name: &str) -> Peer {
        serde_json::from_value(
            json!({"id":id,"name":name,"paired_at":0,"last_seen":null,"online":true}),
        )
        .unwrap()
    }
    fn peer() -> Peer {
        named("paired-key", "Remote")
    }
    fn own() -> Vec<Display> {
        vec![Display {
            id: "screen".into(),
            x: -100,
            y: 0,
            width: 100,
            height: 100,
        }]
    }
    fn placement(device: String, x: i64) -> Placement {
        Placement {
            device,
            x,
            y: 0,
            relative_to_local: None,
        }
    }
    fn all(_: &Peer) -> bool {
        true
    }
    /// Arrange `args` over `peers` placed at `current`, every device visible.
    fn arranged(
        args: &Value,
        aliases: &Aliases,
        caller: &PluginId,
        peers: &[Peer],
        current: &[Placement],
    ) -> Result<Vec<Placement>, &'static str> {
        let own = own();
        let displays = BTreeMap::new();
        let desktops = Desktops {
            own: &own,
            peers,
            displays: &displays,
            placements: current,
        };
        arrange(args, aliases, caller, &desktops, all)
    }

    #[test]
    fn geometry_uses_only_the_callers_device_aliases() {
        let aliases = Aliases::new();
        let caller = PluginId::new("control").unwrap();
        let other = PluginId::new("other").unwrap();
        let peers = vec![peer()];
        let current = vec![placement(peers[0].id.0.clone(), 100)];
        let (own, displays) = (own(), BTreeMap::new());
        let desktops = Desktops {
            own: &own,
            peers: &peers,
            displays: &displays,
            placements: &current,
        };
        let view = inspect(&aliases, &caller, "Local", &desktops, all);
        let device = aliases.alias(&caller, &peers[0].id);
        assert_eq!(view["devices"][1]["device"], device);
        assert_eq!(view["placements"][0]["device"], device);
        assert_ne!(
            view["devices"][1]["device"],
            aliases.alias(&other, &peers[0].id)
        );
        assert!(!view.to_string().contains(&peers[0].id.0));
    }

    #[test]
    fn a_device_the_caller_may_not_see_is_neither_listed_nor_placed() {
        // `displays.watch` is one grant for the whole computer; that a paired device exists, and its name, is told only for a
        // device the person allowed this extension (`peers.watch` for it), as `peersus.peers` `list` does.
        let aliases = Aliases::new();
        let caller = PluginId::new("control").unwrap();
        let (seen, hidden) = (
            named("seen-key", "Desk"),
            named("hidden-key", "Secret laptop"),
        );
        let peers = vec![seen.clone(), hidden.clone()];
        let current = vec![
            placement(seen.id.0.clone(), 100),
            placement(hidden.id.0.clone(), -3000),
        ];
        let (own, displays) = (own(), BTreeMap::new());
        let desktops = Desktops {
            own: &own,
            peers: &peers,
            displays: &displays,
            placements: &current,
        };
        let view = inspect(&aliases, &caller, "Local", &desktops, |peer| {
            peer.id == seen.id
        });
        let text = view.to_string();
        assert!(!text.contains("Secret laptop"), "{text}");
        assert!(
            !text.contains(&aliases.alias(&caller, &hidden.id)),
            "{text}"
        );
        assert!(!text.contains("-3000"), "{text}");
        assert_eq!(view["devices"].as_array().unwrap().len(), 2);
        assert_eq!(view["devices"][1]["name"], "Desk");
        assert_eq!(view["placements"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn arranging_keeps_the_places_of_devices_the_caller_may_not_see_and_cannot_name_them() {
        let aliases = Aliases::new();
        let caller = PluginId::new("control").unwrap();
        let (seen, hidden) = (named("seen-key", "Desk"), named("hidden-key", "Laptop"));
        let peers = vec![seen.clone(), hidden.clone()];
        let current = vec![placement(hidden.id.0.clone(), -3000)];
        let (own, displays) = (own(), BTreeMap::new());
        let desktops = Desktops {
            own: &own,
            peers: &peers,
            displays: &displays,
            placements: &current,
        };
        let visible = |peer: &Peer| peer.id == seen.id;
        // What it read: no placement at all (the hidden one is not its to know).
        let moved = arrange(
            &json!({"expected":[],"placements":[placement(aliases.alias(&caller, &seen.id), 2000)]}),
            &aliases,
            &caller,
            &desktops,
            visible,
        )
        .unwrap();
        assert_eq!(
            moved,
            vec![
                placement(seen.id.0.clone(), 2000),
                placement(hidden.id.0.clone(), -3000)
            ]
        );
        // The hidden device's alias for this caller does not name it.
        assert_eq!(
            arrange(
                &json!({"expected":[],"placements":[placement(aliases.alias(&caller, &hidden.id), 2000)]}),
                &aliases,
                &caller,
                &desktops,
                visible,
            ),
            Err("Unknown desktop")
        );
    }

    #[test]
    fn foreign_aliases_and_raw_identifiers_cannot_move_a_desktop() {
        let aliases = Aliases::new();
        let caller = PluginId::new("control").unwrap();
        let other = PluginId::new("other").unwrap();
        let peers = vec![peer()];
        for device in [
            peers[0].id.0.clone(),
            aliases.alias(&other, &peers[0].id),
            LOCAL.into(),
        ] {
            assert!(
                arranged(
                    &json!({"expected":[],"placements":[placement(device,200)]}),
                    &aliases,
                    &caller,
                    &peers,
                    &[]
                )
                .is_err()
            );
        }
        let device = aliases.alias(&caller, &peers[0].id);
        assert_eq!(
            arranged(
                &json!({"expected":[],"placements":[placement(device,2000)]}),
                &aliases,
                &caller,
                &peers,
                &[]
            )
            .unwrap(),
            vec![placement(peers[0].id.0.clone(), 2000)]
        );
    }

    #[test]
    fn a_stale_snapshot_cannot_overwrite_a_newer_arrangement() {
        let aliases = Aliases::new();
        let caller = PluginId::new("control").unwrap();
        let peers = vec![peer()];
        let current = vec![placement(peers[0].id.0.clone(), 2000)];
        let args = json!({"expected":[],"placements":[]});
        assert!(arranged(&args, &aliases, &caller, &peers, &current).is_err());
        let expected = scoped(&aliases, &caller, &[&peers[0]], &current);
        assert_eq!(
            arranged(
                &json!({"expected":expected,"placements":[]}),
                &aliases,
                &caller,
                &peers,
                &current
            )
            .unwrap(),
            vec![]
        );
    }

    #[test]
    fn relative_side_round_trips_through_inspect_and_participates_in_compare_and_set() {
        use layout::Edge;
        let aliases = Aliases::new();
        let editor = PluginId::new("editor").unwrap();
        let paired = peer();
        let alias = aliases.alias(&editor, &paired.id);
        let local = [Display {
            width: 1920,
            height: 1080,
            ..own().remove(0)
        }];
        let displays = BTreeMap::from([(
            paired.id.clone(),
            vec![Display {
                width: 1080,
                height: 2400,
                ..own().remove(0)
            }],
        )]);
        let current = [Placement {
            device: paired.id.0.clone(),
            x: 17,
            y: -31,
            relative_to_local: Some(Edge::Right),
        }];
        let desktops = Desktops {
            own: &local,
            peers: std::slice::from_ref(&paired),
            displays: &displays,
            placements: &current,
        };
        let inspected = inspect(&aliases, &editor, "Local", &desktops, all);
        assert_eq!(inspected["devices"][1]["at"], json!([17, -31]));
        assert_eq!(inspected["devices"][1]["relative_to_local"], "right");
        assert_eq!(
            inspected["devices"][1]["relative_displays"],
            json!([[1920, 0, 486, 1080]])
        );
        assert_eq!(inspected["devices"][1]["displays"][0]["height"], 2400);
        let next = json!([{"device":alias, "x":17, "y":-31, "relative_to_local":"bottom"}]);
        assert_eq!(
            arrange(
                &json!({"expected":[{"device":alias, "x":17, "y":-31}], "placements":next}),
                &aliases,
                &editor,
                &desktops,
                all
            ),
            Err("Desktop arrangement changed elsewhere; reload before saving")
        );
        let saved = arrange(
            &json!({"expected":inspected["placements"],"placements":next}),
            &aliases,
            &editor,
            &desktops,
            all,
        )
        .unwrap();
        assert_eq!(saved[0].relative_to_local, Some(Edge::Bottom));
        assert_eq!((saved[0].x, saved[0].y), (17, -31));
        for (field, value) in [
            ("relative_to_local", json!("diagonal")),
            ("scale", json!(0.5)),
        ] {
            let mut bad = next.clone();
            bad[0][field] = value;
            assert_eq!(
                arrange(
                    &json!({"expected":inspected["placements"],"placements":bad}),
                    &aliases,
                    &editor,
                    &desktops,
                    all
                ),
                Err("Invalid desktop arrangement")
            );
        }
    }

    #[test]
    fn a_dormant_dock_cannot_claim_a_side_but_does_not_block_a_new_free_crossing() {
        let aliases = Aliases::new();
        let caller = PluginId::new("editor").unwrap();
        let peers = [named("dock", "Dock"), named("free", "Free")];
        let own = own();
        let displays = BTreeMap::from([(peers[1].id.clone(), own.clone())]);
        let free = placement(peers[1].id.0.clone(), 100);
        let dock = Placement {
            relative_to_local: Some(layout::Edge::Right),
            ..placement(peers[0].id.0.clone(), 0)
        };
        let one = Desktops {
            own: &own,
            peers: &peers,
            displays: &displays,
            placements: std::slice::from_ref(&free),
        };
        let alias = |p: &Placement, peer: &Peer| Placement {
            device: aliases.alias(&caller, &peer.id),
            ..p.clone()
        };
        let with_both = [alias(&dock, &peers[0]), alias(&free, &peers[1])];
        let attempt = |desktops| {
            let before = inspect(&aliases, &caller, "Local", desktops, all);
            arrange(
                &json!({"expected":before["placements"], "placements":with_both}),
                &aliases,
                &caller,
                desktops,
                all,
            )
        };
        assert_eq!(
            attempt(&one),
            Err("A relative desktop cannot be attached there")
        );
        let previously_docked = Desktops {
            placements: std::slice::from_ref(&dock),
            ..one
        };
        assert_eq!(attempt(&previously_docked), Ok(vec![dock, free]));
    }

    #[test]
    fn reports_only_relative_conflicts_visible_to_the_caller() {
        let aliases = Aliases::new();
        let caller = PluginId::new("editor").unwrap();
        let peers = [named("dock", "Dock"), named("free", "Free")];
        let own = own();
        let displays = BTreeMap::from([
            (peers[0].id.clone(), own.clone()),
            (peers[1].id.clone(), own.clone()),
        ]);
        let current = [
            Placement {
                relative_to_local: Some(layout::Edge::Right),
                ..placement(peers[0].id.0.clone(), 0)
            },
            placement(peers[1].id.0.clone(), 100),
        ];
        let desktops = Desktops {
            own: &own,
            peers: &peers,
            displays: &displays,
            placements: &current,
        };
        let view = inspect(&aliases, &caller, "Local", &desktops, all);
        assert_eq!(view["devices"][1]["relative_conflict"], "side-in-use");
        assert_eq!(view["devices"][1]["relative_displays"], json!([]));
        let scoped = inspect(&aliases, &caller, "Local", &desktops, |peer| {
            peer.id == peers[0].id
        });
        let without_hidden = Desktops {
            peers: &peers[..1],
            placements: &current[..1],
            ..desktops
        };
        assert_eq!(
            scoped,
            inspect(&aliases, &caller, "Local", &without_hidden, all)
        );
        assert_eq!(
            scoped["devices"][1]["relative_displays"],
            json!([[100, 0, 100, 100]])
        );
    }

    #[test]
    fn overlap_duplicate_destinations_and_out_of_plane_coordinates_are_refused() {
        let aliases = Aliases::new();
        let caller = PluginId::new("control").unwrap();
        let peers = vec![peer()];
        let device = aliases.alias(&caller, &peers[0].id);
        for placements in [
            vec![placement(device.clone(), 0)],
            vec![
                placement(device.clone(), 2000),
                placement(device.clone(), 4000),
            ],
            vec![placement(device, layout::MAX_COORDINATE + 1)],
        ] {
            assert!(
                arranged(
                    &json!({"expected":[],"placements":placements}),
                    &aliases,
                    &caller,
                    &peers,
                    &[]
                )
                .is_err()
            );
        }
    }
}
