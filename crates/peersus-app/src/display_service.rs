//! Scoped desktop geometry and validated arrangements. No renderer or extension-specific policy.
use crate::aliases::Aliases;
use peersus_core::{layout::{self, Display, Placement}, model::{Peer, PeerId}, plugins::PluginId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SERVICE: &str = "peersus.displays";
pub const ARRANGE: &str = "displays.arrange";
pub const DESCRIPTION: &str = "Change this computer's arrangement of paired desktops and where its pointer crosses between them.";
pub const LOCAL: &str = "this-device";

pub(crate) fn inspect(aliases: &Aliases, caller: &PluginId, name: &str, own: &[Display], peers: &[Peer], displays: &BTreeMap<PeerId, Vec<Display>>, placements: &[Placement]) -> Value {
    let mut devices = vec![json!({"device":LOCAL,"name":name,"displays":own,"at":[0,0],"local":true})];
    devices.extend(peers.iter().map(|peer| {
        let at = placements.iter().find(|p| p.device == peer.id.0).map(|p| [p.x,p.y]);
        json!({"device":aliases.alias(caller,&peer.id),"name":peer.name,"displays":displays.get(&peer.id).map(Vec::as_slice).unwrap_or(&[]),"at":at,"local":false})
    }));
    json!({"devices":devices,"placements":scoped(aliases,caller,peers,placements)})
}

fn scoped(aliases: &Aliases, caller: &PluginId, peers: &[Peer], placements: &[Placement]) -> Vec<Placement> {
    placements.iter().filter_map(|p| {
        let peer = peers.iter().find(|peer| peer.id.0 == p.device)?;
        Some(Placement { device: aliases.alias(caller,&peer.id), x:p.x, y:p.y })
    }).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change { expected: Vec<Placement>, placements: Vec<Placement> }

pub(crate) fn arrange(args: &Value, aliases: &Aliases, caller: &PluginId, peers: &[Peer], own: &[Display], displays: &BTreeMap<PeerId, Vec<Display>>, current: &[Placement]) -> Result<Vec<Placement>, &'static str> {
    let change: Change = serde_json::from_value(args.clone()).map_err(|_| "Invalid desktop arrangement")?;
    if change.expected != scoped(aliases,caller,peers,current) { return Err("Desktop arrangement changed elsewhere; reload before saving"); }
    if change.placements.len() > layout::MAX_PLACED { return Err("Too many desktops"); }
    let mut seen = BTreeSet::new();
    let mut placed = Vec::with_capacity(change.placements.len());
    for placement in change.placements {
        let peer = aliases.resolve(caller,&placement.device,peers.iter().map(|peer| &peer.id)).ok_or("Unknown desktop")?;
        if !seen.insert(peer.clone()) || placement.x.unsigned_abs() > layout::MAX_COORDINATE as u64 || placement.y.unsigned_abs() > layout::MAX_COORDINATE as u64 { return Err("Invalid desktop arrangement"); }
        placed.push(Placement { device:peer.0.clone(), x:placement.x, y:placement.y });
    }
    let fallback = |source: &[Display]| if source.is_empty() { vec![Display { id:"?".into(), x:0,y:0,width:1920,height:1080 }] } else { source.to_vec() };
    let mut monitors = layout::place(LOCAL,&fallback(own),(0,0));
    for placement in &placed {
        let said = peers.iter().find(|p| p.id.0 == placement.device).and_then(|p| displays.get(&p.id)).map(Vec::as_slice).unwrap_or(&[]);
        monitors.extend(layout::place(&placement.device,&fallback(said),(placement.x,placement.y)));
    }
    if layout::validate_from(&monitors,LOCAL).is_err_and(|error| error.blocks_saving()) { return Err("Desktop screens overlap or lie outside the usable plane"); }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> Peer {
        serde_json::from_value(json!({"id":"paired-key","name":"Remote","paired_at":0,"last_seen":null,"online":true})).unwrap()
    }
    fn own() -> Vec<Display> {
        vec![Display{id:"screen".into(),x:-100,y:0,width:100,height:100}]
    }
    fn placement(device: String, x: i64) -> Placement { Placement{device,x,y:0} }

    #[test]
    fn geometry_uses_only_the_callers_device_aliases() {
        let aliases=Aliases::new();let caller=PluginId::new("control").unwrap();let other=PluginId::new("other").unwrap();
        let peers=vec![peer()];let current=vec![placement(peers[0].id.0.clone(),100)];
        let view=inspect(&aliases,&caller,"Local",&own(),&peers,&BTreeMap::new(),&current);
        let device=aliases.alias(&caller,&peers[0].id);
        assert_eq!(view["devices"][1]["device"],device);
        assert_eq!(view["placements"][0]["device"],device);
        assert_ne!(view["devices"][1]["device"],aliases.alias(&other,&peers[0].id));
        assert!(!view.to_string().contains(&peers[0].id.0));
    }

    #[test]
    fn foreign_aliases_and_raw_identifiers_cannot_move_a_desktop() {
        let aliases=Aliases::new();let caller=PluginId::new("control").unwrap();let other=PluginId::new("other").unwrap();
        let peers=vec![peer()];
        for device in [peers[0].id.0.clone(),aliases.alias(&other,&peers[0].id),LOCAL.into()] {
            assert!(arrange(&json!({"expected":[],"placements":[placement(device,200)]}),&aliases,&caller,&peers,&own(),&BTreeMap::new(),&[]).is_err());
        }
        let device=aliases.alias(&caller,&peers[0].id);
        assert_eq!(arrange(&json!({"expected":[],"placements":[placement(device,2000)]}),&aliases,&caller,&peers,&own(),&BTreeMap::new(),&[]).unwrap(),vec![placement(peers[0].id.0.clone(),2000)]);
    }

    #[test]
    fn a_stale_snapshot_cannot_overwrite_a_newer_arrangement() {
        let aliases=Aliases::new();let caller=PluginId::new("control").unwrap();let peers=vec![peer()];
        let current=vec![placement(peers[0].id.0.clone(),2000)];
        let args=json!({"expected":[],"placements":[]});
        assert!(arrange(&args,&aliases,&caller,&peers,&own(),&BTreeMap::new(),&current).is_err());
        let expected=scoped(&aliases,&caller,&peers,&current);
        assert_eq!(arrange(&json!({"expected":expected,"placements":[]}),&aliases,&caller,&peers,&own(),&BTreeMap::new(),&current).unwrap(),vec![]);
    }

    #[test]
    fn overlap_duplicate_destinations_and_out_of_plane_coordinates_are_refused() {
        let aliases=Aliases::new();let caller=PluginId::new("control").unwrap();let peers=vec![peer()];
        let device=aliases.alias(&caller,&peers[0].id);
        for placements in [
            vec![placement(device.clone(),0)],
            vec![placement(device.clone(),2000),placement(device.clone(),4000)],
            vec![placement(device,layout::MAX_COORDINATE+1)],
        ] {
            assert!(arrange(&json!({"expected":[],"placements":placements}),&aliases,&caller,&peers,&own(),&BTreeMap::new(),&[]).is_err());
        }
    }
}
