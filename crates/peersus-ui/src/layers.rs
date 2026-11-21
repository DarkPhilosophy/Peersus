//! What extensions keep for their drawings, as a window holds it: the layers of shapes a drawing places by id, fetched once for each
//! change and never carried in a snapshot.
//!
//! WHY. A map or a chart is drawn of the same outlines again and again, and a snapshot travels whenever anything at all changes. A snapshot
//! therefore names each extension's layers only by a stamp; a window that does not hold that stamp asks for the index (every id with the
//! stamp of its content), fetches the layers whose content it lacks, and only then keeps the set. Each layer crosses once for each
//! change, a window that opens later asks for all of them, and what stops being kept is let go of.
//!
//! WHAT BOUNDS IT. A window holds what the owner holds: at most `MAX_KEPT_LAYERS` layers of `MAX_KEPT_PATH` characters of outline in all,
//! for each extension, and an extension that stops is let go of. The outlines are parsed into paths the first time a layer is drawn and
//! kept then, up to `PARSED_PATH` characters in the whole window; beyond that a layer is parsed each time it is drawn rather than kept, so
//! a window is never made to hold more than that in parsed paths.
use freya::prelude::State;
use peersus_api::elements::{MAX_KEPT_LAYERS, MAX_KEPT_PATH, Node};
use peersus_app::runtime::{Client, Reply};
use peersus_core::{
    model::{Command, KeptLayers, LayerContent, PageView},
    plugins::PluginId,
};
use std::{cell::OnceCell, collections::BTreeMap, rc::Rc};

/// Characters of outline whose parsed paths the window keeps at most, besides the outlines themselves.
pub(crate) const PARSED_PATH: usize = 8 * 1024 * 1024;

/// One layer as it is kept: its shapes, and their paths once it was drawn.
pub struct Layer {
    /// Which content this is: an answer that says another stamp has replaced it.
    pub stamp: u64,
    /// Characters of outline in `shapes`.
    pub path: usize,
    pub shapes: Vec<peersus_api::elements::Shape>,
    /// The shapes' outlines as paths, in the same order; `None` for one no parser reads. Parsed on the first draw, when the window
    /// may keep them (see [`PARSED_PATH`]).
    paths: OnceCell<Rc<Vec<Option<skia_path::Path>>>>,
}

impl Layer {
    fn new(content: LayerContent) -> Self {
        let path = content.shapes.iter().map(|shape| shape.path.len()).sum();
        Self {
            stamp: content.stamp,
            path,
            shapes: content.shapes,
            paths: OnceCell::new(),
        }
    }

    /// The outlines of the shapes as paths, parsed once when they may be kept, and never sooner. `budget` is what the window holds of
    /// them in all, and it grows by what this layer takes.
    pub(crate) fn paths(
        self: &Rc<Self>,
        budget: Option<&std::cell::Cell<usize>>,
    ) -> Rc<Vec<Option<skia_path::Path>>> {
        if let Some(paths) = self.paths.get() {
            return paths.clone();
        }
        // An outline is only ever what the contract allows (digits and the notation's letters), so the parser is never given a byte of
        // zero; checked all the same, because it is a precondition of the parser and not a guess about the extension.
        let paths: Vec<Option<skia_path::Path>> = self
            .shapes
            .iter()
            .map(|shape| {
                (!shape.path.as_bytes().contains(&0))
                    .then(|| skia_path::Path::from_svg(&shape.path))
                    .flatten()
            })
            .collect();
        let kept = Rc::new(paths);
        if let Some(budget) = budget {
            let held = budget.get();
            if held.saturating_add(self.path) <= PARSED_PATH {
                budget.set(held + self.path);
                let _ = self.paths.set(kept.clone());
            }
        }
        kept
    }
}

/// The paths of shapes, as the window parses them: the toolkit's own, from its renderer.
mod skia_path {
    pub(super) use freya::engine::prelude::SkPath as Path;
}

/// What one extension keeps now.
#[derive(Clone, Default)]
pub struct Held {
    stamp: u64,
    layers: Rc<BTreeMap<String, Rc<Layer>>>,
}

impl Held {
    pub fn layer(&self, id: &str) -> Option<Rc<Layer>> {
        self.layers.get(id).cloned()
    }
}

