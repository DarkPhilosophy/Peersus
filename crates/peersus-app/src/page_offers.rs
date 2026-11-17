//! How a caller offers what its own manifest already declares: additional placements of its pages, never replacements
//! or new sources; and actions it takes back for now, never new ones. Both are the caller's only, and go with it.
use peersus_api::{
    contributions::{MAX_PAGES, Page, Placement},
    manifest::Manifest,
};
use peersus_core::plugins::PluginId;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SERVICE: &str = "peersus.pages";
/// The service by which a caller says which of its own declared actions are not to be offered now.
pub(crate) const ACTIONS_SERVICE: &str = "peersus.actions";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Withhold {
    actions: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    pages: Vec<Offer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    id: String,
    page: String,
    placement: Placement,
}

#[derive(Default)]
pub(crate) struct PageOffers {
    pages: BTreeMap<PluginId, Vec<Page>>,
    withheld: BTreeMap<PluginId, BTreeSet<String>>,
}

impl PageOffers {
    pub(crate) fn pages(&self, plugin: &PluginId) -> &[Page] {
        self.pages.get(plugin).map_or(&[], Vec::as_slice)
    }

    /// Replace only this caller's additional placements. A refusal preserves the last good set.
    /// Templates are manifest pages: copying them preserves source, capability and platform scope.
    pub(crate) fn set(
        &mut self,
        plugin: &PluginId,
        manifest: &Manifest,
        args: &Value,
    ) -> Result<Vec<String>, &'static str> {
        const REFUSED: &str = "Those page placements were not understood";
        let request: Request = serde_json::from_value(args.clone()).map_err(|_| REFUSED)?;
        if request.pages.len() + manifest.contributions.pages.len() > MAX_PAGES {
            return Err(REFUSED);
        }
        let mut pages = Vec::with_capacity(request.pages.len());
        for offer in request.pages {
            let mut page = manifest
                .contributions
                .pages
                .iter()
                .find(|page| page.id == offer.page)
                .cloned()
                .ok_or(REFUSED)?;
            page.id = offer.id;
            page.placement = offer.placement;
            pages.push(page);
        }
        let mut combined = manifest.clone();
        combined.contributions.pages.extend(pages.iter().cloned());
        combined.validate().map_err(|_| REFUSED)?;
        let removed = self
            .pages(plugin)
            .iter()
            .filter(|previous| !pages.contains(previous))
            .map(|page| page.id.clone())
            .collect();
        if pages.is_empty() {
            self.pages.remove(plugin);
        } else {
            self.pages.insert(plugin.clone(), pages);
        }
        Ok(removed)
    }

    /// Whether the caller took this action of its own back: it is not offered and cannot be run until the caller says otherwise
    /// or stops.
    pub(crate) fn withholds(&self, plugin: &PluginId, action: &str) -> bool {
        self.withheld
            .get(plugin)
            .is_some_and(|actions| actions.contains(action))
    }

    /// Replace the set of this caller's own actions that are not offered now; an empty set offers them all. Only actions its
    /// manifest declares can be named: a refusal preserves the last good set. Says whether anything changed.
    pub(crate) fn withhold(
        &mut self,
        plugin: &PluginId,
        manifest: &Manifest,
        args: &Value,
    ) -> Result<bool, &'static str> {
        const REFUSED: &str = "Those actions were not understood";
        let request: Withhold = serde_json::from_value(args.clone()).map_err(|_| REFUSED)?;
        let declared = |id: &String| {
            manifest
                .contributions
                .actions
                .iter()
                .any(|action| action.id == *id)
        };
        if !request.actions.iter().all(declared) {
            return Err(REFUSED);
        }
        let before = self.withheld.get(plugin).cloned().unwrap_or_default();
        let changed = before != request.actions;
        if request.actions.is_empty() {
            self.withheld.remove(plugin);
        } else {
            self.withheld.insert(plugin.clone(), request.actions);
        }
        Ok(changed)
    }

    pub(crate) fn forget(&mut self, plugin: &PluginId) {
        self.pages.remove(plugin);
        self.withheld.remove(plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "manifest_version": 1, "capabilities": {}, "imports": [],
            "exports": [{"service":"example.editor", "version":1, "description":"Edits a document",
                "consumers":["host"], "methods":{"view":{"description":"Shows the editor"}}, "events":{}}],
            "contributions":{"pages":[{"id":"editor", "label":"Editor", "placement":"settings",
                "surface":"elements", "platforms":["linux"],
                "source":{"service":"example.editor", "method":"view"}}]}
        })).unwrap()
    }

    fn offered() -> Value {
        json!({"pages":[{"id":"editor-main", "page":"editor", "placement":"primary"}]})
    }

    #[test]
    fn additional_placement_keeps_the_original_and_its_exact_source_and_scope() {
        let manifest = manifest();
        manifest.validate().unwrap();
        let plugin = PluginId::new("example").unwrap();
        let mut offers = PageOffers::default();
        assert!(
            offers
                .set(&plugin, &manifest, &offered())
                .unwrap()
                .is_empty()
        );
        let mut expected = manifest.contributions.pages[0].clone();
        expected.id = "editor-main".into();
        expected.placement = Placement::Primary;
        assert_eq!(offers.pages(&plugin), &[expected]);
        assert_eq!(
            manifest.contributions.pages[0].placement,
            Placement::Settings
        );
        assert_eq!(
            offers
                .set(&plugin, &manifest, &json!({"pages":[]}))
                .unwrap(),
            ["editor-main"]
        );
        assert!(offers.pages(&plugin).is_empty());
        assert_eq!(
            manifest.contributions.pages[0].placement,
            Placement::Settings
        );
    }

    #[test]
    fn invalid_requests_cannot_replace_good_pages_or_select_another_owner() {
        let manifest = manifest();
        let plugin = PluginId::new("example").unwrap();
        let mut offers = PageOffers::default();
        offers.set(&plugin, &manifest, &offered()).unwrap();
        let previous = offers.pages(&plugin).to_vec();
        for invalid in [
            json!({"pages":[], "caller":"other"}),
            json!({"pages":[{"id":"extra", "page":"unknown", "placement":"primary"}]}),
            json!({"pages":[{"id":"editor", "page":"editor", "placement":"primary"}]}),
            json!({"pages":[{"id":"extra", "page":"editor", "placement":"primary", "source":{}}]}),
            json!({"pages":[{"id":"extra", "page":"editor", "placement":"primary"},
                            {"id":"extra", "page":"editor", "placement":"status"}]}),
            json!({"pages": (0..MAX_PAGES).map(|i| json!({"id":format!("extra-{i}"), "page":"editor", "placement":"primary"})).collect::<Vec<_>>()}),
        ] {
            assert!(offers.set(&plugin, &manifest, &invalid).is_err());
            assert_eq!(offers.pages(&plugin), previous);
        }
    }

    #[test]
    fn replacing_or_forgetting_a_callers_pages_does_not_touch_another_caller() {
        let manifest = manifest();
        let a = PluginId::new("a").unwrap();
        let b = PluginId::new("b").unwrap();
        let mut offers = PageOffers::default();
        offers.set(&a, &manifest, &offered()).unwrap();
        offers.set(&b, &manifest, &offered()).unwrap();
        let before = offers.pages(&b).to_vec();
        offers.set(&a, &manifest, &json!({"pages":[]})).unwrap();
        assert!(offers.pages(&a).is_empty());
        assert_eq!(offers.pages(&b), before);
        offers.forget(&b);
        assert!(offers.pages(&b).is_empty());
    }

    #[test]
    fn a_caller_takes_back_only_its_own_declared_actions_and_gives_them_back() {
        let mut manifest = manifest();
        manifest.contributions.actions = serde_json::from_value(json!([
            {"id":"send", "label":"Send", "target":{"service":"example.editor", "method":"view"}},
            {"id":"setup", "label":"Set up", "target":{"service":"example.editor", "method":"view"}}
        ]))
        .unwrap();
        let (a, b) = (PluginId::new("a").unwrap(), PluginId::new("b").unwrap());
        let mut offers = PageOffers::default();
        assert!(!offers.withholds(&a, "send"));
        assert_eq!(
            offers.withhold(&a, &manifest, &json!({"actions":["send"]})),
            Ok(true)
        );
        assert!(offers.withholds(&a, "send"));
        assert!(!offers.withholds(&a, "setup"));
        assert!(!offers.withholds(&b, "send"), "another caller's is its own");
        // Said again it changes nothing; what is not understood keeps the last good set.
        assert_eq!(
            offers.withhold(&a, &manifest, &json!({"actions":["send"]})),
            Ok(false)
        );
        for invalid in [
            json!({"actions":["unknown"]}),
            json!({"actions":["send"], "caller":"b"}),
            json!({"actions":"send"}),
            json!({}),
        ] {
            assert!(offers.withhold(&a, &manifest, &invalid).is_err());
            assert!(offers.withholds(&a, "send"));
        }
        assert_eq!(
            offers.withhold(&a, &manifest, &json!({"actions":[]})),
            Ok(true)
        );
        assert!(!offers.withholds(&a, "send"));
        // It goes with the caller, like its placements.
        offers
            .withhold(&a, &manifest, &json!({"actions":["send", "setup"]}))
            .unwrap();
        offers.forget(&a);
        assert!(!offers.withholds(&a, "send") && !offers.withholds(&a, "setup"));
    }
}
