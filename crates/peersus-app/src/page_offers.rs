//! Additional placements of a caller's existing pages, never replacements or new sources.
use peersus_api::{
    contributions::{MAX_PAGES, Page, Placement},
    manifest::Manifest,
};
use peersus_core::plugins::PluginId;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const SERVICE: &str = "peersus.pages";

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

    pub(crate) fn forget(&mut self, plugin: &PluginId) {
        self.pages.remove(plugin);
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
}