/// The window's one handle on what extensions keep, provided above every page and read by every drawing that places a layer.
#[derive(Clone, PartialEq)]
pub struct KeptHandle(pub State<Kept>);

/// What every extension keeps, as the window holds it. Cloned cheaply and compared by identity: a change makes a new one, so the
/// drawings that place layers are drawn again when one arrives and not otherwise.
#[derive(Clone, Default)]
pub struct Kept(Rc<BTreeMap<PluginId, Held>>, Rc<std::cell::Cell<usize>>);

impl PartialEq for Kept {
    /// Equal when it holds the same thing, copied or not: what a snapshot that names no other stamps asks of every page is nothing,
    /// and a drawing is then not drawn again.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
            || (self.0.len() == other.0.len()
                && self.0.iter().zip(other.0.iter()).all(
                    |((plugin, held), (other_plugin, other_held))| {
                        plugin == other_plugin
                            && held.stamp == other_held.stamp
                            && Rc::ptr_eq(&held.layers, &other_held.layers)
                    },
                ))
    }
}

impl Kept {
    pub fn layer(&self, plugin: &PluginId, id: &str) -> Option<Rc<Layer>> {
        self.0.get(plugin)?.layer(id)
    }

    /// What the window keeps of parsed paths in all (see [`PARSED_PATH`]).
    pub(crate) fn budget(&self) -> &std::cell::Cell<usize> {
        &self.1
    }

    #[cfg(all(test, feature = "headless"))]
    pub(crate) fn of(sets: impl IntoIterator<Item = (PluginId, Vec<LayerContent>)>) -> Self {
        let mut held = BTreeMap::new();
        for (plugin, layers) in sets {
            let stamp = layers.iter().map(|layer| layer.stamp).max().unwrap_or(0);
            held.insert(
                plugin,
                Held {
                    stamp,
                    layers: Rc::new(
                        layers
                            .into_iter()
                            .map(|layer| (layer.id.clone(), Rc::new(Layer::new(layer))))
                            .collect(),
                    ),
                },
            );
        }
        Self(Rc::new(held), Rc::new(std::cell::Cell::new(0)))
    }
}

/// What the window asks the owner for next, to hold what a snapshot names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Job {
    /// Every id and the stamp of its content, for the stamp of the set a snapshot names.
    Index { plugin: PluginId, stamp: u64 },
    /// The content of these layers, being the ones the window lacks of `stamp`.
    Fetch {
        plugin: PluginId,
        stamp: u64,
        ids: Vec<String>,
    },
}

/// The window's side of `peersus.drawings`: what it holds for each extension, and what it must ask for to hold what a snapshot names.
#[derive(Default)]
pub(crate) struct Mirror {
    sets: BTreeMap<PluginId, Held>,
    /// The extension whose set is asked for now, so that a snapshot that has not changed meanwhile asks nothing again.
    asking: Option<(PluginId, u64)>,
    /// What the window keeps of parsed paths in all, shared with every [`Layer`] it makes.
    budget: Rc<std::cell::Cell<usize>>,
}

impl Mirror {
    /// What the window holds, shared and not copied.
    pub(crate) fn held(&self) -> Kept {
        Kept(Rc::new(self.sets.clone()), self.budget.clone())
    }

    /// A page and the stamp of its layers are one paintable revision. If the next revision is
    /// not ready, leave its previous page up (including its unresolved preview) until it is.
    /// Do not hold unrelated pages or resurrect a page removed by the owner.
    pub(crate) fn ready_pages(
        &self,
        mut next: Vec<PageView>,
        previous: &[PageView],
        named: &[KeptLayers],
    ) -> Vec<PageView> {
        next.retain_mut(|page| {
            if !named.iter().any(|set| {
                set.plugin == page.plugin
                    && self
                        .sets
                        .get(&page.plugin)
                        .is_none_or(|kept| kept.stamp != set.stamp)
            }) || !uses_layers(&page.view)
            {
                return true;
            }
            let Some(old) = previous
                .iter()
                .find(|old| old.plugin == page.plugin && old.page == page.page)
            else {
                return false;
            };
            page.view.clone_from(&old.view);
            true
        });
        next
    }

