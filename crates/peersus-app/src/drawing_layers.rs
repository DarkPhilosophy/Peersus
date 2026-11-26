//! What extensions keep for their drawings: layers of shapes that a drawing places by id, at a scale and a position.
//!
//! What is drawn again and again (the tiles of a map, the grid of a chart) is sent once, kept here, and placed by name: the extension
//! answers a page with a few words about where each layer goes instead of with its outlines, and a window parses each layer once and
//! draws it as often as it is placed. The layers are the caller's own: one extension never places, reads or forgets another's. What an
//! extension keeps goes with it when it stops, restarts, is turned off or is removed.
//!
//! WHAT A WINDOW IS TOLD. A snapshot names each extension's set by a stamp and nothing more. A window that does not hold that stamp asks
//! for the index (every id with the stamp of its content) and then for the layers it lacks, so each layer reaches a window once for each
//! change, and a window that opens later asks for all of them. The stamps are this owner's own numbers, never the same twice.
//!
//! WHAT BOUNDS IT. An extension keeps at most [`MAX_KEPT_LAYERS`] layers and [`MAX_KEPT_PATH`] characters of outline in all; a `keep`
//! past either is refused and changes nothing.
use peersus_api::elements::{Forget, Keep, MAX_KEPT_LAYERS, MAX_KEPT_PATH, Shape};
use peersus_core::{
    model::{KeptLayers, LayerContent, LayerIndex},
    plugins::PluginId,
};
use rand::Rng;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const SERVICE: &str = "peersus.drawings";
/// Keep one layer, or replace the one kept under that id.
pub(crate) const KEEP: &str = "keep";
/// Let go of some layers.
pub(crate) const FORGET: &str = "forget";

/// The most one answer to a window weighs, as the layers' JSON in bytes ([`weight`] never says less than it is). One layer is carried
/// whole whatever it holds (at most [`peersus_api::elements::MAX_LAYER_PATH`] of outline in [`peersus_api::elements::MAX_LAYER_SHAPES`]
/// shapes), and the window asks again for the rest. A frame to a window holds 4 MiB, and may carry a snapshot as well.
pub(crate) const REPLY_BYTES: usize = 1024 * 1024;
/// What a shape's JSON weighs besides its outline, at the most: every word it may have, with its separator.
const SHAPE_JSON: usize = 100;
/// What a layer's JSON weighs besides its id and its shapes, at the most.
const LAYER_JSON: usize = 128;

/// What `layer` kept under `id` weighs in an answer to a window, serialized: never less than it does.
fn weight(id: &str, layer: &Layer) -> usize {
    LAYER_JSON + id.len() + layer.path + SHAPE_JSON * layer.shapes.len()
}

struct Layer {
    shapes: Vec<Shape>,
    /// Characters of outline in `shapes`.
    path: usize,
    /// Which content this is: changes whenever the id is kept again with other shapes.
    stamp: u64,
}

#[derive(Default)]
struct Set {
    layers: BTreeMap<String, Layer>,
    /// Characters of outline in all the layers.
    path: usize,
    /// Changes whenever any layer of the set does.
    stamp: u64,
}

/// The layers of every extension, each in a set of its own.
pub(crate) struct DrawingLayers {
    /// Only extensions that keep something: a set that was emptied goes.
    sets: BTreeMap<PluginId, Set>,
    /// The last stamp given.
    last: u64,
}

impl Default for DrawingLayers {
    fn default() -> Self {
        // Another run of the application starts elsewhere, so that a stamp a window kept of an owner that ended is not one of this
        // owner's by chance.
        Self {
            sets: BTreeMap::new(),
            last: rand::rng().next_u64(),
        }
    }
}

impl DrawingLayers {
    /// A stamp this owner has not given before. Never zero: zero says that nothing is kept.
    fn stamp(&mut self) -> u64 {
        self.last = self.last.wrapping_add(1);
        if self.last == 0 {
            self.last = 1;
        }
        self.last
    }

