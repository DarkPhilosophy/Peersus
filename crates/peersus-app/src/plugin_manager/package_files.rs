//! What a package on disk is made of, checked before anything is installed: its id, its files (counted, bounded, no links),
//! reading and copying them, the digest of everything in it, and the private directories it is staged and approved in.
use super::*;

/// The id a package installs under: the name of its directory, which the manifest has no say in.
pub(crate) fn package_id(source: &Path) -> Result<String, Refusal> {
    let metadata =
        fs::symlink_metadata(source).map_err(|_| Refusal("The package could not be read"))?;
    if !metadata.is_dir() {
        return Err(Refusal("Choose a plugin directory, not a file or a link"));
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Refusal("The package's name is not usable"))?;
    peersus_core::plugins::PluginId::new(name)
        .map_err(|_| Refusal("The package's name is not usable"))?;
    Ok(name.to_owned())
}

/// The files that make up the package in `root`, relative and sorted: the ONE place that says what a
/// package is, so that what is shown, what is digested, what is copied and what is installed cannot
/// disagree.
///
/// Without a file list in its manifest that is everything under `root`, as it always was. With one
/// it is the manifest, the program and exactly the files the list names; anything else lying beside
/// them is not part of the package, however much of it there is, and when installing it is not even
/// looked at, so neither its size, nor its number, nor a link among it can refuse the package. A name
/// in the list that is not a plain file of the package refuses it: a package that says it has a file
/// it does not have is not one to install quietly.
///
/// A package with two manifests is refused, as everywhere. A manifest that cannot be read at all
/// names nothing, so the whole package is taken, exactly as before the list existed. That is what
/// keeps a damaged installed package reportable as CHANGED (its digest can still be computed and no
/// longer matches) instead of unmeasurable; refusing a manifest that is not valid is the job of the
/// step that validates it, which comes first on install.
pub(super) fn package_files(root: &Path, purpose: Purpose) -> Result<Vec<PathBuf>, Refusal> {
    let manifest_path = crate::discovery::manifest_file(root);
    // Two manifests is not a package, whatever else is true of it.
    if manifest_path == Err("It has two manifests") {
        return Err(Refusal("It has two manifests"));
    }
    let manifest = match (purpose, crate::discovery::read_package(root)) {
        (Purpose::Install, Err(reason @ crate::discovery::MISSING_EXECUTION)) => {
            return Err(Refusal(reason));
        }
        (_, result) => result.ok(),
    };
    let program = manifest
        .as_ref()
        .map_or(PROGRAM_FILE, |manifest| manifest.program_file())
        .to_owned();
    let named = manifest.and_then(|manifest| manifest.files);
    match (purpose, named) {
        // Installing a package that names its files: only those are looked at.
        (Purpose::Install, Some(named)) => {
            let manifest = manifest_path
                .ok()
                .and_then(|path| path.file_name().map(PathBuf::from))
                .ok_or(Refusal("The package needs a manifest and a program"))?;
            let mut kept = vec![manifest, PathBuf::from(&program)];
            kept.extend(named.iter().map(PathBuf::from));
            kept.sort();
            kept.dedup();
            let mut bytes = 0u64;
            for file in &kept {
                bytes = bytes
                    .checked_add(selected_file(
                        root,
                        file,
                        named.iter().any(|n| Path::new(n) == file),
                    )?)
                    .ok_or(Refusal("The package is too large"))?;
                if bytes > MAX_BYTES {
                    return Err(Refusal("The package is too large"));
                }
            }
            if kept.len() > MAX_FILES {
                return Err(Refusal("The package has too many files"));
            }
            Ok(kept)
        }
        // Everything else looks at everything: installing with no list, and comparing with the
        // record. The list decides what is copied at install and never what is looked at
        // afterwards, so a file added later reads as a change, and a named file that has gone does
        // too: the package reads as CHANGED either way, never as gone and never as untouched.
        (_, _) => scan(root),
    }
}

/// The size of one file the package selects, after checking that nothing on the way to it is a link
/// and that it is a plain file. Only that path is looked at, never what lies beside it. `named` says
/// whether the list named it (and so it is a refusal if it is not there) or it is one of the two
/// files every package must have.
pub(super) fn selected_file(root: &Path, relative: &Path, named: bool) -> Result<u64, Refusal> {
    let missing = if named {
        Refusal("The package names a file it does not have")
    } else {
        Refusal("The package needs a manifest and a program")
    };
    let mut path = root.to_path_buf();
    let parts: Vec<_> = relative.components().collect();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(part) = part else {
            return Err(Refusal("A file of the package is outside it"));
        };
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(|_| missing)?;
        if private_fs::is_link(&metadata) {
            return Err(Refusal("A plugin package cannot contain links"));
        }
        let last = index + 1 == parts.len();
        match (last, metadata.is_file(), metadata.is_dir()) {
            (true, true, _) => return Ok(metadata.len()),
            (false, _, true) => {}
            _ => return Err(missing),
        }
    }
    Err(missing)
}

