//! Discover source identities from the extension branch, then obtain their independently signed
//! release approvals. API metadata selects candidates, never authorizes an installation.
use crate::{
    catalog::{self, Refusal},
    store_runtime::StoreFetcher,
};
use peersus_api::package::{self, Package};
use peersus_core::plugins::PluginId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const INVALID: Refusal = Refusal("The extension source listing could not be checked");

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub plugin: String,
    pub version: String,
    /// Original signed bytes, not reserialized JSON.
    pub document: String,
    pub signature: Vec<u8>,
    /// Asset IDs are untrusted locators; the signed archive hash is still mandatory.
    pub assets: BTreeMap<String, u64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema: u32,
    pub approvals: Vec<Approval>,
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
}

/// A recursive listing of one branch, as GitHub's trees API gives it. Also read for the languages branch.
#[derive(Deserialize)]
pub(crate) struct Tree {
    pub(crate) truncated: bool,
    pub(crate) tree: Vec<Blob>,
}
#[derive(Deserialize)]
pub(crate) struct Blob {
    pub(crate) path: String,
    pub(crate) sha: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
}

impl Blob {
    /// Whether this is a file named by a well-formed blob id (40 lowercase hex digits), which is all a blob is fetched by.
    pub(crate) fn is_file(&self) -> bool {
        self.kind == "blob"
            && self.sha.len() == 40
            && self
                .sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}
#[derive(Deserialize)]
struct Release {
    draft: bool,
    prerelease: bool,
    tag_name: String,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    state: String,
}

/// The source manifest of each extension in one branch snapshot: `extension/<dir>/wasm/plugin.json`, the official WebAssembly
/// build's, or `extension/<dir>/plugin.json` for a source without one. Its version names the release that is looked up, so it
/// is the manifest of what the store installs, never the root manifest of a native build beside it.
fn candidates(bytes: &[u8]) -> Result<Vec<(String, String)>, Refusal> {
    let tree: Tree = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    if tree.truncated {
        return Err(INVALID);
    }
    // Plugin id -> (source directory, whether it is the WASM manifest, blob).
    let mut found: BTreeMap<String, (String, bool, String)> = BTreeMap::new();
    for blob in tree.tree {
        let parts: Vec<_> = blob.path.split('/').collect();
        let wasm = match parts.as_slice() {
            ["extension", _, "plugin.json"] => false,
            ["extension", _, "wasm", "plugin.json"] => true,
            _ => continue,
        };
        // Match the trusted submission planner: source branding is not a second plugin ID.
        let plugin = parts[1].strip_prefix("peersus-").unwrap_or(parts[1]);
        if !blob.is_file() || PluginId::new(plugin).is_err() {
            return Err(INVALID);
        }
        match found.get(plugin) {
            // Two directories for one id, or the same manifest twice.
            Some((directory, seen_wasm, _)) if directory != parts[1] || *seen_wasm == wasm => {
                return Err(INVALID);
            }
            Some((_, true, _)) => {}
            _ => {
                found.insert(plugin.to_owned(), (parts[1].to_owned(), wasm, blob.sha));
            }
        }
        if found.len() > catalog::MAX_ENTRIES {
            return Err(INVALID);
        }
    }
    Ok(found
        .into_iter()
        .map(|(plugin, (_, _, sha))| (plugin, sha))
        .collect())
}

/// Calls are serialized and manifests are addressed by the immutable blob from one tree snapshot.
/// A source version with no approved release is not installable and is not presented as approved.
pub async fn discover(
    fetcher: &dyn StoreFetcher,
    previous: Option<&Index>,
) -> Result<Index, Refusal> {
    let bytes = fetcher.fetch("store-tree".into(), MAX_INDEX_BYTES).await?;
    let mut approvals = Vec::new();
    let mut total = 0usize;
    let mut sources = BTreeMap::new();
    let cached: BTreeMap<_, _> = previous
        .into_iter()
        .flat_map(|index| &index.approvals)
        .map(|approval| (approval.plugin.as_str(), approval))
        .collect();
    for (plugin, blob) in candidates(&bytes)? {
        sources.insert(plugin.clone(), blob.clone());
        if previous.and_then(|index| index.sources.get(&plugin)) == Some(&blob)
            && let Some(approval) = cached.get(plugin.as_str())
        {
            total = total
                .checked_add(approval.document.len() + approval.signature.len())
                .ok_or(INVALID)?;
            if total > MAX_INDEX_BYTES / 2 {
                return Err(INVALID);
            }
            approvals.push((*approval).clone());
            continue;
        }
        let manifest = fetcher
            .fetch(format!("store-blob-{blob}"), 64 * 1024)
            .await?;
        let package: Package = package::split(&manifest)
            .map_err(|_| INVALID)?
            .package
            .ok_or(INVALID)?;
        let tag = format!("extension/{plugin}/v{}", package.version);
        let metadata = fetcher
            .fetch(
                format!("store-release-{plugin}--v{}", package.version),
                1024 * 1024,
            )
            .await?;
        let release: Option<Release> = serde_json::from_slice(&metadata).map_err(|_| INVALID)?;
        let Some(release) = release else {
            continue;
        };
        if release.draft
            || release.prerelease
            || release.tag_name != tag
            || release.assets.len() > 36
        {
            return Err(INVALID);
        }
        let mut names = BTreeSet::new();
        if release.assets.iter().any(|asset| {
            asset.id == 0 || asset.state != "uploaded" || !names.insert(asset.name.as_str())
        }) {
            return Err(INVALID);
        }
        let proof = release
            .assets
            .iter()
            .find(|asset| asset.name == "approval.json")
            .ok_or(INVALID)?;
        let signature = release
            .assets
            .iter()
            .find(|asset| asset.name == "approval.sig")
            .ok_or(INVALID)?;
        if proof.size > catalog::MAX_CATALOG_BYTES as u64 || signature.size != 64 {
            return Err(INVALID);
        }
        let document = fetcher
            .fetch(
                format!("store-asset-{}", proof.id),
                catalog::MAX_CATALOG_BYTES,
            )
            .await?;
        let signature = fetcher
            .fetch(format!("store-asset-{}", signature.id), 64)
            .await?;
        total = total
            .checked_add(document.len() + signature.len())
            .ok_or(INVALID)?;
        if total > MAX_INDEX_BYTES / 2 {
            return Err(INVALID);
        }
        approvals.push(Approval {
            plugin,
            version: package.version,
            document: String::from_utf8(document).map_err(|_| INVALID)?,
            signature,
            assets: release
                .assets
                .into_iter()
                .filter(|asset| asset.name.ends_with(".tar.gz"))
                .map(|asset| (asset.name, asset.id))
                .collect(),
        });
    }
    Ok(Index {
        schema: 1,
        approvals,
        sources,
    })
}

pub(crate) struct VerifiedIndex {
    pub catalog: catalog::Catalog,
    pub locators: BTreeMap<String, String>,
    pub floors: Index,
}

fn authenticate(
    key: &ed25519_dalek::VerifyingKey,
    approval: &Approval,
) -> Result<(catalog::Catalog, package::Version), Refusal> {
    PluginId::new(&approval.plugin).map_err(|_| INVALID)?;
    let catalog = catalog::verify(key, approval.document.as_bytes(), &approval.signature, 0)?;
    // Read identity only AFTER authenticating all bytes and validating every platform artifact.
    let wire: serde_json::Value = serde_json::from_str(&approval.document).map_err(|_| INVALID)?;
    let entries = wire["entries"].as_array().ok_or(INVALID)?;
    if entries.len() != 1
        || entries[0]["name"].as_str() != Some(approval.plugin.as_str())
        || entries[0]["version"].as_str() != Some(approval.version.as_str())
    {
        return Err(INVALID);
    }
    Ok((
        catalog,
        package::Version::parse(&approval.version).ok_or(INVALID)?,
    ))
}

/// Independent, authenticated version floors: one plugin's release counter cannot
/// authorize a rollback of another. Missing plugins retain their historical floor.
pub(crate) fn verify(
    key: &ed25519_dalek::VerifyingKey,
    index: &Index,
    data: &std::path::Path,
) -> Result<VerifiedIndex, Refusal> {
    if index.schema != 1 || index.approvals.len() > catalog::MAX_ENTRIES {
        return Err(INVALID);
    }
    let path = data.join("store-admissions.json");
    let history = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Index {
            schema: 1,
            approvals: Vec::new(),
            sources: BTreeMap::new(),
        },
        Err(_) => return Err(Refusal("The saved extension approvals could not be read")),
        Ok(_) => serde_json::from_slice::<Index>(&crate::store_runtime::read_bounded(
            &path,
            MAX_INDEX_BYTES,
        )?)
        .map_err(|_| Refusal("The saved extension approvals are damaged"))?,
    };
    if history.schema != 1 || history.approvals.len() > catalog::MAX_ENTRIES {
        return Err(INVALID);
    }
    let mut previous = BTreeMap::new();
    for approval in history.approvals {
        let (_, version) = authenticate(key, &approval)?;
        if previous
            .insert(approval.plugin.clone(), (version, approval))
            .is_some()
        {
            return Err(INVALID);
        }
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    let mut locators = BTreeMap::new();
    let mut sequence = 0;
    for approval in &index.approvals {
        if !seen.insert(approval.plugin.as_str()) {
            return Err(INVALID);
        }
        let (approved, version) = authenticate(key, approval)?;
        if let Some((accepted, old)) = previous.get(&approval.plugin)
            && (version < *accepted || (version == *accepted && approval.document != old.document))
        {
            return Err(Refusal(
                "The extension approval is older than one already accepted",
            ));
        }
        sequence = sequence.max(approved.sequence);
        for entry in approved.entries {
            let asset = approval
                .assets
                .get(&entry.archive)
                .copied()
                .filter(|id| *id > 0)
                .ok_or(INVALID)?;
            locators.insert(entry.name.clone(), format!("store-asset-{asset}"));
            entries.push(entry);
        }
        previous.insert(approval.plugin.clone(), (version, approval.clone()));
    }
    if previous.len() > catalog::MAX_ENTRIES {
        return Err(INVALID);
    }
    Ok(VerifiedIndex {
        catalog: catalog::Catalog {
            schema: 2,
            sequence,
            entries,
        },
        locators,
        floors: Index {
            schema: 1,
            approvals: previous
                .into_values()
                .map(|(_, approval)| approval)
                .collect(),
            sources: BTreeMap::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_runtime::fake::MapFetcher;

    #[tokio::test]
    async fn an_empty_branch_is_a_valid_empty_store_without_any_remote_catalog() {
        let fetcher = MapFetcher::default();
        fetcher.put(
            "store-tree",
            br#"{"truncated":false,"tree":[{"path":"README.md","type":"blob","sha":"unneeded"}]}"#
                .to_vec(),
        );
        assert!(discover(&fetcher, None).await.unwrap().approvals.is_empty());
    }

    #[test]
    fn incomplete_trees_duplicate_plugins_and_invalid_blobs_cannot_replace_a_cache() {
        for data in [
            serde_json::json!({"truncated": true, "tree": []}),
            serde_json::json!({"truncated": false, "tree": [{"path":"extension/upload/plugin.json","type":"blob","sha":"invalid"}]}),
            serde_json::json!({"truncated": false, "tree": [
                {"path":"extension/upload/plugin.json","type":"blob","sha":"a".repeat(40)},
                {"path":"extension/upload/plugin.json","type":"blob","sha":"b".repeat(40)}]}),
        ] {
            assert!(candidates(&serde_json::to_vec(&data).unwrap()).is_err());
        }
    }

    #[test]
    fn source_branding_maps_to_the_stable_installation_id_and_collisions_are_refused() {
        let tree = serde_json::json!({"truncated": false, "tree": [
            {"path":"extension/peersus-upload/plugin.json","type":"blob","sha":"a".repeat(40)}
        ]});
        assert_eq!(
            candidates(&serde_json::to_vec(&tree).unwrap()).unwrap(),
            vec![("upload".into(), "a".repeat(40))]
        );
        let duplicate = serde_json::json!({"truncated": false, "tree": [
            {"path":"extension/peersus-upload/plugin.json","type":"blob","sha":"a".repeat(40)},
            {"path":"extension/upload/plugin.json","type":"blob","sha":"b".repeat(40)}
        ]});
        assert!(candidates(&serde_json::to_vec(&duplicate).unwrap()).is_err());
    }

    #[test]
    fn the_official_wasm_manifest_names_the_version_and_a_root_manifest_is_the_fallback() {
        let blob = |path: &str, sha: char| serde_json::json!({"path": path, "type": "blob", "sha": sha.to_string().repeat(40)});
        let found = |tree: Vec<serde_json::Value>| {
            candidates(
                &serde_json::to_vec(&serde_json::json!({"truncated": false, "tree": tree}))
                    .unwrap(),
            )
        };
        // A native build's root manifest beside the WASM one, in either order: the WASM manifest is the source.
        for tree in [
            vec![
                blob("extension/peersus-clipboard/plugin.json", 'a'),
                blob("extension/peersus-clipboard/wasm/plugin.json", 'b'),
            ],
            vec![
                blob("extension/peersus-clipboard/wasm/plugin.json", 'b'),
                blob("extension/peersus-clipboard/plugin.json", 'a'),
            ],
        ] {
            assert_eq!(
                found(tree).unwrap(),
                vec![("clipboard".into(), "b".repeat(40))]
            );
        }
        // A guest-only source has no root manifest at all, and was not listed before.
        assert_eq!(
            found(vec![blob(
                "extension/peersus-control/wasm/plugin.json",
                'c'
            )])
            .unwrap(),
            vec![("control".into(), "c".repeat(40))]
        );
        // A source with only a root manifest is still found.
        assert_eq!(
            found(vec![blob("extension/peersus-test/plugin.json", 'd')]).unwrap(),
            vec![("test".into(), "d".repeat(40))]
        );
        // One id from two directories is still refused, whichever manifests they carry.
        assert!(
            found(vec![
                blob("extension/peersus-upload/wasm/plugin.json", 'e'),
                blob("extension/upload/plugin.json", 'f')
            ])
            .is_err()
        );
        // Deeper files are not manifests.
        assert!(
            found(vec![blob(
                "extension/peersus-upload/wasm/src/plugin.json",
                'g'
            )])
            .unwrap()
            .is_empty()
        );
    }

    fn proof(plugin: &str, updated: bool, sequence: u64) -> Approval {
        use ed25519_dalek::Signer;
        let source = if updated {
            include_bytes!("../tests/fixtures/catalog-v2/catalog.json").as_slice()
        } else {
            include_bytes!("../tests/fixtures/catalog/catalog.json").as_slice()
        };
        let mut catalog: catalog::Catalog = serde_json::from_slice(source).unwrap();
        catalog.sequence = sequence;
        catalog.entries[0].name = plugin.into();
        let entry = &catalog.entries[0];
        let version = entry.version.clone();
        let assets = BTreeMap::from([(entry.archive.clone(), if updated { 20 } else { 10 })]);
        let document = serde_json::to_string(&catalog).unwrap();
        let signature = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .sign(document.as_bytes())
            .to_bytes()
            .to_vec();
        Approval {
            plugin: plugin.into(),
            version,
            document,
            signature,
            assets,
        }
    }

    fn index(approvals: Vec<Approval>) -> Index {
        Index {
            schema: 1,
            approvals,
            sources: BTreeMap::new(),
        }
    }

    fn service(data: &std::path::Path) -> crate::store_service::StoreService {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes();
        crate::store_service::StoreService::new(Some(key), data)
    }

    #[test]
    fn independent_release_versions_block_replay_even_with_another_fresh_plugin() {
        let data = tempfile::tempdir().unwrap();
        let mut store = service(data.path());
        store.begin_refresh().unwrap();
        store
            .finish_discovery(index(vec![
                proof("echo", false, 1),
                proof("other", false, 900),
            ]))
            .unwrap();
        store.begin_refresh().unwrap();
        store
            .finish_discovery(index(vec![
                proof("echo", true, 1),
                proof("other", false, 900),
            ]))
            .unwrap();
        let newest = store.state().listings.clone();
        store.begin_refresh().unwrap();
        assert!(
            store
                .finish_discovery(index(vec![
                    proof("echo", false, 9999),
                    proof("other", true, 10000)
                ]))
                .is_err()
        );
        assert_eq!(store.state().listings, newest);
        let fetch = store
            .begin_install(&PluginId::new("echo").unwrap())
            .unwrap();
        assert_eq!(fetch.archive, "store-asset-20");
        let old_archive = include_bytes!("../tests/fixtures/catalog/echo.tar.gz").to_vec();
        assert!(
            store
                .finish_install(&PluginId::new("echo").unwrap(), old_archive, data.path())
                .is_err()
        );
        let mut restarted = service(data.path());
        assert_eq!(restarted.state().listings, newest);
        restarted.begin_refresh().unwrap();
        assert!(
            restarted
                .finish_discovery(index(vec![proof("echo", false, 99999)]))
                .is_err()
        );
        assert_eq!(restarted.state().listings, newest);
    }

    #[test]
    fn dynamic_cache_is_available_offline_but_cannot_authorize_a_tampered_package() {
        let data = tempfile::tempdir().unwrap();
        let mut store = service(data.path());
        store.begin_refresh().unwrap();
        store
            .finish_discovery(index(vec![proof("echo", false, 1)]))
            .unwrap();
        let expected = store.state().listings.clone();
        let mut restarted = service(data.path());
        assert_eq!(restarted.state().listings, expected);
        restarted.begin_refresh().unwrap();
        restarted.failed("The extension store could not be reached");
        assert_eq!(restarted.state().listings, expected);
        restarted
            .begin_install(&PluginId::new("echo").unwrap())
            .unwrap();
        let archive = include_bytes!("../tests/fixtures/catalog/echo.tar.gz").to_vec();
        assert!(
            restarted
                .finish_install(&PluginId::new("echo").unwrap(), archive, data.path())
                .is_ok()
        );
        restarted.done();
        let mut tampered = proof("echo", false, 1);
        tampered.document = tampered.document.replace("Someone", "Impostor");
        restarted.begin_refresh().unwrap();
        assert!(restarted.finish_discovery(index(vec![tampered])).is_err());
        assert_eq!(restarted.state().listings, expected);
    }

    #[tokio::test]
    async fn unchanged_source_blobs_reuse_approved_proofs_without_refetching_releases() {
        let fetcher = MapFetcher::default();
        let approval = proof("echo", false, 1);
        let blob = "a".repeat(40);
        fetcher.put("store-tree", serde_json::to_vec(&serde_json::json!({
            "truncated": false, "tree": [{"path": "extension/echo/plugin.json", "type": "blob", "sha": blob}]
        })).unwrap());
        fetcher.put(&format!("store-blob-{blob}"), serde_json::to_vec(&serde_json::json!({
            "package": {"name": "Echo", "version": "1.0.0", "author": "Someone", "link": "https://example.org/echo"}
        })).unwrap());
        fetcher.put("store-release-echo--v1.0.0", serde_json::to_vec(&serde_json::json!({
            "draft": false, "prerelease": false, "tag_name": "extension/echo/v1.0.0", "assets": [
                {"id": 1, "name": "approval.json", "size": approval.document.len(), "state": "uploaded"},
                {"id": 2, "name": "approval.sig", "size": 64, "state": "uploaded"},
                {"id": 10, "name": "echo.tar.gz", "size": 1, "state": "uploaded"}
            ]
        })).unwrap());
        fetcher.put("store-asset-1", approval.document.as_bytes().to_vec());
        fetcher.put("store-asset-2", approval.signature);
        let first = discover(&fetcher, None).await.unwrap();
        // The only request that can now succeed is the branch tree check.
        fetcher
            .0
            .lock()
            .unwrap()
            .retain(|name, _| name == "store-tree");
        let second = discover(&fetcher, Some(&first)).await.unwrap();
        assert_eq!(second.approvals[0].document, first.approvals[0].document);
        assert_eq!(second.approvals[0].assets, first.approvals[0].assets);
    }
}