    /// Keep one layer for `caller`, or replace the one under that id. Says whether anything changed: keeping what is kept already
    /// changes nothing and costs no window anything. A refusal keeps the previous set.
    pub(crate) fn keep(&mut self, caller: &PluginId, args: &Value) -> Result<bool, &'static str> {
        const REFUSED: &str = "That layer was not understood";
        const FULL: &str = "This extension keeps as many layers as it may";
        let keep = Keep::parse(args).map_err(|_| REFUSED)?;
        let path = keep.path_len();
        let set = self.sets.get(caller);
        let old = set.and_then(|set| set.layers.get(&keep.id));
        if old.is_some_and(|old| old.shapes == keep.shapes) {
            return Ok(false);
        }
        let layers = set.map_or(0, |set| set.layers.len()) + usize::from(old.is_none());
        let total = set
            .map_or(0, |set| set.path)
            .saturating_sub(old.map_or(0, |old| old.path))
            + path;
        if layers > MAX_KEPT_LAYERS || total > MAX_KEPT_PATH {
            return Err(FULL);
        }
        let stamp = self.stamp();
        let set = self.sets.entry(caller.clone()).or_default();
        set.layers.insert(
            keep.id,
            Layer {
                shapes: keep.shapes,
                path,
                stamp,
            },
        );
        set.path = total;
        set.stamp = stamp;
        Ok(true)
    }

    /// Let go of the layers `args` names that `caller` keeps; the others are ignored. Says whether anything changed. A refusal keeps
    /// the previous set.
    pub(crate) fn forget(&mut self, caller: &PluginId, args: &Value) -> Result<bool, &'static str> {
        const REFUSED: &str = "Those layers were not understood";
        let forget = Forget::parse(args).map_err(|_| REFUSED)?;
        let Some(set) = self.sets.get_mut(caller) else {
            return Ok(false);
        };
        let mut changed = false;
        for id in &forget.ids {
            if let Some(layer) = set.layers.remove(id) {
                set.path = set.path.saturating_sub(layer.path);
                changed = true;
            }
        }
        if !changed {
            return Ok(false);
        }
        if set.layers.is_empty() {
            self.sets.remove(caller);
        } else {
            let stamp = self.stamp();
            if let Some(set) = self.sets.get_mut(caller) {
                set.stamp = stamp;
            }
        }
        Ok(true)
    }

    /// Everything of `caller` goes: it stopped, restarted, was turned off or was removed. Says whether it kept anything.
    pub(crate) fn forget_plugin(&mut self, caller: &PluginId) -> bool {
        self.sets.remove(caller).is_some()
    }

    /// The stamp of each extension's set, for the snapshot: only extensions that keep something, in the order of their names.
    pub(crate) fn stamps(&self) -> Vec<KeptLayers> {
        self.sets
            .iter()
            .map(|(plugin, set)| KeptLayers {
                plugin: plugin.clone(),
                stamp: set.stamp,
            })
            .collect()
    }

    /// What `plugin` keeps, by id, with the stamp of each layer's content, and the stamp of the whole (zero: nothing is kept).
    pub(crate) fn index(&self, plugin: &PluginId) -> LayerIndex {
        self.sets
            .get(plugin)
            .map_or_else(LayerIndex::default, |set| LayerIndex {
                stamp: set.stamp,
                layers: set
                    .layers
                    .iter()
                    .map(|(id, layer)| (id.clone(), layer.stamp))
                    .collect(),
            })
    }

    /// The content of the layers of `plugin` named by `ids`, in that order: as many as fit one answer ([`REPLY_BYTES`], and at least
    /// one), and not an id that is not kept. The window asks again for what it still lacks.
    pub(crate) fn fetch(&self, plugin: &PluginId, ids: &[String]) -> Vec<LayerContent> {
        let Some(set) = self.sets.get(plugin) else {
            return Vec::new();
        };
        let mut carried = 0;
        let mut answer = Vec::new();
        for id in ids.iter().take(MAX_KEPT_LAYERS) {
            let Some(layer) = set.layers.get(id) else {
                continue;
            };
            let weighs = weight(id, layer);
            if !answer.is_empty() && carried + weighs > REPLY_BYTES {
                break;
            }
            carried += weighs;
            answer.push(LayerContent {
                id: id.clone(),
                stamp: layer.stamp,
                shapes: layer.shapes.clone(),
            });
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::elements::MAX_LAYER_PATH;
    use serde_json::json;

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }

    /// An outline of exactly `len` characters, made of text an outline may hold.
    fn outline(len: usize) -> String {
        let mut outline = String::from("M0 0");
        while outline.len() + 5 <= len {
            outline.push_str(" L1 1");
        }
        while outline.len() < len {
            outline.push(' ');
        }
        outline
    }

    /// A `keep` of layer `id` whose one shape is an outline of `path` characters.
    fn keep(id: &str, path: usize) -> Value {
        json!({"id": id, "shapes": [{"path": outline(path), "stroke": "accent", "line": 10}]})
    }

    /// A `keep` of layer `id` of `count` shapes of `path` characters each, every optional word of every shape written.
    fn many_shapes(id: &str, count: usize, path: usize) -> Value {
        let shape = json!({"path": outline(path), "stroke": "transparent", "fill": "transparent",
            "line": 1000, "opacity": 10000, "round": true});
        json!({"id": id, "shapes": vec![shape; count]})
    }

    fn ids(layers: &DrawingLayers, caller: &PluginId) -> Vec<String> {
        layers
            .index(caller)
            .layers
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    #[test]
    fn a_layer_is_kept_replaced_and_told_by_the_stamp_of_what_it_holds() {
        let map = plugin("map");
        let mut layers = DrawingLayers::default();
        assert!(layers.stamps().is_empty());
        assert_eq!(layers.index(&map), LayerIndex::default());

        assert_eq!(layers.keep(&map, &keep("tile/1", 20)), Ok(true));
        let first = layers.index(&map);
        assert_ne!(first.stamp, 0);
        assert_eq!(layers.stamps().len(), 1);
        assert_eq!(layers.stamps()[0].stamp, first.stamp);
        assert_eq!(first.layers.len(), 1);

        // The same layer again is already kept: nothing changes, so no window is asked to look.
        assert_eq!(layers.keep(&map, &keep("tile/1", 20)), Ok(false));
        assert_eq!(layers.index(&map), first);

        // Other shapes under the same id replace it, and its stamp and the set's change.
        assert_eq!(layers.keep(&map, &keep("tile/1", 30)), Ok(true));
        let second = layers.index(&map);
        assert_ne!(second.stamp, first.stamp);
        assert_ne!(second.layers[0].1, first.layers[0].1);
        assert_eq!(second.layers.len(), 1);

        // A second layer changes the set's stamp and leaves the first one's.
        assert_eq!(layers.keep(&map, &keep("tile/2", 20)), Ok(true));
        let third = layers.index(&map);
        assert_ne!(third.stamp, second.stamp);
        assert_eq!(third.layers[0], second.layers[0]);

        let held = layers.fetch(&map, &["tile/2".into(), "tile/9".into(), "tile/1".into()]);
        assert_eq!(
            held.iter()
                .map(|layer| layer.id.as_str())
                .collect::<Vec<_>>(),
            ["tile/2", "tile/1"],
            "in the order asked, and not what is not kept"
        );
        assert_eq!(held[1].stamp, third.layers[0].1);
        assert_eq!(held[1].shapes.len(), 1);
    }

    #[test]
    fn what_is_not_understood_or_over_the_limits_is_refused_and_the_previous_set_stays() {
        let map = plugin("map");
        let mut layers = DrawingLayers::default();
        layers.keep(&map, &keep("a", 20)).unwrap();
        let before = layers.index(&map);
        let long = "a".repeat(MAX_LAYER_PATH + 1);
        for bad in [
            json!({"id":"A","shapes":[{"path":"M0 0"}]}),
            json!({"id":"a","shapes":[]}),
            json!({"id":"a","shapes":[{"path":"url(#x)"}]}),
            json!({"id":"a","shapes":[{"path":"M0 0"}],"extra":1}),
            json!({"id":"a","shapes":[{"path": long}]}),
            keep("a", MAX_LAYER_PATH + 1),
            json!("a"),
            Value::Null,
        ] {
            assert!(layers.keep(&map, &bad).is_err(), "{bad}");
            assert_eq!(layers.index(&map), before, "{bad}");
        }
        for bad in [
            json!({"ids":["A"]}),
            json!({"ids":"a"}),
            json!({"ids":[],"x":1}),
            json!({"ids": vec!["a"; peersus_api::elements::MAX_FORGET + 1]}),
        ] {
            assert!(layers.forget(&map, &bad).is_err(), "{bad}");
            assert_eq!(layers.index(&map), before, "{bad}");
        }
    }

    #[test]
    fn an_extension_keeps_at_most_so_many_layers_and_so_much_outline_and_a_refusal_changes_nothing()
    {
        let map = plugin("map");
        let mut layers = DrawingLayers::default();
        // As many layers as may be kept, each a few characters.
        for n in 0..MAX_KEPT_LAYERS {
            assert_eq!(layers.keep(&map, &keep(&format!("t{n}"), 8)), Ok(true));
        }
        let full = layers.index(&map);
        assert_eq!(full.layers.len(), MAX_KEPT_LAYERS);
        assert!(layers.keep(&map, &keep("one-more", 8)).is_err());
        assert_eq!(
            layers.index(&map),
            full,
            "the refusal kept the set as it was"
        );
        // One already kept is not one more: it may be replaced.
        assert_eq!(layers.keep(&map, &keep("t0", 12)), Ok(true));
        // Another extension's quota is its own.
        let other = plugin("chart");
        assert_eq!(layers.keep(&other, &keep("one-more", 8)), Ok(true));
        // Letting one go makes room for one.
        assert_eq!(layers.forget(&map, &json!({"ids":["t1"]})), Ok(true));
        assert_eq!(layers.keep(&map, &keep("one-more", 8)), Ok(true));

        // The same for the outline: layers of the most outline each, until the next one would pass the most in all.
        let mut layers = DrawingLayers::default();
        let each = MAX_LAYER_PATH;
        let fit = MAX_KEPT_PATH / each;
        for n in 0..fit {
            assert_eq!(layers.keep(&map, &keep(&format!("t{n}"), each)), Ok(true));
        }
        let before = layers.index(&map);
        let room = MAX_KEPT_PATH - fit * each;
        assert!(room < each, "{room}");
        assert!(layers.keep(&map, &keep("over", room + 1)).is_err());
        assert_eq!(layers.index(&map), before);
        // What is left fits, and replacing a layer by a larger one that would pass the most is refused with the old one kept.
        assert_eq!(layers.keep(&map, &keep("last", room)), Ok(true));
        assert!(
            layers.keep(&map, &keep("t0", each)).is_ok(),
            "the same size again"
        );
        assert!(layers.keep(&map, &keep("last", room + 1)).is_err());
        assert_eq!(
            layers.fetch(&map, &["last".into()])[0].shapes[0].path.len(),
            room
        );
        // Replacing by a smaller one gives the difference back, to the character.
        assert_eq!(layers.keep(&map, &keep("t1", 8)), Ok(true));
        assert!(layers.keep(&map, &keep("extra", each - 7)).is_err());
        assert_eq!(layers.keep(&map, &keep("extra", each - 8)), Ok(true));
        assert!(layers.keep(&map, &keep("more", 8)).is_err());
    }

    #[test]
    fn layers_are_forgotten_by_id_by_extension_and_with_the_extension() {
        let (map, chart) = (plugin("map"), plugin("chart"));
        let mut layers = DrawingLayers::default();
        for id in ["a", "b", "c"] {
            layers.keep(&map, &keep(id, 12)).unwrap();
        }
        layers.keep(&chart, &keep("a", 12)).unwrap();
        let stamp = layers.index(&map).stamp;
        // What is not kept is ignored, and changes nothing.
        assert_eq!(layers.forget(&map, &json!({"ids":["x","y"]})), Ok(false));
        assert_eq!(
            layers.forget(&plugin("none"), &json!({"ids":["a"]})),
            Ok(false)
        );
        assert_eq!(layers.index(&map).stamp, stamp);
        assert_eq!(layers.forget(&map, &json!({"ids":[]})), Ok(false));
        assert_eq!(layers.forget(&map, &json!({"ids":["a","x","a"]})), Ok(true));
        assert_eq!(ids(&layers, &map), ["b", "c"]);
        assert_ne!(layers.index(&map).stamp, stamp);
        assert_eq!(
            ids(&layers, &chart),
            ["a"],
            "another extension's layer of the same id stays"
        );
        // The last one gone, the set goes: the snapshot names nothing of it.
        assert_eq!(layers.forget(&map, &json!({"ids":["b","c"]})), Ok(true));
        assert_eq!(
            layers
                .stamps()
                .iter()
                .map(|kept| kept.plugin.as_str())
                .collect::<Vec<_>>(),
            ["chart"]
        );
        assert_eq!(layers.index(&map), LayerIndex::default());
        // Stopped, restarted, turned off or removed: all of its layers go, and the person's windows are told by the stamps.
        assert!(layers.forget_plugin(&chart));
        assert!(!layers.forget_plugin(&chart));
        assert!(layers.stamps().is_empty());
        assert!(layers.fetch(&chart, &["a".into()]).is_empty());
    }

    #[test]
    fn a_stamp_is_never_given_twice_nor_is_it_zero() {
        let map = plugin("map");
        let mut layers = DrawingLayers::default();
        let mut seen = std::collections::BTreeSet::new();
        for n in 0..200 {
            layers.keep(&map, &keep("a", 8 + n)).unwrap();
            assert!(seen.insert(layers.index(&map).stamp));
        }
        // Even from the end of the counter.
        layers.last = u64::MAX - 1;
        for _ in 0..3 {
            layers.keep(&map, &keep("b", 8 + seen.len())).unwrap();
            let stamp = layers.index(&map).stamp;
            assert_ne!(stamp, 0);
            seen.insert(stamp);
        }
    }

    #[test]
    fn one_answer_carries_a_bounded_amount_and_the_window_asks_again_for_the_rest() {
        let map = plugin("map");
        let mut layers = DrawingLayers::default();
        let each = MAX_LAYER_PATH;
        let count = REPLY_BYTES / each + 5;
        let names: Vec<String> = (0..count).map(|n| format!("t{n}")).collect();
        for name in &names {
            layers.keep(&map, &keep(name, each)).unwrap();
        }
        let mut wanted = names.clone();
        let mut rounds = 0;
        let mut got = Vec::new();
        while !wanted.is_empty() {
            let answer = layers.fetch(&map, &wanted);
            assert!(!answer.is_empty(), "an answer carries at least one layer");
            let sent = serde_json::to_vec(&answer).unwrap().len();
            assert!(sent <= REPLY_BYTES, "{sent}");
            wanted.retain(|id| !answer.iter().any(|layer| layer.id == *id));
            got.extend(answer.into_iter().map(|layer| layer.id));
            rounds += 1;
        }
        assert!(rounds > 1);
        assert_eq!(got, names, "all of them, in the order asked");
        // More names than may be kept are not all looked at.
        let mut layers = DrawingLayers::default();
        layers.keep(&map, &keep("t", 8)).unwrap();
        let many = vec!["t".to_owned(); MAX_KEPT_LAYERS * 2];
        assert_eq!(layers.fetch(&map, &many).len(), MAX_KEPT_LAYERS);
    }

    #[test]
    fn an_answer_is_bounded_by_what_it_weighs_serialized_and_not_by_its_outline_alone() {
        use peersus_api::elements::MAX_LAYER_SHAPES;
        let map = plugin("map");
        let mut layers = DrawingLayers::default();
        // The most layers, each of the most shapes, each shape of the least outline: hardly any outline, and a great deal of words
        // around it, which is what overran a frame to a window when only the outline was counted.
        let names: Vec<String> = (0..MAX_KEPT_LAYERS).map(|n| format!("tile/{n}")).collect();
        for name in &names {
            layers
                .keep(&map, &many_shapes(name, MAX_LAYER_SHAPES, 4))
                .unwrap();
        }
        let mut wanted = names.clone();
        let (mut got, mut rounds) = (Vec::new(), 0);
        while !wanted.is_empty() {
            let answer = layers.fetch(&map, &wanted);
            assert!(!answer.is_empty());
            let sent = serde_json::to_vec(&answer).unwrap().len();
            assert!(sent <= REPLY_BYTES, "{sent} bytes in one answer");
            wanted.retain(|id| !answer.iter().any(|layer| layer.id == *id));
            got.extend(answer.into_iter().map(|layer| layer.id));
            rounds += 1;
        }
        assert!(rounds > 1, "{rounds}");
        assert_eq!(got, names);
        // What a layer is said to weigh is never less than what it does.
        for (id, layer) in &layers.sets[&map].layers {
            let content = LayerContent {
                id: id.clone(),
                stamp: u64::MAX,
                shapes: layer.shapes.clone(),
            };
            let serialized = serde_json::to_vec(&content).unwrap().len();
            assert!(
                serialized <= weight(id, layer),
                "{serialized} > {}",
                weight(id, layer)
            );
        }
    }
}
