//! Scoped desktop geometry and validated arrangements. No renderer or extension-specific policy.
use crate::aliases::Aliases;
use peersus_core::{
    layout::{self, Display, Placement},
    model::{Peer, PeerId},
    plugins::PluginId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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

/// This computer's screens and the paired desktops `visible` lets the caller see (the person allowed it that device), each by
/// the caller's own alias. A device it may not see is neither listed nor placed: not its name, its screens or its place.
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
    devices.extend(seen.iter().map(|peer| {
        let at = desktops.placements.iter().find(|p| p.device == peer.id.0).map(|p| [p.x,p.y]);
        json!({"device":aliases.alias(caller,&peer.id),"name":peer.name,"displays":desktops.displays.get(&peer.id).map(Vec::as_slice).unwrap_or(&[]),"at":at,"local":false})
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
    let (peers, own, displays, current) = (
        desktops.peers,
        desktops.own,
        desktops.displays,
        desktops.placements,
    );
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
        });
    }
    placed.extend(kept);
    let fallback = |source: &[Display]| {
        if source.is_empty() {
            vec![Display {
                id: "?".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }]
        } else {
            source.to_vec()
        }
    };
    let mut monitors = layout::place(LOCAL, &fallback(own), (0, 0));
    for placement in &placed {
        let said = peers
            .iter()
            .find(|p| p.id.0 == placement.device)
            .and_then(|p| displays.get(&p.id))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        monitors.extend(layout::place(
            &placement.device,
            &fallback(said),
            (placement.x, placement.y),
        ));
    }
    if layout::validate_from(&monitors, LOCAL).is_err_and(|error| error.blocks_saving()) {
        return Err("Desktop screens overlap or lie outside the usable plane");
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
        Placement { device, x, y: 0 }
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