/// Why the files of a package are being worked out.
#[derive(Clone, Copy)]
pub(super) enum Purpose {
    /// Putting it in the store: it must be what it says it is.
    Install,
    /// Comparing it with the record of what was installed: measure what is there.
    Measure,
}

/// Whether a file at the root of a plugin's folder is one the application wrote there itself, and so is not part of the
/// package: the approval, the switch that turns the plugin off and the mark of an installation from the command line. ONE
/// list, so that measuring and copying cannot disagree about what belongs to the plugin.
pub(super) fn is_the_applications_own(name: &std::ffi::OsStr) -> bool {
    name == crate::plugin_approval::FILE
        || name == crate::plugin_folder::DISABLED_MARKER
        || name == crate::plugin_folder::NEW_INSTALL_MARKER
        || name == crate::plugin_folder::STORE_MARKER
}

pub(super) fn scan(root: &Path) -> Result<Vec<PathBuf>, Refusal> {
    let mut queue = vec![PathBuf::new()];
    let mut out = Vec::new();
    let (mut entries, mut bytes) = (0usize, 0u64);
    while let Some(directory) = queue.pop() {
        let read = fs::read_dir(root.join(&directory))
            .map_err(|_| Refusal("The package could not be read"))?;
        for entry in read {
            let entry = entry.map_err(|_| Refusal("The package could not be read"))?;
            entries += 1;
            if entries > MAX_FILES {
                return Err(Refusal("The package has too many files"));
            }
            let relative = directory.join(entry.file_name());
            // What the application writes into the plugin's folder is its own, not the plugin's: the approval, and the switch
            // that turns it off. It is not part of the package: a package must not be able to bring its own, and writing one
            // must not change what the package is. Only those at the root are the application's; a file of that name
            // further down is the plugin's.
            if directory.as_os_str().is_empty() && is_the_applications_own(&entry.file_name()) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| Refusal("The package could not be read"))?;
            if private_fs::is_link(&metadata) {
                return Err(Refusal("A plugin package cannot contain links"));
            }
            if metadata.is_dir() {
                queue.push(relative);
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(Refusal("The package is too large"))?;
                if bytes > MAX_BYTES {
                    return Err(Refusal("The package is too large"));
                }
                out.push(relative);
            } else {
                return Err(Refusal("A plugin package holds only files and directories"));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Read one file of a package without following a link, up to a limit.
pub(super) fn read_file(path: &Path, limit: u64) -> Result<Vec<u8>, Refusal> {
    let file = private_fs::read_no_follow(path)
        .map_err(|_| Refusal("A file of the package could not be read"))?;
    let metadata = file
        .metadata()
        .map_err(|_| Refusal("A file of the package could not be read"))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(Refusal("A file of the package is not usable"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Refusal("A file of the package could not be read"))?;
    if bytes.len() as u64 > limit {
        return Err(Refusal("A file of the package is too large"));
    }
    Ok(bytes)
}

/// Copy the package into `destination`, making every file private to this user. Whether a file
/// may be run is kept, because that is the one thing about its mode a package decides.
pub(super) fn copy_package(
    source: &Path,
    destination: &Path,
    files: &[PathBuf],
) -> Result<(), Refusal> {
    private_fs::private_folder()
        .create(destination)
        .map_err(|_| Refusal("The package could not be staged"))?;
    #[cfg(target_os = "android")]
    let native_program = crate::discovery::read_package(source)
        .ok()
        .filter(|manifest| {
            matches!(
                manifest.execution,
                peersus_api::manifest::Execution::NativeLibrary { .. }
            )
        })
        .map(|manifest| manifest.program_file().to_owned());
    for file in files {
        if file
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Refusal("A file of the package is outside it"));
        }
        let from = source.join(file);
        let executable = private_fs::is_runnable(
            &fs::symlink_metadata(&from).map_err(|_| Refusal("The package could not be read"))?,
        );
        let bytes = read_file(&from, MAX_BYTES)?;
        let to = destination.join(file);
        if let Some(parent) = to.parent() {
            private_fs::private_folders()
                .create(parent)
                .map_err(|_| Refusal("The package could not be staged"))?;
        }
        let mut out = private_fs::new_file(if executable { 0o700 } else { 0o600 })
            .open(&to)
            .map_err(|_| Refusal("The package could not be staged"))?;
        out.write_all(&bytes)
            .and_then(|()| out.sync_all())
            .map_err(|_| Refusal("The package could not be staged"))?;
        #[cfg(target_os = "android")]
        if native_program
            .as_deref()
            .is_some_and(|program| file == Path::new(program))
        {
            // Only our newly created staging copy changes mode. The user's source is untouched.
            use std::os::unix::fs::PermissionsExt;
            out.set_permissions(fs::Permissions::from_mode(if executable {
                0o500
            } else {
                0o400
            }))
            .map_err(|_| Refusal("The native module could not be made read-only"))?;
        }
    }
    Ok(())
}

/// Hex SHA-256 over every file: its name, its length, its bytes, and whether it can be run, so
/// that renaming, resizing, changing or making a file runnable all change the digest.
pub(super) fn digest(root: &Path, files: &[PathBuf]) -> Result<String, Refusal> {
    let mut hash = Sha256::new();
    for file in files {
        let name = file
            .to_str()
            .ok_or(Refusal("A file name of the package is not usable"))?;
        let bytes = read_file(&root.join(file), MAX_BYTES)?;
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        let metadata = fs::symlink_metadata(root.join(file))
            .map_err(|_| Refusal("A file of the package could not be read"))?;
        hash.update([u8::from(private_fs::is_runnable(&metadata))]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// The digest of what is installed at `path`, or `None` if nothing usable is there.
pub fn destination_digest(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    digest(path, &package_files(path, Purpose::Measure).ok()?).ok()
}

/// The digest of the package in `source`, a directory the person chose, as it is NOW. The same
/// measure as the one the record of installations keeps, so that "what was shown" and "what is
/// installed" are compared with one rule and cannot drift apart.
pub fn package_digest(source: &Path) -> Result<String, Refusal> {
    digest(source, &package_files(source, Purpose::Install)?)
}

/// Whether the package is, at confirmation, the one that was shown: its digest now equals the one
/// taken when it was shown. A package that cannot be read now, or an empty digest on either side,
/// is never a match, so a doubtful case is refused and never installed.
pub(crate) fn still_what_was_shown(shown: &str, now: Result<String, Refusal>) -> bool {
    !shown.is_empty() && now.is_ok_and(|now| now == shown)
}

/// Make `directory` exist and be changeable only by its owner.
pub(super) fn private_directory(directory: &Path) -> Result<(), Refusal> {
    private_fs::private_folders()
        .create(directory)
        .map_err(|_| Refusal("The plugin store could not be made"))?;
    let metadata = fs::symlink_metadata(directory)
        .map_err(|_| Refusal("The plugin store could not be read"))?;
    if !metadata.is_dir() || private_fs::others_can_change(&metadata) {
        return Err(Refusal("The plugin store can be changed by others"));
    }
    Ok(())
}

/// Write the approval for the plugin in `directory`: the program that is there now and what its manifest asks for, by name.
/// Nothing else is approved. Refused (with nothing written) when the manifest cannot be read and accepted or the program
/// cannot be measured, since there is then nothing to say was approved.
pub(crate) fn approve_copy(directory: &Path) -> std::io::Result<()> {
    let refused = |what: &'static str| std::io::Error::other(what);
    let manifest = crate::discovery::read_package(directory).map_err(refused)?;
    crate::plugin_host::check_external_execution(manifest.execution).map_err(refused)?;
    crate::plugin_host::check_manifest(&manifest)
        .map_err(|_| refused("The manifest is not accepted"))?;
    let program = manifest.program_file().to_owned();
    let digest = crate::plugin_approval::program_digest(
        &directory.join(&program),
        crate::plugin_folder::MAX_PROGRAM_BYTES,
    )
    .ok_or_else(|| refused("The program could not be measured"))?;
    crate::plugin_approval::write(
        directory,
        &crate::plugin_approval::Approval {
            program,
            execution: manifest.execution,
            sha256: digest,
            capabilities: manifest.capabilities.keys().cloned().collect(),
            guards: crate::plugin_approval::guards_of(&manifest),
            approved_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        },
    )
}

/// Whether `store` is a real directory of the application's own, and not a link to somewhere else. Read only: it makes
/// nothing, unlike [`private_directory`].
///
/// This is what makes an unconditional removal safe. Removing a plugin in any condition means deleting at
/// `<store>/<id>` without asking what is in it, which is only right if `<store>` is the application's own directory; if
/// it has been turned into a link, the same path names a folder somewhere else.
///
/// Only the store itself is looked at, and what is above it is not. The data directory may sit behind links the application
/// does not control and has no business refusing (on an immutable system `/home` is a link to `/var/home`, which the
/// person's own data is under), and a rule that looked above the store would refuse every removal there. A store that
/// does not exist yet has nothing in it to remove and is not refused here.
pub(super) fn store_is_really_the_store(store: &Path) -> Result<(), Refusal> {
    match fs::symlink_metadata(store) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Refusal("The plugin store could not be read")),
        // Asked first, with its own words: for a link `is_dir` is false too, and the person should be told it is a link.
        Ok(metadata) if private_fs::is_link(&metadata) => {
            Err(Refusal("The plugin store is a link"))
        }
        Ok(metadata) if !metadata.is_dir() => Err(Refusal("The plugin store is not a directory")),
        Ok(_) => Ok(()),
    }
}