    /// What must be asked for next so that the window holds what `named` says, or none when it holds it all. Nothing is asked for an
    /// extension whose set was asked for already and not yet answered.
    pub(crate) fn next(&mut self, named: &[KeptLayers]) -> Option<Job> {
        // What an extension that is no longer named (stopped, removed, keeping nothing) is let go of, parsed paths and all: the budget
        // comes back with them.
        let named_plugins: std::collections::BTreeSet<&PluginId> =
            named.iter().map(|set| &set.plugin).collect();
        self.sets.retain(|plugin, held| {
            let keep = named_plugins.contains(plugin);
            if !keep {
                for layer in held.layers.values() {
                    if layer.paths.get().is_some() {
                        self.budget
                            .set(self.budget.get().saturating_sub(layer.path));
                    }
                }
            }
            keep
        });
        if let Some((plugin, stamp)) = &self.asking
            && !named
                .iter()
                .any(|set| set.plugin == *plugin && set.stamp == *stamp)
        {
            // The snapshot changed under the question: its answer is about a set that is no longer the one to hold.
            self.asking = None;
        }
        let (plugin, stamp) = named
            .iter()
            .find(|set| self.sets.get(&set.plugin).map(|held| held.stamp) != Some(set.stamp))
            .map(|set| (set.plugin.clone(), set.stamp))?;
        if self
            .asking
            .as_ref()
            .is_some_and(|(asking, _)| *asking == plugin)
        {
            return None;
        }
        self.asking = Some((plugin.clone(), stamp));
        Some(Job::Index { plugin, stamp })
    }

    /// Keep the index an extension answered: the layers the window does not hold at that stamp are what must be fetched; those it no
    /// longer keeps are let go of. `None` when it holds them all already.
    pub(crate) fn indexed(
        &mut self,
        plugin: &PluginId,
        stamp: u64,
        layers: &[(String, u64)],
    ) -> Option<Job> {
        let ids: Vec<String> = layers
            .iter()
            .take(MAX_KEPT_LAYERS)
            .filter(|(id, layer_stamp)| {
                self.sets
                    .get(plugin)
                    .and_then(|held| held.layer(id))
                    .is_none_or(|layer| layer.stamp != *layer_stamp)
            })
            .map(|(id, _)| id.clone())
            .collect();
        if ids.is_empty() {
            self.install(plugin, stamp, layers, Vec::new());
            return None;
        }
        Some(Job::Fetch {
            plugin: plugin.clone(),
            stamp,
            ids,
        })
    }

    /// Keep the content the owner answered for `stamp`: at most the layers it names, and none of the ones it no longer keeps.
    pub(crate) fn fetched(
        &mut self,
        plugin: &PluginId,
        stamp: u64,
        index: &[(String, u64)],
        contents: Vec<LayerContent>,
    ) -> bool {
        let arrived: BTreeMap<_, _> = contents
            .iter()
            .map(|content| (content.id.as_str(), content.stamp))
            .collect();
        if index.iter().take(MAX_KEPT_LAYERS).any(|(id, wanted)| {
            arrived.get(id.as_str()) != Some(wanted)
                && self
                    .sets
                    .get(plugin)
                    .and_then(|set| set.layers.get(id))
                    .is_none_or(|layer| layer.stamp != *wanted)
        }) {
            self.unanswered();
            return false;
        }
        self.install(plugin, stamp, index, contents);
        true
    }

    /// The extension asked for something the owner does not know (an owner from before layers): nothing more is asked for it until
    /// the snapshot changes, and what it keeps is not held.
    pub(crate) fn unanswered(&mut self) {
        self.asking = None;
    }

