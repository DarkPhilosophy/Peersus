//! Seed and conservatively upgrade the first-party removable APK package.
//! User packages, disabled state, grants and a later removal are preserved.
use peersus_app::plugin_manager::{Manager, Refusal, Standing, destination_digest, package_digest};
use std::{fs, io::ErrorKind, path::Path};

// Exact package shipped before contextual Send/configuration, not merely its program.
const LEGACY_DIGEST: &str = "9fa95fe1f56486083a1774b87afbcbd26c0eadbc2693a2f6425652c6d88f2934";
const MARKER: &str = "bundled-upload-seeded-v2";
const JOURNAL: &str = "bundled-upload-upgrade";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Upgrade {
    retired: String,
    previous: Option<String>,
}

fn exists(path: &Path) -> Result<bool, Refusal> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Refusal("The bundled extension state could not be read")),
    }
}

fn save(path: &Path, bytes: &[u8]) -> Result<(), Refusal> {
    peersus_net::atomic::write(path, bytes)
        .map_err(|_| Refusal("The bundled extension state could not be saved"))
}

pub(crate) fn install_once(
    manager: &Manager,
    scratch: &Path,
    archive: &[u8],
) -> Result<bool, Refusal> {
    let state = manager
        .store()
        .parent()
        .ok_or(Refusal("No extension data directory"))?;
    let marker = state.join(MARKER);
    let journal = state.join(JOURNAL);
    // Startup owns this store: finish rollback before discovery can see a partial upgrade.
    if exists(&journal)? {
        let upgrade: Upgrade = serde_json::from_slice(
            &fs::read(&journal)
                .map_err(|_| Refusal("The extension upgrade could not be recovered"))?,
        )
        .map_err(|_| Refusal("The extension upgrade record is invalid"))?;
        if !upgrade
            .retired
            .strip_prefix(".retired-upload-")
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(Refusal("The extension upgrade record is invalid"));
        }
        let retired = manager.store().join(&upgrade.retired);
        if exists(&retired)? {
            if let Some(previous) = upgrade.previous {
                save(&marker, previous.as_bytes())?;
            } else if exists(&marker)? {
                fs::remove_file(&marker)
                    .map_err(|_| Refusal("The extension upgrade could not be recovered"))?;
            }
            manager.restore("upload", &retired)?;
        }
        fs::remove_file(&journal)
            .map_err(|_| Refusal("The extension upgrade could not be recovered"))?;
    }
    let previous = if exists(&marker)? {
        Some(
            fs::read_to_string(&marker)
                .map_err(|_| Refusal("The bundled extension state could not be read"))?,
        )
    } else {
        None
    };
    let offered = previous.is_some() || exists(&state.join("bundled-upload-seeded-v1"))?;
    let destination = manager.store().join("upload");
    if offered && !exists(&destination)? {
        return Ok(false); // The person removed it; an APK update must not resurrect it.
    }
    let installed_digest = destination_digest(&destination);
    let expected = previous.as_deref().map(str::trim).unwrap_or(LEGACY_DIGEST);
    if exists(&destination)? && (!offered || installed_digest.as_deref() != Some(expected)) {
        if !offered {
            save(&marker, b"user-managed\n")?;
        }
        return Ok(false);
    }
    let source = peersus_app::archive::keep_source(archive, "upload.tar.gz", scratch)
        .map_err(|error| Refusal(error.0))?;
    let package = peersus_app::archive::unpack_in(source.path(), scratch)
        .map_err(|error| Refusal(error.0))?;
    if package.root().file_name().and_then(|name| name.to_str()) != Some("upload") {
        return Err(Refusal("The bundled upload package has the wrong identity"));
    }
    let next = package_digest(package.root())?;
    if installed_digest.as_deref() == Some(&next) {
        save(&marker, next.as_bytes())?;
        return Ok(false);
    }
    if !exists(&destination)? {
        // Ordinary installation writes approval, never runtime file/HTTPS grants.
        manager.install(package.root())?;
        save(&marker, next.as_bytes())?;
        return Ok(true);
    }
    let disabled = match manager
        .list()?
        .into_iter()
        .find(|entry| entry.id == "upload")
        .map(|entry| entry.standing)
    {
        Some(Standing::Ready) => false,
        Some(Standing::Disabled) => true,
        _ => return Ok(false),
    };
    let upgrade = Upgrade {
        retired: format!(".retired-upload-{}", std::process::id()),
        previous,
    };
    save(
        &journal,
        &serde_json::to_vec(&upgrade)
            .map_err(|_| Refusal("The extension upgrade could not be recorded"))?,
    )?;
    let retired = manager.retire("upload")?;
    let result = (|| {
        manager.install(package.root())?;
        if disabled {
            manager.set_disabled("upload", true)?;
        }
        save(&marker, next.as_bytes())?;
        Ok(true)
    })();
    if result.is_err() {
        if let Some(previous) = upgrade.previous {
            save(&marker, previous.as_bytes())?;
        } else if exists(&marker)? {
            fs::remove_file(&marker)
                .map_err(|_| Refusal("The extension upgrade could not be recovered"))?;
        }
        manager.restore("upload", &retired)?;
    }
    // Retain the journal until cleanup completes, so any interrupted replacement rolls back.
    manager.drop_retired(&retired);
    fs::remove_file(&journal)
        .map_err(|_| Refusal("The extension upgrade could not be completed"))?;
    result
}