    /// Install an index after all of its contents arrived. Do not expose one fetch page at a time.
    fn install(
        &mut self,
        plugin: &PluginId,
        stamp: u64,
        index: &[(String, u64)],
        contents: Vec<LayerContent>,
    ) {
        let previous = self.sets.remove(plugin);
        let mut arrived: BTreeMap<String, LayerContent> = contents
            .into_iter()
            .map(|content| (content.id.clone(), content))
            .collect();
        let mut layers = BTreeMap::new();
        let mut total = 0;
        for (id, layer_stamp) in index.iter().take(MAX_KEPT_LAYERS) {
            let layer = match arrived
                .remove(id)
                .filter(|content| content.stamp == *layer_stamp)
            {
                Some(content) => Rc::new(Layer::new(content)),
                None => match previous.as_ref().and_then(|held| held.layer(id)) {
                    Some(layer) if layer.stamp == *layer_stamp => layer,
                    _ => continue,
                },
            };
            // What is held stays within what the owner may keep, whatever an owner says.
            if total + layer.path > MAX_KEPT_PATH {
                break;
            }
            total += layer.path;
            layers.insert(id.clone(), layer);
        }
        // Parsed paths of what was replaced or let go of are given back.
        if let Some(previous) = &previous {
            for (id, layer) in previous.layers.iter() {
                if layers.get(id).is_none_or(|kept| !Rc::ptr_eq(kept, layer))
                    && layer.paths.get().is_some()
                {
                    self.budget
                        .set(self.budget.get().saturating_sub(layer.path));
                }
            }
        }
        self.sets.insert(
            plugin.clone(),
            Held {
                stamp,
                layers: Rc::new(layers),
            },
        );
        self.asking = None;
    }
}

fn uses_layers(node: &Node) -> bool {
    match node {
        Node::Drawing { layers, .. } => !layers.is_empty(),
        Node::Column { children } | Node::Row { children } | Node::Box { children, .. } => {
            children.iter().any(uses_layers)
        }
        Node::Interval { child, .. } => uses_layers(child),
        _ => false,
    }
}

/// One step of [`Mirror::next`]: ask the owner for what it says, and give the answer back to the mirror. Says whether the window's
/// content may have changed.
pub(crate) async fn follow(client: &Client, mirror: &mut Mirror, job: Job) -> bool {
    let Job::Index { plugin, stamp } = job else {
        return false;
    };
    let Ok(Reply::LayerIndex(answer)) = client
        .command(Command::LayerIndex {
            plugin: plugin.clone(),
        })
        .await
    else {
        mirror.unanswered();
        return false;
    };
    if answer.stamp != stamp {
        mirror.unanswered();
        return false;
    }
    let Some(Job::Fetch { ids, .. }) = mirror.indexed(&plugin, stamp, &answer.layers) else {
        return true;
    };
    let mut missing: BTreeMap<_, _> = answer
        .layers
        .iter()
        .take(MAX_KEPT_LAYERS)
        .filter(|(id, _)| ids.contains(id))
        .map(|(id, revision)| (id.as_str(), *revision))
        .collect();
    let mut fetched = Vec::with_capacity(missing.len());
    let mut outline_bytes = 0;
    // A fetch reply carries at most 1 MiB. Request its remaining names, not the whole index
    // again, and verify every returned layer against the original per-layer stamp. An unrelated
    // keep can advance the set stamp in the meantime without changing what we fetched.
    while !missing.is_empty() {
        let Ok(Reply::Layers(contents)) = client
            .command(Command::FetchLayers {
                plugin: plugin.clone(),
                ids: missing.keys().map(|id| (*id).to_owned()).collect(),
            })
            .await
        else {
            break;
        };
        let before = missing.len();
        for content in contents {
            if missing.get(content.id.as_str()) == Some(&content.stamp) {
                let bytes: usize = content.shapes.iter().map(|shape| shape.path.len()).sum();
                outline_bytes += bytes;
                if outline_bytes > MAX_KEPT_PATH {
                    mirror.unanswered();
                    return false;
                }
                missing.remove(content.id.as_str());
                fetched.push(content);
            }
        }
        if missing.len() == before {
            break;
        }
    }
    if missing.is_empty() {
        mirror.fetched(&plugin, stamp, &answer.layers, fetched)
    } else {
        mirror.unanswered();
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::elements::Shape;

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }

    fn layer(id: &str, stamp: u64) -> LayerContent {
        LayerContent {
            id: id.into(),
            stamp,
            shapes: vec![Shape {
                path: "M0 0 L9 9".into(),
                stroke: peersus_api::elements::Colour::Accent,
                line: 10,
                fill: peersus_api::elements::Colour::Transparent,
                opacity: None,
                round: false,
            }],
        }
    }

    fn index(layers: &[(&str, u64)]) -> Vec<(String, u64)> {
        layers
            .iter()
            .map(|(id, stamp)| ((*id).to_owned(), *stamp))
            .collect()
    }

    #[test]
    fn a_window_asks_for_what_a_snapshot_names_and_holds_it_only_once_it_arrived() {
        let map = plugin("map");
        let mut mirror = Mirror::default();
        assert_eq!(mirror.next(&[]), None);
        let named = [KeptLayers {
            plugin: map.clone(),
            stamp: 10,
        }];
        assert_eq!(
            mirror.next(&named),
            Some(Job::Index {
                plugin: map.clone(),
                stamp: 10
            })
        );
        // Asked for already: a snapshot that arrives meanwhile asks nothing again.
        assert_eq!(mirror.next(&named), None);
        let asked = mirror.indexed(&map, 10, &index(&[("a", 1), ("b", 2)]));
        assert_eq!(
            asked,
            Some(Job::Fetch {
                plugin: map.clone(),
                stamp: 10,
                ids: vec!["a".into(), "b".into()]
            })
        );
        assert!(
            mirror.held().layer(&map, "a").is_none(),
            "not before it arrived"
        );
        mirror.fetched(
            &map,
            10,
            &index(&[("a", 1), ("b", 2)]),
            vec![layer("a", 1), layer("b", 2)],
        );
        assert_eq!(mirror.held().layer(&map, "a").unwrap().stamp, 1);
        assert_eq!(mirror.held().layer(&map, "b").unwrap().shapes.len(), 1);
        assert_eq!(mirror.next(&named), None, "held at that stamp");
    }

    #[test]
    fn a_new_stamp_is_followed_and_only_the_layers_that_changed_are_fetched() {
        let map = plugin("map");
        let mut mirror = Mirror::default();
        let held = [("a", 1), ("b", 2)];
        mirror.fetched(&map, 10, &index(&held), vec![layer("a", 1), layer("b", 2)]);
        let named = [KeptLayers {
            plugin: map.clone(),
            stamp: 11,
        }];
        assert_eq!(
            mirror.next(&named),
            Some(Job::Index {
                plugin: map.clone(),
                stamp: 11
            })
        );
        // b was replaced, a stayed, c is new; a layer let go of is let go of here.
        let now = index(&[("a", 1), ("b", 3), ("c", 9)]);
        assert_eq!(
            mirror.indexed(&map, 11, &now),
            Some(Job::Fetch {
                plugin: map.clone(),
                stamp: 11,
                ids: vec!["b".into(), "c".into()]
            })
        );
        mirror.fetched(&map, 11, &now, vec![layer("b", 3), layer("c", 9)]);
        let kept = mirror.held();
        assert_eq!(kept.layer(&map, "a").unwrap().stamp, 1, "kept from before");
        assert_eq!(kept.layer(&map, "b").unwrap().stamp, 3);
        assert!(kept.layer(&map, "d").is_none());
    }

    #[test]
    fn a_partial_fetch_does_not_replace_the_visible_layer_set() {
        let map = plugin("map");
        let mut mirror = Mirror::default();
        mirror.fetched(&map, 1, &index(&[("old", 1)]), vec![layer("old", 1)]);
        let all = index(&[("fresh-a", 2), ("fresh-b", 3)]);
        let page = |id: &str| PageView {
            plugin: map.clone(),
            page: "map".into(),
            view: Node::parse(&serde_json::json!({"k":"drawing","width":200,"height":200,
                "layers":[{"id":id,"scale":1000000,"at":[0,0]}]}))
            .unwrap(),
        };
        let previous = [page("old")];
        let next = [page("fresh-b")];
        let named = [KeptLayers {
            plugin: map.clone(),
            stamp: 3,
        }];
        mirror.fetched(&map, 3, &all, vec![layer("fresh-a", 2)]);
        let held = mirror.held();
        assert!(held.layer(&map, "old").is_some());
        assert!(held.layer(&map, "fresh-a").is_none());
        assert_eq!(
            mirror.ready_pages(next.to_vec(), &previous, &named),
            previous
        );
        mirror.fetched(
            &map,
            3,
            &all,
            vec![layer("fresh-a", 2), layer("fresh-b", 3)],
        );
        let held = mirror.held();
        assert!(held.layer(&map, "old").is_none());
        assert!(held.layer(&map, "fresh-a").is_some());
        assert!(held.layer(&map, "fresh-b").is_some());
        assert_eq!(mirror.ready_pages(next.to_vec(), &previous, &named), next);
    }

    #[test]
    fn an_extension_that_stops_or_keeps_nothing_is_let_go_of_with_what_it_parsed() {
        let (map, chart) = (plugin("map"), plugin("chart"));
        let mut mirror = Mirror::default();
        for (which, (plugin, id)) in [(&map, "a"), (&chart, "a")].into_iter().enumerate() {
            mirror.fetched(plugin, 5, &index(&[(id, 1)]), vec![layer(id, 1)]);
            let layer = mirror.held().layer(plugin, id).unwrap();
            let paths = layer.paths(Some(mirror.held().budget()));
            assert!(paths[0].is_some(), "an outline the parser reads");
            assert_eq!(
                mirror.held().budget().get(),
                (which + 1) * "M0 0 L9 9".len()
            );
        }
        assert_eq!(mirror.held().budget().get(), 2 * "M0 0 L9 9".len());
        mirror.next(&[KeptLayers {
            plugin: chart.clone(),
            stamp: 5,
        }]);
        assert!(mirror.held().layer(&map, "a").is_none());
        assert!(mirror.held().layer(&chart, "a").is_some());
        assert_eq!(
            mirror.held().budget().get(),
            "M0 0 L9 9".len(),
            "what was let go of is given back"
        );
        // Nothing at all named: nothing is held.
        mirror.next(&[]);
        assert!(mirror.held().layer(&chart, "a").is_none());
        assert_eq!(mirror.held().budget().get(), 0);
    }

    #[test]
    fn an_answer_about_another_set_than_the_one_asked_for_is_not_believed() {
        let map = plugin("map");
        let mut mirror = Mirror::default();
        let named = [KeptLayers {
            plugin: map.clone(),
            stamp: 10,
        }];
        assert!(matches!(mirror.next(&named), Some(Job::Index { .. })));
        // The owner answered about a set the snapshot no longer names (another change happened meanwhile).
        mirror.unanswered();
        assert!(mirror.held().layer(&map, "a").is_none());
        // It is asked again for the new stamp.
        assert_eq!(
            mirror.next(&[KeptLayers {
                plugin: map.clone(),
                stamp: 12
            }]),
            Some(Job::Index {
                plugin: map.clone(),
                stamp: 12
            })
        );
        // A layer the owner no longer keeps is not held, whatever arrived for it.
        mirror.fetched(
            &map,
            12,
            &index(&[("a", 2)]),
            vec![layer("a", 2), layer("gone", 2)],
        );
        assert!(mirror.held().layer(&map, "a").is_some());
        assert!(mirror.held().layer(&map, "gone").is_none());
        // A layer whose content is another stamp than the index said is not held either.
        let map = plugin("map2");
        mirror.fetched(&map, 1, &index(&[("a", 3)]), vec![layer("a", 4)]);
        assert!(mirror.held().layer(&map, "a").is_none());
    }

    #[test]
    fn what_is_held_stays_within_what_the_owner_may_keep_whatever_an_owner_says() {
        let map = plugin("map");
        let mut mirror = Mirror::default();
        // An owner of its own making answers with more than the contract allows: the window keeps the most of it and no more.
        let ids: Vec<(String, u64)> = (0..peersus_api::elements::MAX_KEPT_LAYERS + 50)
            .map(|n| (format!("t{n}"), 1))
            .collect();
        let contents = ids.iter().map(|(id, stamp)| layer(id, *stamp)).collect();
        mirror.fetched(&map, 1, &ids, contents);
        let held = mirror.held();
        let kept = ids
            .iter()
            .filter(|(id, _)| held.layer(&map, id).is_some())
            .count();
        assert_eq!(kept, peersus_api::elements::MAX_KEPT_LAYERS);
    }

    /// A drawing of one layer with a bright horizontal line 4 pixels thick, placed at `scale` of a million.
    #[cfg(feature = "headless")]
    fn line_pixels(scale: u32) -> Vec<usize> {
        use freya::prelude::*;
        use freya_testing::TestingRunner;
        use peersus_api::elements::{Colour, Placed, Shape};
        let map = plugin("map");
        let kept = Kept::of([(
            map.clone(),
            vec![LayerContent {
                id: "t/1".into(),
                stamp: 1,
                shapes: vec![Shape {
                    path: "M0 50 L100 50".into(),
                    stroke: Colour::Text,
                    line: 40,
                    fill: Colour::Transparent,
                    opacity: None,
                    round: false,
                }],
            }],
        )]);
        let (mut runner, ()) = TestingRunner::new(
            move || {
                rect().expanded().background((0, 0, 0)).child(
                    crate::contributed_page::drawing_with_layers(
                        200,
                        200,
                        &[Placed {
                            id: "t/1".into(),
                            scale,
                            at: [0, 0],
                        }],
                        Some(&map),
                        Some(&kept),
                        &[],
                        crate::theme::DARK,
                    ),
                )
            },
            (200., 200.).into(),
            |_| (),
            1.,
        );
        runner.sync_and_update();
        let pixels = image::load_from_memory(runner.render().as_bytes())
            .unwrap()
            .to_rgba8();
        // Which rows, at a column inside the line, are painted by it clearly enough to be the stroke rather than its edge.
        (0..200_u32)
            .filter(|y| {
                let pixel = pixels.get_pixel(20, *y);
                let pixel: &image::Rgba<u8> = pixel;
                pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200
            })
            .map(|y| y as usize)
            .collect()
    }

    #[test]
    #[cfg(feature = "headless")]
    fn a_placed_layer_is_drawn_where_it_is_placed_with_lines_as_thick_as_they_were_kept() {
        // A million draws it as it was kept: the line is at y = 50, four pixels thick.
        let as_kept = line_pixels(1_000_000);
        assert!(!as_kept.is_empty(), "the placed layer was not drawn");
        assert!(as_kept.contains(&50), "{as_kept:?}");
        assert!(as_kept.iter().all(|y| (46..54).contains(y)), "{as_kept:?}");
        // Twice as large: the line is at y = 100, and it is exactly as thick as it was kept: a line of a map does not thicken with
        // the zoom.
        let enlarged = line_pixels(2_000_000);
        assert!(enlarged.contains(&100), "{enlarged:?}");
        assert!(
            enlarged.iter().all(|y| (96..104).contains(y)),
            "{enlarged:?}"
        );
        assert!(
            enlarged.len().abs_diff(as_kept.len()) <= 1,
            "{as_kept:?} became {enlarged:?}"
        );
    }

    #[test]
    fn parsed_paths_are_kept_only_up_to_what_the_window_may_hold_of_them() {
        // A layer of the most outline, drawn twice: kept once, and not again.
        let map = plugin("map");
        let mut mirror = Mirror::default();
        let shape = |len: usize| Shape {
            path: "M0 0".to_owned() + &" L1 1".repeat(len / 5),
            stroke: peersus_api::elements::Colour::Accent,
            line: 10,
            fill: peersus_api::elements::Colour::Transparent,
            opacity: None,
            round: false,
        };
        let each = peersus_api::elements::MAX_LAYER_PATH;
        let mut ids = Vec::new();
        let mut contents = Vec::new();
        for n in 0..(PARSED_PATH / each + 1) {
            let id = format!("t{n}");
            ids.push((id.clone(), 1));
            contents.push(LayerContent {
                id,
                stamp: 1,
                shapes: vec![shape(each)],
            });
        }
        mirror.fetched(&map, 1, &ids, contents);
        let held = mirror.held();
        let budget = held.budget();
        let mut kept = 0;
        for (id, _) in &ids {
            let layer = held.layer(&map, id).unwrap();
            let first = layer.paths(Some(budget));
            assert!(first[0].is_some());
            if budget.get() <= PARSED_PATH {
                kept += usize::from(layer.paths.get().is_some());
            }
        }
        assert!(kept > 0);
        assert!(budget.get() <= PARSED_PATH, "{}", budget.get());
        // The one over is parsed each time and not kept: the window is never made to hold more.
        let over = held.layer(&map, &ids[ids.len() - 1].0).unwrap();
        assert!(over.paths.get().is_none());
        assert!(over.paths(Some(budget))[0].is_some(), "still drawn");
    }
}
