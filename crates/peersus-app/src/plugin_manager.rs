//! Installing, listing and removing plugin packages.
//!
//! This follows the plugin manager of the other applications of this family (Attyx, Heresia):
//! each application keeps its own store, `<app data>/plugins/<id>/`, and the same ideas make a
//! package safe. A package is a directory; it is inspected whole, with limits and with no
//! links; a digest of everything in it is recorded when it is installed; and installing means
//! staging a copy, checking the copy, and renaming it into place in one step. What is shared
//! between the applications is that convention, not a directory.
//!
//! A package here is a directory whose name is the plugin's id, holding the manifest and the
//! program the application will start, and optionally files they use:
//!
//! ```text
//! <package>/plugin.json   the manifest
//! <package>/plugin        the program
//! <package>/...           anything else the program needs
//! ```
//!
//! WHAT THIS DECIDES. That a package is one this application would load, and that what is on disk afterwards is what was
//! checked and approved. It does not decide what the plugin may do: that is the user's grant, made later and for each
//! device.
//!
//! THE FOLDER IS THE TRUTH. What is true of a plugin is written in its own folder: its name is the folder's name, what
//! the person approved (the program's digest and what its manifest asks) is `approval.json` inside it, and a switch that
//! turns it off is a marker file inside it. There is no separate record to damage or to disagree: a plugin cannot become
//! something that is neither usable nor removable, and the state of each is worked out from what is there every time
//! (`plugin_state`). A grant belongs to a plugin id; a program replaced on disk after the person decided is no longer what
//! was approved, and the plugin waits to be approved again instead of inheriting what was granted.
//!
//! A process running as this user can rewrite the approval as well as the files, and this does not claim otherwise: it
//! detects a change that was not meant, and does not defend against one that was.
#![cfg(unix)]

use crate::discovery::{PROGRAM_FILE, load_as};
use crate::plugin_host::{PluginSpec, Registry};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

/// Most entries (files and directories) one package may hold.
pub const MAX_FILES: usize = 512;
/// Most bytes all the files of one package may hold together.
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;
/// How long loading at start waits for the store when a command has it. Those commands hold it
/// for milliseconds, so this is far more than needed; it is a bound, not an expectation.
pub const LOAD_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Why something was not done, in words this application chose. Nothing read from a package is
/// repeated, because what a file says is the plugin's own text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal(pub &'static str);

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

/// What installing did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// The package was copied into the store and approved.
    New(String),
    /// The same package was already installed and in good standing; nothing was changed.
    Unchanged(String),
}

/// What is true of one entry of the store: what its folder says, in the application's own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// It is what was approved and may run.
    Ready,
    /// The person switched it off.
    Disabled,
    /// Nobody approved it: put there by hand.
    NeedsApproval,
    /// Something in it is not what was approved.
    Changed,
    /// It cannot be used at all: damaged, unreadable or unsafe.
    Broken,
}

impl From<crate::plugin_state::State> for Standing {
    fn from(state: crate::plugin_state::State) -> Self {
        use crate::plugin_state::State;
        match state {
            State::Ready => Self::Ready,
            State::Disabled => Self::Disabled,
            State::NeedsApproval => Self::NeedsApproval,
            State::Changed => Self::Changed,
            State::Broken(_) => Self::Broken,
        }
    }
}

/// One entry of the store, as `list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub standing: Standing,
}

/// The application's own plugin manager: a store of packages. What is true of each lives in its own folder.
pub struct Manager {
    store: PathBuf,
    lock: PathBuf,
}

impl Manager {
    /// A manager for the application whose data directory is `data`: packages go in `<data>/plugins/`, each with its own
    /// approval inside it.
    pub fn new(data: &Path) -> Self {
        Self {
            store: data.join("plugins"),
            lock: data.join("installed.lock"),
        }
    }

    /// Whether an application is running over this data directory, and so will not see a
    /// change to the store until it is restarted.
    ///
    /// Decided by the lock the application holds while it runs, the same one it takes to
    /// become the owner, and not by whether its control socket exists: a socket can be left
    /// behind by a process that died, and would claim an owner that is not there.
    pub fn owner_is_running(data: &Path) -> bool {
        // Taking the lock and letting it go at once is harmless: it is only held to ask.
        // On Android the owner is this very application: asking whether one is running would only ask
        // itself, and the single-instance lock does not exist there. Elsewhere the lock decides.
        #[cfg(target_os = "android")]
        {
            let _ = data;
            true
        }
        #[cfg(not(target_os = "android"))]
        matches!(crate::instance::Instance::acquire(data), Ok(None))
    }

    /// Where installed packages live, which is where `discover` looks.
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// Install the package in `source`: a directory named for the plugin's id.
    pub fn install(&self, source: &Path) -> Result<Installed, Refusal> {
        let id = package_id(source)?;
        let files = package_files(source, Purpose::Install)?;
        let program = crate::discovery::read_package(source)
            .map(|manifest| manifest.program_file().to_owned())
            .unwrap_or_else(|_| PROGRAM_FILE.to_owned());
        // Look at what is being offered before the store is touched at all.
        if !files.iter().any(|file| crate::discovery::MANIFEST_FILES.iter().any(|name| file == Path::new(name)))
            || !files.iter().any(|file| file == Path::new(&program))
        {
            return Err(Refusal("The package needs a manifest and a program"));
        }
        let _guard = self.lock()?;
        private_directory(self.store.parent().ok_or(Refusal("No data directory"))?)?;
        private_directory(&self.store)?;
        let destination = self.store.join(&id);
        let staging = self
            .store
            .join(format!(".install-{id}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&staging);
        let result = (|| {
            copy_package(source, &staging, &files)?;
            // The copy is held to the rules that will be applied when it is loaded, so that
            // nothing is installed that would never run.
            let spec = load_as(&self.store, &staging, &id).map_err(Refusal)?;
            // And to the rules the registry applies when plugins are loaded together: the
            // manifest's own validation, actions that can be reached, `receive` behind a
            // permission, and no two plugins offering the same service. `load_as` only reads
            // the manifest; a manifest that parses can still be one the application refuses.
            self.check_with_installed(spec)?;
            let digest = digest(&staging, &package_files(&staging, Purpose::Install)?)?;
            if fs::symlink_metadata(&destination).is_ok() {
                // The same package again is not an error and changes nothing, but only when what is installed IS that
                // package and is in good standing; a different one under the same id is refused, never written over.
                let same = destination_digest(&destination).as_deref() == Some(&digest)
                    && matches!(
                        crate::plugin_folder::look(&destination).state,
                        crate::plugin_state::State::Ready | crate::plugin_state::State::Disabled
                    );
                return if same {
                    Ok(Installed::Unchanged(id.clone()))
                } else {
                    Err(Refusal("Something is already installed under this name"))
                };
            }
            if fs::read_dir(&self.store)
                .map(|read| read.filter_map(Result::ok).filter(|e| !e.file_name().to_string_lossy().starts_with('.')).count())
                .unwrap_or(0)
                >= crate::discovery::MAX_PLUGINS
            {
                return Err(Refusal("Too many plugins are installed"));
            }
            // The person was shown what this package asks for before confirming, so installing is the approval of exactly
            // that: the program that is here, and what its manifest asks, are written into the copy BEFORE it is moved into
            // place, so that the plugin is never at its name without the approval that lets it run. Whatever cannot be
            // written is a package that is not installed.
            approve_copy(&staging).map_err(|_| Refusal("The approval could not be written"))?;
            fs::rename(&staging, &destination)
                .map_err(|_| Refusal("The package could not be put in place"))?;
            Ok(Installed::New(id.clone()))
        })();
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// Every plugin in the store, with what its folder says of it.
    ///
    /// The folder is the truth, so what is on disk is listed whatever state it is in: a plugin that is changed, broken, or
    /// that nobody approved is listed as that, and can be removed. Nothing here can fail because of one damaged plugin.
    pub fn list(&self) -> Result<Vec<Entry>, Refusal> {
        let _guard = self.lock()?;
        let mut entries = Vec::new();
        if let Ok(read) = fs::read_dir(&self.store) {
            for entry in read.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let standing = crate::plugin_folder::look(&self.store.join(&name)).state.into();
                entries.push(Entry { id: name, standing });
            }
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    /// The plugins the application will actually run: those that were installed AND are still
    /// exactly what was installed.
    ///
    /// This is the one way plugins are loaded. `discover` only reads a directory and applies
    /// the rules for what a package may contain; it knows nothing about what was approved.
    /// Here the record is the approval, and three things are refused, each by name:
    ///
    /// * a directory nobody installed (no record), such as one put in the store by hand;
    /// * an installed plugin whose files are no longer what was recorded, whatever changed:
    ///   the program, a file it uses, or only the words a person read when deciding;
    /// * everything, if the record itself cannot be read. If what was approved cannot be
    ///   known, nothing is treated as approved: falling back to loading whatever is on disk
    ///   would turn a damaged record into permission for everything.
    ///
    /// One plugin being refused never stops another. The digest is a net for mistakes, partial
    /// edits and hand-placed directories; a process running as this user can rewrite the
    /// record as well as the files, and this does not claim otherwise.
    pub fn load(&self) -> crate::discovery::Discovered {
        self.load_waiting(LOAD_PATIENCE)
    }

    /// [`Manager::load`], waiting up to `patience` for the store instead of the usual time.
    pub fn load_waiting(&self, patience: std::time::Duration) -> crate::discovery::Discovered {
        use crate::discovery::{Discovered, Problem};
        let problem = |plugin: &str, reason: &'static str| Problem {
            plugin: plugin.to_owned(),
            reason,
        };
        let mut loaded = Discovered::default();
        let Ok(_guard) = self.lock_waiting(patience) else {
            loaded
                .problems
                .push(problem("plugins", "The plugin store is in use"));
            return loaded;
        };
        // The folder is the truth: every directory in the store is looked at, and its own state decides whether it runs,
        // whatever the record of installations says or has become. If the store cannot be listed at all, nothing is known
        // about anyone and `installed` stays `None`, so that no permission is ever forgotten for a store that was only
        // unreachable. An absent store is that too: it may be an unmounted disk, and a first run has nothing to forget.
        let Ok(read) = fs::read_dir(&self.store) else {
            return loaded;
        };
        let mut names: Vec<String> = read
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            // What starts with a dot is the store's own (an install in progress, a package retired).
            .filter(|name| !name.starts_with('.'))
            .collect();
        names.sort();
        // Who is installed is KNOWN now: every folder in the store with a name a plugin may have, loaded or not.
        loaded.installed = Some(
            names
                .iter()
                .filter_map(|name| peersus_core::plugins::PluginId::new(name.clone()).ok())
                .collect(),
        );
        for name in names {
            let Ok(id) = peersus_core::plugins::PluginId::new(name.clone()) else {
                loaded.problems.push(problem(&name, "Its name is not usable"));
                continue;
            };
            let path = self.store.join(&name);
            let looked = crate::plugin_folder::look(&path);
            use crate::plugin_state::State;
            use peersus_core::model::Standing;
            let held = |standing: Standing, why: Option<&'static str>| crate::discovery::HeldBack {
                id: id.clone(),
                standing,
                // What it declares is shown when it could be read; a broken one has nothing to show.
                manifest: crate::discovery::read_package(&path).ok().unwrap_or_default(),
                why,
                // Measured now, so that approving later is bound to exactly the program that is shown.
                program: crate::discovery::read_package(&path).ok().and_then(|manifest| {
                    crate::plugin_approval::program_digest(
                        &path.join(manifest.program_file()),
                        crate::plugin_folder::MAX_PROGRAM_BYTES,
                    )
                }),
            };
            match looked.state {
                State::Ready | State::Disabled => match load_as(&self.store, &path, &name) {
                    Ok(spec) => {
                        if loaded.registry.register(spec).is_err() {
                            // Ready on its own, and clashing with another plugin that was registered first (an id or a
                            // service two of them offer). It is not run, and it is not invisible.
                            loaded.problems.push(problem(&name, "It could not be registered"));
                            loaded.held_back.push(held(Standing::Broken, Some("It clashes with another plugin")));
                        } else if looked.state == State::Disabled {
                            // A plugin the person switched off is loaded and listed like any other, so the window can
                            // show it and offer to switch it on; it is only noted here, and not started.
                            loaded.disabled.insert(id);
                        }
                    }
                    Err(reason) => {
                        loaded.problems.push(problem(&name, reason));
                        loaded.held_back.push(held(Standing::Broken, Some("It could not be loaded")));
                    }
                },
                State::NeedsApproval => loaded.held_back.push(held(Standing::NeedsApproval, None)),
                State::Changed => loaded.held_back.push(held(Standing::Changed, None)),
                State::Broken(why) => {
                    loaded.problems.push(problem(&name, why.words()));
                    loaded.held_back.push(held(Standing::Broken, Some(why.words())));
                }
            }
        }
        loaded
    }

    /// Remove a plugin, in whatever condition it is in. The store is the application's own directory, so what is in it
    /// is the application's to delete, and a plugin must never become something that can be neither used nor removed:
    /// a changed one, one nobody approved (put there by hand), and a broken one are all removed. What is refused is a
    /// name that is not a plugin's (it could point out of the store) and one that has no folder in the store.
    pub fn remove(&self, id: &str) -> Result<(), Refusal> {
        // Only the name of a plugin: never a path, never a hidden entry (those are the store's own, `.retired-*`).
        if peersus_core::plugins::PluginId::new(id).is_err() || id.starts_with('.') {
            return Err(Refusal("That is not the name of a plugin"));
        }
        let _guard = self.lock()?;
        // Deleting without asking what is in a plugin is only right if the store is the application's own directory: if it,
        // or a directory inside the data one on its way, has been turned into a link, the same path names a folder
        // somewhere else, and nothing is deleted.
        store_is_really_the_store(&self.store)?;
        let path = self.store.join(id);
        if fs::symlink_metadata(&path).is_err() {
            return Err(Refusal("No such plugin is installed"));
        }
        // Renamed away first, so that a failure part way never leaves half a package at the name the application
        // loads from. A link in the store is renamed as a link and removed as a link: neither follows it, so what
        // it points to is never touched (two tests hold this, since it is what a link put there by hand relies on).
        let retired = self
            .store
            .join(format!(".retired-{id}-{}", std::process::id()));
        fs::rename(&path, &retired).map_err(|_| Refusal("The package could not be taken out of use"))?;
        let _ = fs::remove_dir_all(&retired);
        Ok(())
    }

    /// Take an installed package out of use but KEEP it, and say where: it is no longer at its name and is not loaded.
    /// [`Manager::restore`] puts it back exactly, [`Manager::drop_retired`] lets it go for good. This is what replacing a
    /// plugin is made of, so that a replacement that cannot be completed can be undone. The approval and the switch live
    /// inside the folder, so they go and come back with it: nothing else is kept, and nothing is held in memory.
    pub fn retire(&self, id: &str) -> Result<PathBuf, Refusal> {
        if peersus_core::plugins::PluginId::new(id).is_err() || id.starts_with('.') {
            return Err(Refusal("That is not the name of a plugin"));
        }
        let _guard = self.lock()?;
        store_is_really_the_store(&self.store)?;
        let path = self.store.join(id);
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(Refusal("No such plugin is installed"));
        }
        // Only a plugin that is in good standing is replaced: one that was changed, or that nobody approved, is the
        // person's to look at first, and is not quietly set aside.
        let looked = crate::plugin_folder::look(&path);
        if !matches!(looked.state, crate::plugin_state::State::Ready | crate::plugin_state::State::Disabled) {
            return Err(Refusal("Its files are not what was installed; not replacing them"));
        }
        let kept = self.store.join(format!(".retired-{id}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&kept);
        fs::rename(&path, &kept).map_err(|_| Refusal("The package could not be taken out of use"))?;
        Ok(kept)
    }

    /// Put a package that was retired back as it was: its files, its approval and its switch, at its name.
    pub fn restore(&self, id: &str, retired: &Path) -> Result<(), Refusal> {
        if peersus_core::plugins::PluginId::new(id).is_err() || id.starts_with('.') {
            return Err(Refusal("That is not the name of a plugin"));
        }
        let _guard = self.lock()?;
        store_is_really_the_store(&self.store)?;
        // What was retired is a folder of this store that was set aside by `retire`, and nothing else: never a path
        // from somewhere else, and it is still what it was (ready, or switched off) and not something altered meanwhile.
        let in_store = retired.parent() == Some(self.store.as_path())
            && retired
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!(".retired-{id}-")));
        if !in_store || !fs::symlink_metadata(retired).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(Refusal("Nothing was retired under that name"));
        }
        let looked = crate::plugin_folder::look(retired);
        if !matches!(looked.state, crate::plugin_state::State::Ready | crate::plugin_state::State::Disabled) {
            return Err(Refusal("What was retired is not what it was"));
        }
        let path = self.store.join(id);
        // Whatever a failed install left at the name is cleared away first.
        let _ = fs::remove_dir_all(&path);
        fs::rename(retired, &path).map_err(|_| Refusal("The package could not be put back"))
    }

    /// Let a retired package go for good.
    pub fn drop_retired(&self, retired: &Path) {
        let _ = fs::remove_dir_all(retired);
    }

    /// Switch the plugin `id` off or on. A plugin that is switched off stays installed and keeps what it was granted, and
    /// is not started at the next start. The choice is a marker file in the plugin's own folder, so it lasts across
    /// restarts and is as much the folder's as everything else about the plugin: no record holds it and none can lose it.
    /// Asking about a plugin that has no folder is refused.
    pub fn set_disabled(&self, id: &str, disabled: bool) -> Result<(), Refusal> {
        if peersus_core::plugins::PluginId::new(id).is_err() || id.starts_with('.') {
            return Err(Refusal("That is not the name of a plugin"));
        }
        let _guard = self.lock()?;
        store_is_really_the_store(&self.store)?;
        let folder = self.store.join(id);
        // A link in place of the folder is never followed: what it points to is not the store's.
        if !fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(Refusal("No such plugin is installed"));
        }
        let marker = folder.join(crate::plugin_folder::DISABLED_MARKER);
        if disabled {
            // Created if it is not there, and a marker that already is stays: switching off twice is switching off once.
            match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&marker) {
                Ok(_) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(_) => Err(Refusal("The plugin could not be switched off")),
            }
        } else {
            match fs::remove_file(&marker) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(Refusal("The plugin could not be switched on")),
            }
        }
    }

    /// Approve what is in the folder of `id` now, which the person has just been shown: the program and what its manifest asks
    /// for. `shown` is the digest of the program as the application measured it when it showed the plugin; if the program is
    /// not that any more, nothing is approved, since what the person agreed to is not what is there.
    ///
    /// Only a plugin that is waiting for approval or that has changed can be approved. One that is already ready has nothing
    /// to approve, and one that is broken is not something to approve at all: it can only be removed.
    pub fn approve(&self, id: &str, shown: &str) -> Result<(), Refusal> {
        if peersus_core::plugins::PluginId::new(id).is_err() || id.starts_with('.') {
            return Err(Refusal("That is not the name of a plugin"));
        }
        let _guard = self.lock()?;
        store_is_really_the_store(&self.store)?;
        let folder = self.store.join(id);
        if !fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(Refusal("No such plugin is installed"));
        }
        if !matches!(
            crate::plugin_folder::look(&folder).state,
            crate::plugin_state::State::NeedsApproval | crate::plugin_state::State::Changed
        ) {
            return Err(Refusal("There is nothing to approve for this plugin"));
        }
        let now = crate::discovery::read_package(&folder)
            .ok()
            .and_then(|manifest| {
                crate::plugin_approval::program_digest(
                    &folder.join(manifest.program_file()),
                    crate::plugin_folder::MAX_PROGRAM_BYTES,
                )
            });
        if shown.is_empty() || now.as_deref() != Some(shown) {
            return Err(Refusal("The plugin is not what was shown; look at it again"));
        }
        approve_copy(&folder).map_err(|_| Refusal("The approval could not be written"))
    }

    /// Register `candidate` in a registry holding what is already installed, so it gets the
    /// refusals the application would give it at start.
    ///
    /// A plugin installed earlier that can no longer be loaded is left out: it is reported by
    /// `list`, and must not stop an unrelated plugin from being installed. The candidate's own
    /// id is left out too, because installing the same package again is handled by the caller
    /// and is not a clash with itself.
    fn check_with_installed(&self, candidate: PluginSpec) -> Result<(), Refusal> {
        let mut registry = Registry::default();
        if let Ok(read) = fs::read_dir(&self.store) {
            let mut names: Vec<String> = read
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| !name.starts_with('.') && name != candidate.id.as_str())
                .collect();
            names.sort();
            for name in names {
                if let Ok(spec) = load_as(&self.store, &self.store.join(&name), &name) {
                    let _ = registry.register(spec);
                }
            }
        }
        registry.register(candidate).map_err(|error| {
            use crate::plugin_host::RegistryError::*;
            Refusal(match error {
                Duplicate => "Another plugin already has this name",
                ServiceConflict => "It offers a service another plugin already offers",
                Manifest => "Its manifest is not valid",
                ActionUnreachable => "One of its actions could never be reached",
                ReceiveWithoutCapability => "It takes things in without a permission to guard them",
                UnsupportedPlatform => "It does not support this operating system",
            })
        })
    }

    /// Take the store's lock, waiting up to `patience` for whoever holds it to let go.
    ///
    /// Everything that holds it holds it for milliseconds, and the operating system releases
    /// it if the holder dies, so a short wait is nearly always enough; the bound is for a
    /// holder that is stuck, which must never be able to hang the start of the application.
    fn lock_waiting(&self, patience: std::time::Duration) -> Result<fs::File, Refusal> {
        let deadline = std::time::Instant::now() + patience;
        loop {
            match self.lock() {
                Ok(file) => return Ok(file),
                Err(refusal)
                    if refusal.0 == "The plugin store is being changed by another process"
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(refusal) => return Err(refusal),
            }
        }
    }

    fn lock(&self) -> Result<fs::File, Refusal> {
        private_directory(self.lock.parent().ok_or(Refusal("No data directory"))?)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&self.lock)
            .map_err(|_| Refusal("The plugin store could not be locked"))?;
        // The same `flock(LOCK_EX | LOCK_NB)` that `fs2` made, taken through `rustix`, which is on every Unix
        // this builds for, Android included. The lock is held for as long as the file is open.
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| Refusal("The plugin store is being changed by another process"))?;
        Ok(file)
    }
}

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
fn package_files(root: &Path, purpose: Purpose) -> Result<Vec<PathBuf>, Refusal> {
    let manifest_path = crate::discovery::manifest_file(root);
    // Two manifests is not a package, whatever else is true of it.
    if manifest_path == Err("It has two manifests") {
        return Err(Refusal("It has two manifests"));
    }
    let manifest = crate::discovery::read_package(root).ok();
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
                    .checked_add(selected_file(root, file, named.iter().any(|n| Path::new(n) == file))?)
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
fn selected_file(root: &Path, relative: &Path, named: bool) -> Result<u64, Refusal> {
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
        if metadata.file_type().is_symlink() {
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
enum Purpose {
    /// Putting it in the store: it must be what it says it is.
    Install,
    /// Comparing it with the record of what was installed: measure what is there.
    Measure,
}

/// Whether a file at the root of a plugin's folder is one the application wrote there itself, and so is not part of the
/// package: the approval and the switch that turns the plugin off. ONE list, so that measuring and copying cannot disagree
/// about what belongs to the plugin.
fn is_the_applications_own(name: &std::ffi::OsStr) -> bool {
    name == crate::plugin_approval::FILE || name == crate::plugin_folder::DISABLED_MARKER
}

fn scan(root: &Path) -> Result<Vec<PathBuf>, Refusal> {
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
            if metadata.file_type().is_symlink() {
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
fn read_file(path: &Path, limit: u64) -> Result<Vec<u8>, Refusal> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
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
fn copy_package(source: &Path, destination: &Path, files: &[PathBuf]) -> Result<(), Refusal> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(destination)
        .map_err(|_| Refusal("The package could not be staged"))?;
    #[cfg(target_os = "android")]
    let native_program = crate::discovery::read_package(source).ok()
        .filter(|manifest| matches!(manifest.execution, peersus_plugin_api::manifest::Execution::NativeLibrary { .. }))
        .map(|manifest| manifest.program_file().to_owned());
    for file in files {
        if file
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Refusal("A file of the package is outside it"));
        }
        let from = source.join(file);
        let executable = fs::symlink_metadata(&from)
            .map_err(|_| Refusal("The package could not be read"))?
            .permissions()
            .mode()
            & 0o111
            != 0;
        let bytes = read_file(&from, MAX_BYTES)?;
        let to = destination.join(file);
        if let Some(parent) = to.parent() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(parent)
                .map_err(|_| Refusal("The package could not be staged"))?;
        }
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if executable { 0o700 } else { 0o600 })
            .open(&to)
            .map_err(|_| Refusal("The package could not be staged"))?;
        out.write_all(&bytes)
            .and_then(|()| out.sync_all())
            .map_err(|_| Refusal("The package could not be staged"))?;
        #[cfg(target_os = "android")]
        if native_program.as_deref().is_some_and(|program| file == Path::new(program)) {
            // Only our newly created staging copy changes mode. The user's source is untouched.
            out.set_permissions(fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }))
                .map_err(|_| Refusal("The native module could not be made read-only"))?;
        }
    }
    Ok(())
}

/// Hex SHA-256 over every file: its name, its length, its bytes, and whether it can be run, so
/// that renaming, resizing, changing or making a file runnable all change the digest.
fn digest(root: &Path, files: &[PathBuf]) -> Result<String, Refusal> {
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
        let mode = fs::symlink_metadata(root.join(file))
            .map_err(|_| Refusal("A file of the package could not be read"))?
            .permissions()
            .mode();
        hash.update([u8::from(mode & 0o111 != 0)]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// The digest of what is installed at `path`, or `None` if nothing usable is there.
fn destination_digest(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    digest(path, &package_files(path, Purpose::Measure).ok()?).ok()
}

/// The digest of the package in `source`, a directory the person chose, as it is NOW. The same
/// measure as the one the record of installations keeps, so that "what was shown" and "what is
/// installed" are compared with one rule and cannot drift apart.
pub(crate) fn package_digest(source: &Path) -> Result<String, Refusal> {
    digest(source, &package_files(source, Purpose::Install)?)
}

/// Whether the package is, at confirmation, the one that was shown: its digest now equals the one
/// taken when it was shown. A package that cannot be read now, or an empty digest on either side,
/// is never a match, so a doubtful case is refused and never installed.
pub(crate) fn still_what_was_shown(shown: &str, now: Result<String, Refusal>) -> bool {
    !shown.is_empty() && now.is_ok_and(|now| now == shown)
}

/// Make `directory` exist and be changeable only by its owner.
fn private_directory(directory: &Path) -> Result<(), Refusal> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(directory)
        .map_err(|_| Refusal("The plugin store could not be made"))?;
    let metadata = fs::symlink_metadata(directory)
        .map_err(|_| Refusal("The plugin store could not be read"))?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
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
    crate::plugin_host::check_manifest(&manifest).map_err(|_| refused("The manifest is not accepted"))?;
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
fn store_is_really_the_store(store: &Path) -> Result<(), Refusal> {
    match fs::symlink_metadata(store) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Refusal("The plugin store could not be read")),
        // Asked first, with its own words: for a link `is_dir` is false too, and the person should be told it is a link.
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Refusal("The plugin store is a link")),
        Ok(metadata) if !metadata.is_dir() => Err(Refusal("The plugin store is not a directory")),
        Ok(_) => Ok(()),
    }
}

/// Forget what was granted to plugins that are no longer installed, on every device and outright.
///
/// Run at start, before anything is registered. Who has been removed is read from the RECORD OF
/// INSTALLATIONS and not from what happened to load: a plugin can be installed and still not
/// load, for a reason that passes (the store was busy, its files are being repaired), and its
/// consent must outlive that. When the record could not be read nothing is known about anyone,
/// and nothing is forgotten.
///
/// This is cleanup and NOT the whole defence. A plugin removed and a different package
/// installed under the same id BEFORE the next start is installed at that start, so it is kept
/// here and inherits what the first was granted. Closing that needs consent tied to one
/// installation and not to a name; see the open item in the notes.
pub fn forget_removed(
    found: &crate::discovery::Discovered,
    devices: &mut peersus_net::peers::Peers,
    standalone: &mut peersus_net::grants::StandaloneGrants,
) -> std::io::Result<usize> {
    let Some(installed) = &found.installed else {
        return Ok(0);
    };
    Ok(devices.retain_plugins(installed)? + standalone.retain_plugins(installed)?)
}

/// Forget what ONE plugin was granted, on every device and outright, now.
///
/// This is what removing a plugin does in the same step as taking its files away: the owner is the
/// only writer of these two records, so it can do it while running, and a package installed
/// afterwards under the same name starts with nothing instead of inheriting what the first was
/// granted. Everyone else is kept: it is `forget_removed` for a set of one.
pub fn forget_one(
    plugin: &peersus_core::plugins::PluginId,
    devices: &mut peersus_net::peers::Peers,
    standalone: &mut peersus_net::grants::StandaloneGrants,
) -> std::io::Result<usize> {
    Ok(devices.forget_plugin(plugin)? + standalone.forget_plugin(plugin)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::MANIFEST_FILE;

    const GOOD: &str = r#"{
        "manifest_version": 1,
        "capabilities": {"thing.use": "Lets the thing do what it does."},
        "imports": [],
        "exports": [{
            "service": "thing.service", "version": 1, "description": "A test service.",
            "consumers": ["host"],
            "methods": {"go": {"capability": "thing.use", "description": "Goes."}},
            "events": {}
        }]
    }"#;

    /// A package: a directory named `name` holding a manifest and a program.
    fn package(parent: &Path, name: &str, manifest: &str, program: &[u8], mode: u32) -> PathBuf {
        let directory = parent.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(MANIFEST_FILE), manifest).unwrap();
        fs::write(directory.join(PROGRAM_FILE), program).unwrap();
        fs::set_permissions(
            directory.join(PROGRAM_FILE),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        directory
    }

    /// `GOOD`, written as TOML.
    const GOOD_TOML: &str = r#"
        manifest_version = 1
        imports = []

        [capabilities]
        "thing.use" = "Lets the thing do what it does."

        [[exports]]
        service = "thing.service"
        version = 1
        description = "A test service."
        consumers = ["host"]

        [exports.methods.act]
        capability = "thing.use"
        description = "Acts."

        [exports.events]
    "#;

    /// A package whose manifest is `plugin.toml`, with a runnable program.
    fn package_toml(parent: &Path, name: &str, manifest: &str) -> PathBuf {
        let directory = parent.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("plugin.toml"), manifest).unwrap();
        fs::write(directory.join(PROGRAM_FILE), b"x").unwrap();
        fs::set_permissions(directory.join(PROGRAM_FILE), fs::Permissions::from_mode(0o755)).unwrap();
        directory
    }

    #[test]
    fn a_package_whose_manifest_is_toml_installs_and_is_listed_as_intact() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package_toml(parent.path(), "thing", GOOD_TOML);
        let Ok(Installed::New(_)) = manager.install(&source) else { panic!("a TOML package was not installed") };
        let installed = manager.store().join("thing");
        assert!(installed.join("plugin.toml").is_file() && installed.join(PROGRAM_FILE).is_file());
        assert!(!installed.join("plugin.json").exists());
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
        // And installing it again changes nothing, as for any package.
        assert!(matches!(manager.install(&source), Ok(Installed::Unchanged(_))));
    }

    #[test]
    fn a_toml_package_that_names_its_files_installs_only_those_and_brings_no_second_manifest() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let manifest = format!("files = [\"settings.toml\"]\n{GOOD_TOML}");
        let source = package_toml(parent.path(), "thing", &manifest);
        fs::write(source.join("settings.toml"), b"t").unwrap();
        fs::write(source.join("README.md"), b"r").unwrap();
        let Ok(Installed::New(_)) = manager.install(&source) else { panic!("not installed") };
        let installed = manager.store().join("thing");
        for named in ["plugin.toml", "plugin", "settings.toml"] {
            assert!(installed.join(named).is_file(), "{named} was not installed");
        }
        assert!(!installed.join("README.md").exists());
        // The package as installed is one the application loads: exactly one manifest.
        assert!(crate::discovery::manifest_file(&installed).is_ok());
    }

    #[test]
    fn a_package_with_both_manifests_is_refused_when_installing_and_nothing_is_installed() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package_toml(parent.path(), "thing", GOOD_TOML);
        fs::write(source.join("plugin.json"), GOOD).unwrap();
        assert!(manager.install(&source).is_err());
        assert!(manager.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(manager.store()).map(|d| d.count()).unwrap_or(0), 0);
        // And what is shown is refused the same way, so confirming is never offered.
        assert!(package_digest(&source).is_err());
    }

    /// An application data directory that only its owner can change.
    fn data() -> tempfile::TempDir {
        let data = tempfile::tempdir().unwrap();
        fs::set_permissions(data.path(), fs::Permissions::from_mode(0o700)).unwrap();
        data
    }

    fn ids(entries: &[Entry]) -> Vec<(&str, Standing)> {
        entries
            .iter()
            .map(|e| (e.id.as_str(), e.standing))
            .collect()
    }

    #[test]
    fn a_good_package_is_installed_recorded_and_then_loaded_by_discovery() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", GOOD, b"#!/bin/sh\nexit 0\n", 0o755);
        let Ok(Installed::New(record)) = manager.install(&source) else {
            panic!("not installed");
        };
        assert_eq!(record, "thing");
        assert!(crate::plugin_approval::read(&manager.store().join("thing")).is_some_and(|a| a.sha256.len() == 64));
        // Not just that files exist, but that the application would load them.
        let found = crate::discovery::discover(manager.store());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(found.registry.iter().count(), 1);
        assert_eq!(
            fs::read(manager.store().join("thing").join(PROGRAM_FILE)).unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
    }

    #[test]
    fn a_named_file_reached_through_a_link_is_refused_and_so_is_a_named_link() {
        // `lib` is a link to somewhere else: `lib/a.so` would be a file that is not the package's.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let outside = parent.path().join("elsewhere");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("a.so"), b"not the package's own").unwrap();

        let through = package(parent.path(), "through", &manifest_naming(&["lib/a.so"]), b"x", 0o755);
        std::os::unix::fs::symlink(&outside, through.join("lib")).unwrap();
        assert_eq!(
            manager.install(&through).unwrap_err(),
            Refusal("A plugin package cannot contain links")
        );

        // The named file is itself a link.
        let named = package(parent.path(), "named", &manifest_naming(&["a.so"]), b"x", 0o755);
        std::os::unix::fs::symlink(outside.join("a.so"), named.join("a.so")).unwrap();
        assert_eq!(
            manager.install(&named).unwrap_err(),
            Refusal("A plugin package cannot contain links")
        );
        assert!(manager.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(manager.store()).map(|d| d.count()).unwrap_or(0), 0);
    }

    /// A package whose manifest names its program `peersus-thing`, with that file as the program.
    fn package_named(parent: &Path, name: &str, files: Option<&[&str]>) -> PathBuf {
        let mut manifest = GOOD.replacen('{', "{\"program\":\"peersus-thing\",", 1);
        if let Some(files) = files {
            let list: Vec<String> = files.iter().map(|f| format!("{f:?}")).collect();
            manifest = manifest.replacen('{', &format!("{{\"files\":[{}],", list.join(",")), 1);
        }
        let directory = parent.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(MANIFEST_FILE), manifest).unwrap();
        fs::write(directory.join("peersus-thing"), b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(directory.join("peersus-thing"), fs::Permissions::from_mode(0o755)).unwrap();
        directory
    }

    #[test]
    fn a_package_that_names_its_program_installs_it_under_that_name_and_it_is_measured_by_it() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package_named(parent.path(), "thing", None);
        let Ok(Installed::New(_)) = manager.install(&source) else { panic!("not installed") };
        let installed = manager.store().join("thing");
        assert!(installed.join("peersus-thing").is_file(), "the program was not installed under its name");
        assert!(!installed.join("plugin").exists());
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
        // Replacing the program after the fact is noticed, as for any package.
        fs::write(installed.join("peersus-thing"), b"another").unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Changed)]);
    }

    #[test]
    fn a_package_that_names_a_program_it_does_not_hold_is_refused_before_the_store_is_touched() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        // A file called `plugin` is there, but the manifest says the program is another file.
        let source = package(parent.path(), "thing", &GOOD.replacen('{', "{\"program\":\"peersus-thing\",", 1), b"x", 0o755);
        assert!(manager.install(&source).is_err());
        assert!(manager.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(manager.store()).map(|d| d.count()).unwrap_or(0), 0);
    }

    #[test]
    fn a_package_that_names_its_program_and_its_files_installs_both_and_leaves_the_rest() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package_named(parent.path(), "thing", Some(&["settings.toml"]));
        fs::write(source.join("settings.toml"), b"t").unwrap();
        fs::write(source.join("README.md"), b"r").unwrap();
        let Ok(Installed::New(_)) = manager.install(&source) else { panic!("not installed") };
        let installed = manager.store().join("thing");
        for named in ["plugin.json", "peersus-thing", "settings.toml"] {
            assert!(installed.join(named).is_file(), "{named} was not installed");
        }
        assert!(!installed.join("README.md").exists());
    }

    #[test]
    fn removing_a_plugin_forgets_what_it_was_granted_in_the_same_step_and_only_that_plugin() {
        use peersus_core::plugins::{Grants, PluginId};
        let dir = tempfile::tempdir().unwrap();
        let mut standalone =
            peersus_net::grants::StandaloneGrants::open(dir.path().join("grants.json")).unwrap();
        let mut peers = peersus_net::peers::Peers::open(dir.path().join("peers.json")).unwrap();
        let first = PluginId::new("first").unwrap();
        let second = PluginId::new("second").unwrap();
        // A set of nothing is stored as nothing, so each is granted something real.
        let some = |owner: &PluginId| {
            Grants::new([peersus_plugin_api::permission::Permission::new(owner.as_str(), "thing.use").unwrap()])
                .unwrap()
        };
        standalone.set(first.clone(), some(&first)).unwrap();
        standalone.set(second.clone(), some(&second)).unwrap();
        assert_eq!(standalone.all().len(), 2);

        let forgotten = forget_one(&first, &mut peers, &mut standalone).unwrap();

        // What `first` was granted is gone now, not at some later start, and `second` keeps its own.
        assert!(standalone.all().get(&first).is_none(), "first kept its grants");
        assert!(standalone.all().get(&second).is_some(), "second lost its grants");
        assert!(forgotten >= 1);
        // The same thing again is not an error and forgets nothing more.
        assert_eq!(forget_one(&first, &mut peers, &mut standalone).unwrap(), 0);
    }

    #[test]
    fn nothing_but_the_plugin_is_left_in_the_store() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let names: Vec<String> = fs::read_dir(manager.store())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["thing"], "a staging directory was left behind");
    }

    #[test]
    fn the_store_is_made_private_and_a_package_arrives_private() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        // Offered group-writable and world-readable, as a download might be.
        let source = package(parent.path(), "thing", GOOD, b"x", 0o775);
        manager.install(&source).unwrap();
        let store = fs::metadata(manager.store()).unwrap().permissions().mode();
        assert_eq!(store & 0o077, 0, "the store is not private");
        let program = fs::metadata(manager.store().join("thing").join(PROGRAM_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(program & 0o022, 0, "the program can be changed by others");
        assert_ne!(program & 0o100, 0, "the program cannot be run");
        let manifest = fs::metadata(manager.store().join("thing").join(MANIFEST_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            manifest & 0o177,
            0,
            "the manifest is not private or is runnable"
        );
    }

    #[test]
    fn a_package_the_application_would_refuse_to_load_is_not_installed() {
        let parent = tempfile::tempdir().unwrap();
        // Each is refused by `discover`, so installing it would install something never run.
        // The last three are well-formed JSON that parses as a manifest, so only the rules
        // the registry applies can refuse them.
        let wrong_version = GOOD.replace("\"manifest_version\": 1", "\"manifest_version\": 2");
        let unguarded_receive = r#"{
            "manifest_version": 1, "capabilities": {}, "imports": [],
            "exports": [{
                "service": "thing.service", "version": 1, "description": "A test service.",
                "consumers": ["host"],
                "methods": {"receive": {"description": "Takes things in with no guard."}},
                "events": {}
            }]
        }"#;
        let unreachable_action = r#"{
            "manifest_version": 1,
            "capabilities": {"thing.use": "Lets the thing go."}, "imports": [],
            "exports": [{
                "service": "thing.service", "version": 1, "description": "A test service.",
                "consumers": [],
                "methods": {"go": {"capability": "thing.use", "description": "Goes."}},
                "events": {}
            }],
            "contributions": {"actions": [{
                "id": "go", "label": "Go",
                "target": {"service": "thing.service", "method": "go"},
                "capability": "thing.use", "needs_device": false, "destructive": false
            }]}
        }"#;
        let cases: [(&str, &str, &[u8], u32); 6] = [
            ("not json", "this is not a manifest", b"x", 0o755),
            ("empty manifest", "", b"x", 0o755),
            ("not runnable", GOOD, b"x", 0o644),
            ("wrong manifest version", &wrong_version, b"x", 0o755),
            ("receive with no guard", unguarded_receive, b"x", 0o755),
            ("action nobody can reach", unreachable_action, b"x", 0o755),
        ];
        for (name, manifest, program, mode) in cases {
            let data = data();
            let manager = Manager::new(data.path());
            let source = package(parent.path(), "thing", manifest, program, mode);
            assert!(manager.install(&source).is_err(), "{name} was installed");
            assert!(manager.list().unwrap().is_empty(), "{name} was recorded");
            let left = fs::read_dir(manager.store())
                .map(|r| r.count())
                .unwrap_or(0);
            assert_eq!(left, 0, "{name} left something in the store");
            fs::remove_dir_all(&source).unwrap();
        }
    }

    #[test]
    fn a_plugin_offering_a_service_another_installed_plugin_offers_is_refused() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "first", GOOD, b"x", 0o755))
            .unwrap();
        // The same service name under another plugin: inbound routing would be ambiguous.
        let clash = package(parent.path(), "second", GOOD, b"y", 0o755);
        assert_eq!(
            manager.install(&clash),
            Err(Refusal("It offers a service another plugin already offers"))
        );
        assert_eq!(ids(&manager.list().unwrap()), [("first", Standing::Ready)]);
        assert!(!manager.store().join("second").exists());
        // The control: a plugin offering a different service is installed alongside it.
        let other = GOOD.replace("thing.service", "other.service");
        manager
            .install(&package(parent.path(), "third", &other, b"z", 0o755))
            .unwrap();
        assert_eq!(
            ids(&manager.list().unwrap()),
            [("first", Standing::Ready), ("third", Standing::Ready)]
        );
    }

    #[test]
    fn an_installed_plugin_that_can_no_longer_load_does_not_block_another_install() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "broken", GOOD, b"x", 0o755))
            .unwrap();
        // Its manifest is damaged after the fact; `list` reports it, and it must not take
        // an unrelated install down with it.
        let manifest = manager.store().join("broken").join(MANIFEST_FILE);
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&manifest, b"damaged").unwrap();
        let other = GOOD.replace("thing.service", "other.service");
        assert!(
            manager
                .install(&package(parent.path(), "fine", &other, b"y", 0o755))
                .is_ok()
        );
        assert_eq!(
            ids(&manager.list().unwrap()),
            [("broken", Standing::Broken), ("fine", Standing::Ready)]
        );
    }

    #[test]
    fn a_package_without_both_files_is_refused_before_the_store_is_touched() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        for missing in [MANIFEST_FILE, PROGRAM_FILE] {
            let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
            fs::remove_file(source.join(missing)).unwrap();
            assert!(manager.install(&source).is_err(), "no {missing}");
            fs::remove_dir_all(&source).unwrap();
        }
        assert!(!manager.store().exists(), "the store was made for nothing");
    }

    #[test]
    fn the_id_is_the_directory_name_and_an_unusable_one_is_refused() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        // A leading dot is what marks a directory being staged or retired, so it can never be
        // a plugin's name; a space is outside the grammar every name in the system shares.
        for name in ["Has Space", ".hidden", ".install-x-1", ".retired-x-1"] {
            let source = package(parent.path(), name, GOOD, b"x", 0o755);
            assert!(manager.install(&source).is_err(), "{name:?} installed");
            fs::remove_dir_all(&source).unwrap();
        }
        assert!(manager.list().unwrap().is_empty());
        // The control: names that are fine are not caught by the same rule.
        for name in ["UPPER", "a..b", "example.live-agent"] {
            let source = package(parent.path(), name, GOOD, b"x", 0o755);
            // Each needs its own manifest-valid service name only in one store at a time.
            let data = self::data();
            assert!(
                Manager::new(data.path()).install(&source).is_ok(),
                "{name:?} was refused"
            );
            fs::remove_dir_all(&source).unwrap();
        }
    }

    #[test]
    fn installing_the_same_package_again_changes_nothing() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", GOOD, b"program", 0o755);
        let Ok(Installed::New(first)) = manager.install(&source) else {
            panic!("first install failed");
        };
        assert_eq!(manager.install(&source), Ok(Installed::Unchanged(first)));
        assert_eq!(manager.list().unwrap().len(), 1);
    }

    #[test]
    fn a_different_package_under_an_installed_name_is_refused_not_written_over() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let first = package(parent.path(), "thing", GOOD, b"first", 0o755);
        manager.install(&first).unwrap();
        fs::remove_dir_all(&first).unwrap();
        let second = package(parent.path(), "thing", GOOD, b"SECOND", 0o755);
        assert!(manager.install(&second).is_err());
        assert_eq!(
            fs::read(manager.store().join("thing").join(PROGRAM_FILE)).unwrap(),
            b"first",
            "an installed plugin was replaced"
        );
    }

    #[test]
    fn only_the_program_is_measured_so_a_data_file_changing_is_not_a_change_of_what_was_approved() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
        fs::create_dir(source.join("assets")).unwrap();
        fs::write(source.join("assets").join("data.bin"), b"original").unwrap();
        manager.install(&source).unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
        let file = manager.store().join("thing").join("assets").join("data.bin");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&file, b"tampered").unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
    }

    #[test]
    fn what_is_changed_in_an_installed_package_decides_its_standing_exactly() {
        /// A name for the change, the change made to an installed package, and what the folder then says.
        type Tampering = (&'static str, fn(&Path), Standing);
        let ways: [Tampering; 5] = [
            // The program is what runs, and it is what was approved.
            ("program changed", |d| fs::write(d.join(PROGRAM_FILE), b"other").unwrap(), Standing::Changed),
            // It now asks for something the person was never shown.
            (
                "manifest asks for something else",
                |d| fs::write(d.join(MANIFEST_FILE), GOOD.replace("thing.use", "thing.usx")).unwrap(),
                Standing::NeedsApproval,
            ),
            // Not the program, so not measured (see `a_file_added_beside_the_program_...`).
            ("file added", |d| fs::write(d.join("extra"), b"x").unwrap(), Standing::Ready),
            // A program that cannot be run is not something to approve or to run.
            (
                "runnable bit dropped",
                |d| fs::set_permissions(d.join(PROGRAM_FILE), fs::Permissions::from_mode(0o600)).unwrap(),
                Standing::Broken,
            ),
            ("manifest removed", |d| fs::remove_file(d.join(MANIFEST_FILE)).unwrap(), Standing::Broken),
        ];
        for (name, change, expected) in ways {
            let (parent, data) = (tempfile::tempdir().unwrap(), data());
            let manager = Manager::new(data.path());
            manager
                .install(&package(parent.path(), "thing", GOOD, b"first", 0o755))
                .unwrap();
            change(&manager.store().join("thing"));
            assert_eq!(ids(&manager.list().unwrap()), [("thing", expected)], "{name}");
        }
    }

    #[test]
    fn a_directory_put_in_the_store_by_hand_is_listed_with_what_it_lacks() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "recorded", GOOD, b"x", 0o755))
            .unwrap();
        // An empty folder is not a plugin waiting for approval: it has no manifest and no program.
        fs::create_dir(manager.store().join("byhand")).unwrap();
        // A complete package put there by hand (nobody approved it) is waiting for approval.
        let complete = package(manager.store(), "complete", &GOOD.replace("thing", "complete"), b"x", 0o700);
        fs::set_permissions(&complete, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            ids(&manager.list().unwrap()),
            [
                ("byhand", Standing::Broken),
                ("complete", Standing::NeedsApproval),
                ("recorded", Standing::Ready)
            ]
        );
    }

    #[test]
    fn a_plugin_whose_files_are_gone_leaves_nothing_behind_to_list() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        fs::remove_dir_all(manager.store().join("thing")).unwrap();
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn removing_deletes_the_package_and_its_record() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        manager.remove("thing").unwrap();
        assert!(!manager.store().join("thing").exists());
        assert!(manager.list().unwrap().is_empty());
        // And it can be installed again afterwards.
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
    }

    #[test]
    fn a_plugin_whose_program_was_changed_can_still_be_removed_and_nothing_is_left_behind() {
        // The plugin store is the application's own directory, and a plugin in it must never become something the
        // person can neither use nor remove. One whose program is not what was approved goes whole: the folder, so that
        // the name is free again.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        fs::write(manager.store().join("thing").join(PROGRAM_FILE), b"not what was installed").unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Changed)]);
        manager.remove("thing").expect("a changed plugin could not be removed");
        assert!(!manager.store().join("thing").exists(), "the folder was left behind");
        assert!(manager.list().unwrap().is_empty(), "it is still listed after it was removed");
        // And the name is free: it installs again.
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
    }

    #[test]
    fn a_folder_nobody_installed_can_be_removed_too() {
        // Put there by hand with nothing a plugin has (no manifest, no program): it is Broken, listed, and removable.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "recorded", GOOD, b"x", 0o755))
            .unwrap();
        let byhand = manager.store().join("byhand");
        fs::create_dir(&byhand).unwrap();
        fs::write(byhand.join("anything"), b"x").unwrap();
        assert_eq!(
            ids(&manager.list().unwrap()),
            [("byhand", Standing::Broken), ("recorded", Standing::Ready)]
        );
        manager.remove("byhand").expect("a folder put there by hand could not be removed");
        assert!(!byhand.exists());
        // What was recorded is untouched by it.
        assert_eq!(ids(&manager.list().unwrap()), [("recorded", Standing::Ready)]);
    }

    #[test]
    fn a_plugin_whose_folder_is_gone_is_simply_not_there() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        fs::remove_dir_all(manager.store().join("thing")).unwrap();
        // Nothing remembers it, so there is nothing to list and nothing to forget; removing it is refused as unknown.
        assert!(manager.list().unwrap().is_empty());
        assert!(manager.remove("thing").is_err());
    }

    #[test]
    fn removing_what_is_neither_recorded_nor_a_folder_is_refused() {
        let data = data();
        assert!(Manager::new(data.path()).remove("nothing").is_err());
        // And a name that could point out of the store is never a plugin to remove.
        for name in ["..", ".", "a/b", "", ".hidden", "../x"] {
            assert!(Manager::new(data.path()).remove(name).is_err(), "{name:?} was accepted");
        }
    }

    #[test]
    fn removing_a_link_in_the_store_takes_away_the_link_and_never_what_it_points_to() {
        // Something that is not the store's is never followed: a link put in the store by hand, pointing at the
        // person's own files, is removed as a link, and what it points at is still there afterwards.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "recorded", GOOD, b"x", 0o755))
            .unwrap();
        let elsewhere = parent.path().join("the-persons-own-folder");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("precious.txt"), b"keep me").unwrap();
        std::os::unix::fs::symlink(&elsewhere, manager.store().join("sneaky")).unwrap();
        manager.remove("sneaky").expect("a link in the store could not be removed");
        assert!(fs::symlink_metadata(manager.store().join("sneaky")).is_err(), "the link is still there");
        assert!(elsewhere.join("precious.txt").exists(), "what the link pointed to was deleted");
        assert_eq!(ids(&manager.list().unwrap()), [("recorded", Standing::Ready)]);
    }

    #[test]
    fn removal_is_refused_when_the_store_itself_is_a_link_and_nothing_outside_is_touched() {
        // The store is a directory of the application's own, but if it has been turned into a link (by a mistake, a
        // migration, a hand) then "remove thing" would act on a folder somewhere else with the same name. The one thing
        // that makes unconditional removal safe is that the store really is the store, so it is checked before any
        // delete, and a link is refused.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let outside = parent.path().join("somewhere-else");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(outside.join("thing")).unwrap();
        fs::write(outside.join("thing").join("precious"), b"must survive").unwrap();
        std::os::unix::fs::symlink(&outside, data.path().join("plugins")).unwrap();
        let refused = Manager::new(data.path())
            .remove("thing")
            .expect_err("a removal went through a store that is a link");
        // The refusal must be about the link and not about something else that happens to refuse (a permission, a
        // lock): a test that passes for the wrong reason holds nothing.
        assert_eq!(refused.to_string(), "The plugin store is a link");
        assert!(outside.join("thing").join("precious").exists(), "something outside the store was deleted");
    }

    #[test]
    fn a_data_directory_that_sits_below_a_link_still_works_as_it_does_when_the_home_is_a_link() {
        // On an immutable system `/home` is a link to `/var/home` and the whole of a person's data is under it. A rule
        // that refused every path with a link in it would refuse every removal there, which is the block this exists to
        // end. Only what is below the data directory is judged, so a link ABOVE the data directory (as `/home` is) is not
        // looked at. The link is above the data directory here, not on it: that is where it is on such a system.
        let (parent, through) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let real_home = parent.path().join("real-home");
        fs::create_dir(&real_home).unwrap();
        let linked_home = through.path().join("home");
        std::os::unix::fs::symlink(&real_home, &linked_home).unwrap();
        let data = linked_home.join("data");
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
        let manager = Manager::new(&data);
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        assert!(real_home.join("data").join("plugins").join("thing").exists());
        manager.remove("thing").expect("a data directory below a link could not be used");
        assert!(!real_home.join("data").join("plugins").join("thing").exists(), "the folder was left behind");
    }

    #[test]
    fn a_damaged_record_file_does_not_hide_the_plugins_that_are_on_disk() {
        // A listing that failed with the record would show nothing at all, which is the state the person was left in
        // ("no plugins installed" with a plugin on disk). Every folder is listed, and what cannot be known about it is
        // said as "unrecorded", which is true, and each can be removed.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "alpha", GOOD, b"x", 0o755))
            .unwrap();
        manager
            .install(&package(parent.path(), "beta", &GOOD.replace("thing", "other"), b"y", 0o755))
            .unwrap();
        fs::write(data.path().join("installed.json"), b"\xff\xfe not json at all").unwrap();
        assert_eq!(
            ids(&manager.list().expect("a damaged record made the listing fail")),
            [("alpha", Standing::Ready), ("beta", Standing::Ready)]
        );
        // And a folder that is hidden is the store's own, not a plugin to be offered.
        fs::create_dir(manager.store().join(".retired-x-1")).unwrap();
        assert_eq!(manager.list().unwrap().len(), 2);
    }

    #[test]
    fn an_old_damaged_record_file_does_not_stop_a_plugin_from_being_removed() {
        // The case the person described: a corrupt installed.json left a plugin that could be neither used nor removed.
        // Nothing reads that file now, and the plugin is removed whatever it holds.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        fs::write(data.path().join("installed.json"), b"this is not json {{{").unwrap();
        manager.remove("thing").expect("a damaged record kept a plugin from being removed");
        assert!(!manager.store().join("thing").exists(), "the folder was left behind");
    }

    #[test]
    fn a_name_that_would_leave_the_store_is_refused_even_when_something_is_there() {
        // The refusal must come from the name itself and not from nothing being found: something is placed where
        // each of these names would lead, and it must still be there afterwards.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        // `<data>/plugins/../keep` is `<data>/keep`.
        fs::create_dir(data.path().join("keep")).unwrap();
        fs::write(data.path().join("keep").join("file"), b"must survive").unwrap();
        fs::create_dir(manager.store().join(".hidden")).unwrap();
        for name in ["../keep", ".hidden", "..", "thing/.."] {
            assert!(manager.remove(name).is_err(), "{name:?} was accepted");
        }
        assert!(data.path().join("keep").join("file").exists(), "something outside the store was deleted");
        assert!(manager.store().join(".hidden").exists(), "a hidden entry of the store was deleted");
        assert!(manager.store().join("thing").exists());
    }

    #[test]
    fn a_link_in_the_store_is_removed_as_a_link_even_when_it_points_at_a_folder_of_the_store() {
        // A link to a folder is a folder to `remove_dir_all` after a rename, so only an explicit check keeps the
        // link and its target apart: the target, which is another plugin, is still there afterwards.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "real", GOOD, b"x", 0o755))
            .unwrap();
        std::os::unix::fs::symlink(manager.store().join("real"), manager.store().join("alias")).unwrap();
        manager.remove("alias").expect("a link could not be removed");
        assert!(fs::symlink_metadata(manager.store().join("alias")).is_err());
        assert!(manager.store().join("real").join(PROGRAM_FILE).exists(), "the plugin the link pointed at was deleted");
    }

    #[test]
    fn a_link_anywhere_in_the_package_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let outside = parent.path().join("outside");
        fs::write(&outside, b"not the plugin's own").unwrap();
        /// A name for where the link is, and the change that puts it there.
        type Placement = (&'static str, fn(&Path, &Path));
        let places: [Placement; 3] = [
            ("program is a link", |source, outside| {
                fs::remove_file(source.join(PROGRAM_FILE)).unwrap();
                std::os::unix::fs::symlink(outside, source.join(PROGRAM_FILE)).unwrap();
            }),
            ("extra file is a link", |source, outside| {
                std::os::unix::fs::symlink(outside, source.join("extra")).unwrap();
            }),
            ("nested link", |source, outside| {
                fs::create_dir(source.join("sub")).unwrap();
                std::os::unix::fs::symlink(outside, source.join("sub").join("deep")).unwrap();
            }),
        ];
        for (name, make) in places {
            let data = data();
            let manager = Manager::new(data.path());
            let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
            make(&source, &outside);
            assert!(manager.install(&source).is_err(), "{name} was installed");
            assert!(manager.list().unwrap().is_empty(), "{name} was recorded");
            fs::remove_dir_all(&source).unwrap();
        }
    }

    #[test]
    fn a_source_that_is_not_a_directory_is_refused() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let file = parent.path().join("thing");
        fs::write(&file, b"x").unwrap();
        assert!(manager.install(&file).is_err());
        assert!(manager.install(&parent.path().join("missing")).is_err());
        // A link to a good package is a link: not followed.
        let real = package(parent.path(), "real", GOOD, b"x", 0o755);
        let link = parent.path().join("alias");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(manager.install(&link).is_err());
    }

    #[test]
    fn a_store_others_can_change_is_not_used() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        fs::create_dir(manager.store()).unwrap();
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o777)).unwrap();
        let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
        assert!(manager.install(&source).is_err());
        assert_eq!(fs::read_dir(manager.store()).unwrap().count(), 0);
    }

    #[test]
    fn too_many_files_are_refused_without_installing_any() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
        for n in 0..MAX_FILES {
            fs::write(source.join(format!("f{n}")), b"x").unwrap();
        }
        assert!(manager.install(&source).is_err());
        assert!(manager.list().unwrap().is_empty());
    }

    /// A manifest that names these files, as `package` writes it.
    fn manifest_naming(files: &[&str]) -> String {
        let list: Vec<String> = files.iter().map(|f| format!("{f:?}")).collect();
        GOOD.replacen('{', &format!("{{\"files\":[{}],", list.join(",")), 1)
    }

    #[test]
    fn a_package_that_names_its_files_installs_only_those_and_what_lies_beside_them_is_left_out() {
        // The extreme is a home folder with a manifest and a program in it; the everyday case is a
        // README and a spare file. Neither is installed, because the package did not name them.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["lib/libwork.so", "settings.toml"]), b"x", 0o755);
        fs::create_dir_all(source.join("lib")).unwrap();
        fs::write(source.join("lib").join("libwork.so"), b"lib").unwrap();
        fs::write(source.join("settings.toml"), b"t").unwrap();
        fs::write(source.join("README.md"), b"read me").unwrap();
        fs::create_dir(source.join("Documents")).unwrap();
        fs::write(source.join("Documents").join("private.txt"), b"mine").unwrap();

        let Ok(Installed::New(_)) = manager.install(&source) else { panic!("not installed") };
        let installed = manager.store().join("thing");
        for named in ["plugin.json", "plugin", "lib/libwork.so", "settings.toml"] {
            assert!(installed.join(named).is_file(), "{named} was not installed");
        }
        assert!(!installed.join("README.md").exists(), "a file the package did not name was installed");
        assert!(!installed.join("Documents").exists(), "a folder the package did not name was installed");
    }

    #[test]
    fn a_manifest_that_fails_validation_never_widens_what_is_taken_from_the_folder() {
        // Its file list is not allowed (it names a way out). The manifest as a whole is refused, so
        // nothing is installed: in particular NOT everything in the folder, which is what falling back
        // to "no list" would do for a package that said, in writing, that it wants less.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["../escape"]), b"x", 0o755);
        fs::write(source.join("README.md"), b"read me").unwrap();
        assert!(manager.install(&source).is_err());
        assert!(manager.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(manager.store()).map(|d| d.count()).unwrap_or(0), 0);
        // Nothing is measured for a package that is refused: there is no digest to compare against,
        // so nothing can be confirmed against one, and nothing is taken in its place.
        assert!(package_digest(&source).is_err());
    }

    #[test]
    fn an_installed_package_whose_listed_data_file_is_removed_afterwards_is_still_the_approved_program() {
        // What the manifest lists is copied at install; afterwards the plugin's own data files are the plugin's. Only the
        // program is approved, so a data file going does not change the standing (the plugin may fail on its own).
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["settings.toml"]), b"x", 0o755);
        fs::write(source.join("settings.toml"), b"t").unwrap();
        manager.install(&source).unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);

        fs::remove_file(manager.store().join("thing").join("settings.toml")).unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
    }

    #[test]
    fn a_folder_that_names_its_files_is_not_refused_for_the_size_or_links_of_what_it_does_not_name() {
        // The same package that installs from an archive must install from a folder: what the list
        // leaves out is not looked at, so neither its size nor a link lying among it refuses the package.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["settings.toml"]), b"x", 0o755);
        fs::write(source.join("settings.toml"), b"t").unwrap();
        // Not named, and more than a package may hold, as a home folder would be.
        let big = fs::File::create(source.join("sources.bin")).unwrap();
        big.set_len(MAX_BYTES + 1).unwrap();
        // Not named, and a link, which a package may not contain.
        std::os::unix::fs::symlink("/etc/passwd", source.join("alias")).unwrap();
        for n in 0..=MAX_FILES {
            fs::write(source.join(format!("junk{n}")), b"x").unwrap();
        }

        let Ok(Installed::New(_)) = manager.install(&source) else {
            panic!("a package was refused for what it does not name");
        };
        let installed = manager.store().join("thing");
        for named in ["plugin.json", "plugin", "settings.toml"] {
            assert!(installed.join(named).is_file(), "{named} was not installed");
        }
        assert!(!installed.join("sources.bin").exists() && !installed.join("alias").exists());
        assert!(!installed.join("junk0").exists());
        // What is shown is measured the same way, so confirming is not refused either.
        assert!(package_digest(&source).is_ok());
    }

    #[test]
    fn a_file_added_beside_the_program_does_not_change_the_standing_and_this_is_the_accepted_cost() {
        // The approval is of the PROGRAM (its bytes and whether it can be run), not of the folder: the design chose this
        // on purpose (docs/plugin-installation-design.txt, M3 and C1). A file dropped next to a plugin is therefore NOT
        // noticed. That is a real limit, pinned here so it cannot be forgotten or changed by accident: a plugin that loads
        // libraries from its own folder is not protected from what is put there, and the plugin runs as the same user with no
        // sandbox, so the approval was never a guarantee against a process that can write there.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["settings.toml"]), b"x", 0o755);
        fs::write(source.join("settings.toml"), b"t").unwrap();
        manager.install(&source).unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);

        fs::write(manager.store().join("thing").join("injected.so"), b"evil").unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
        // The program itself changing IS noticed.
        fs::write(manager.store().join("thing").join(PROGRAM_FILE), b"other program").unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Changed)]);
    }

    #[test]
    fn installing_refuses_a_named_file_the_package_lacks_with_that_reason_and_not_another() {
        // Pinned by its reason: a refusal that came from somewhere else would hide the rule.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["missing.so"]), b"x", 0o755);
        assert_eq!(
            manager.install(&source).unwrap_err(),
            Refusal("The package names a file it does not have")
        );
    }

    #[test]
    fn an_installed_package_whose_manifest_is_damaged_afterwards_is_broken_and_can_be_removed() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["settings.toml"]), b"x", 0o755);
        fs::write(source.join("settings.toml"), b"t").unwrap();
        manager.install(&source).unwrap();
        fs::write(manager.store().join("thing").join(MANIFEST_FILE), b"this is not json").unwrap();
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Broken)]);
        manager.remove("thing").expect("a plugin with a destroyed manifest could not be removed");
    }

    #[test]
    fn a_package_without_a_file_list_still_takes_everything_in_its_folder() {
        // Every package made before the list existed: nothing about it changes.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
        fs::write(source.join("extra.txt"), b"e").unwrap();
        let Ok(Installed::New(_)) = manager.install(&source) else { panic!("not installed") };
        assert!(manager.store().join("thing").join("extra.txt").is_file());
    }

    #[test]
    fn a_package_that_names_a_file_it_does_not_hold_is_refused_and_nothing_is_installed() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["missing.so"]), b"x", 0o755);
        assert!(manager.install(&source).is_err());
        assert!(manager.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(manager.store()).map(|d| d.count()).unwrap_or(0), 0);
        // A named directory is not a file either.
        let dir = package(parent.path(), "other", &manifest_naming(&["lib"]), b"x", 0o755);
        fs::create_dir(dir.join("lib")).unwrap();
        assert!(manager.install(&dir).is_err());
    }

    #[test]
    fn what_is_not_named_does_not_change_the_digest_and_what_is_named_does() {
        // The person was shown a package; a file the package does not name changing next to it is
        // not that package changing, and confirming must not be refused for it.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", &manifest_naming(&["settings.toml"]), b"x", 0o755);
        fs::write(source.join("settings.toml"), b"t").unwrap();
        let shown = package_digest(&source).unwrap();

        fs::write(source.join("README.md"), b"anything").unwrap();
        assert_eq!(package_digest(&source).unwrap(), shown, "an unnamed file changed the digest");
        fs::write(source.join("settings.toml"), b"changed").unwrap();
        assert_ne!(package_digest(&source).unwrap(), shown, "a named file changed and nothing noticed");

        // And the same package installs, then installing it again changes nothing: the digest of the
        // folder it came from and of what is in the store are the same, README or not.
        fs::write(source.join("settings.toml"), b"t").unwrap();
        let Ok(Installed::New(first)) = manager.install(&source) else { panic!("not installed") };
        assert_eq!(first, "thing");
        assert!(crate::plugin_approval::read(&manager.store().join("thing")).is_some_and(|a| a.sha256.len() == 64));
        assert!(matches!(manager.install(&source), Ok(Installed::Unchanged(_))));
    }

    #[test]
    fn a_damaged_old_record_file_stops_neither_an_install_nor_the_listing() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        fs::write(data.path().join("installed.json"), b"this is not json").unwrap();
        assert!(manager.list().unwrap().is_empty());
        let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
        assert!(matches!(manager.install(&source), Ok(Installed::New(_))));
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
    }

    // What the application actually runs. `discover` only reads a directory; this is the one
    // way plugins are loaded, so what is run is what was installed and still is.

    #[test]
    fn a_fresh_data_directory_loads_nothing_and_reports_no_problem() {
        // The first run of the application has no plugins and no store. That is not a
        // problem to report, and loading must not be what creates the store: only installing
        // does, with its checks.
        let data = data();
        let manager = Manager::new(data.path());
        let loaded = manager.load();
        assert!(loaded.registry.is_empty());
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert!(!manager.store().exists(), "loading made the store");
    }

    #[test]
    fn what_was_installed_and_is_unchanged_is_loaded() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let loaded = manager.load();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.registry.iter().count(), 1);
    }

    #[test]
    fn installing_is_approving_what_was_shown_so_an_installed_plugin_is_ready_and_loads() {
        // The person sees what a package asks for before they confirm, so installing is the approval of exactly that.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"x", 0o755)).unwrap();
        let approval = crate::plugin_approval::read(&manager.store().join("thing")).expect("installing wrote no approval");
        assert_eq!(approval.capabilities.iter().map(String::as_str).collect::<Vec<_>>(), ["thing.use"]);
        let loaded = manager.load();
        assert_eq!(loaded.registry.iter().count(), 1, "{:?}", loaded.problems);
        assert!(loaded.held_back.is_empty(), "{:?}", loaded.held_back);
    }

    #[test]
    fn a_plugin_whose_manifest_wording_was_rewritten_and_whose_old_record_was_destroyed_still_runs_and_is_listed() {
        // The case that started this. The manifest is rewritten after install (another description, other whitespace) and
        // installed.json is destroyed; the plugin must still be found, still run, and still be removable.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"x", 0o755)).unwrap();
        fs::write(
            manager.store().join("thing").join(MANIFEST_FILE),
            GOOD.replace("Lets the thing do what it does.", "Rewritten later, by someone else.").replace("  ", "\t"),
        )
        .unwrap();
        fs::write(data.path().join("installed.json"), b"destroyed {{{").unwrap();
        let loaded = manager.load();
        assert_eq!(loaded.registry.iter().count(), 1, "the plugin did not load: {:?} {:?}", loaded.problems, loaded.held_back);
        assert!(loaded.held_back.is_empty());
        // And it is listed, whatever the record says.
        // Only the words changed, and what it asks for did not: it is still what the person approved. (Changing what it
        // asks for is `NeedsApproval`, and is held by `what_is_changed_in_an_installed_package_decides_its_standing_exactly`.)
        assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
        manager.remove("thing").expect("it could not be removed");
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn a_plugin_nobody_approved_is_held_back_with_its_standing_and_never_run() {
        // Put in the store by hand: a perfectly good package, no approval. It is not run, and it is not invisible: it is
        // reported with what it is waiting for, so that it can be approved or removed.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "installed", GOOD, b"x", 0o755)).unwrap();
        let other = GOOD.replace("thing.service", "other.service");
        let by_hand = package(manager.store(), "byhand", &other, b"y", 0o755);
        fs::set_permissions(&by_hand, fs::Permissions::from_mode(0o700)).unwrap();
        let loaded = manager.load();
        let ran: Vec<&str> = loaded.registry.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ran, ["installed"], "a plugin nobody approved was run");
        let held: Vec<(&str, peersus_core::model::Standing)> =
            loaded.held_back.iter().map(|h| (h.id.as_str(), h.standing)).collect();
        assert_eq!(held, [("byhand", peersus_core::model::Standing::NeedsApproval)]);
    }

    /// A package put in the store by hand (so nobody approved it), and the digest the owner would have shown for it.
    fn waiting(manager: &Manager, name: &str, program: &[u8]) -> String {
        let folder = package(manager.store(), name, &GOOD.replace("thing", name), program, 0o755);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        let held = manager.load().held_back;
        let found = held.iter().find(|h| h.id.as_str() == name).expect("it was not held back");
        assert_eq!(found.standing, peersus_core::model::Standing::NeedsApproval);
        found.program.clone().expect("no digest was measured for a plugin that waits")
    }

    #[test]
    fn approving_what_was_shown_makes_a_plugin_that_waited_ready_and_loaded() {
        let data = data();
        let manager = Manager::new(data.path());
        fs::create_dir_all(manager.store()).unwrap();
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o700)).unwrap();
        let shown = waiting(&manager, "byhand", b"the program");
        manager.approve("byhand", &shown).expect("what was shown could not be approved");
        let loaded = manager.load();
        assert_eq!(loaded.registry.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["byhand"]);
        assert!(loaded.held_back.is_empty(), "{:?}", loaded.held_back);
        // What was approved is what the manifest asked for, by name, and the program that was shown.
        let approval = crate::plugin_approval::read(&manager.store().join("byhand")).unwrap();
        assert_eq!(approval.sha256, shown);
        assert_eq!(approval.capabilities.iter().map(String::as_str).collect::<Vec<_>>(), ["byhand.use"]);
    }

    #[test]
    fn a_program_that_is_not_the_one_shown_is_not_approved_and_nothing_is_written() {
        let data = data();
        let manager = Manager::new(data.path());
        fs::create_dir_all(manager.store()).unwrap();
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o700)).unwrap();
        let shown = waiting(&manager, "byhand", b"the program that was shown");
        // Swapped after it was shown and before it was approved.
        let program = manager.store().join("byhand").join(PROGRAM_FILE);
        fs::write(&program, b"another program").unwrap();
        let refusal = manager.approve("byhand", &shown).unwrap_err();
        assert_eq!(refusal, Refusal("The plugin is not what was shown; look at it again"));
        assert!(!manager.store().join("byhand").join(crate::plugin_approval::FILE).exists(), "an approval was written anyway");
        // Empty and made-up digests are never a match either.
        for bad in ["", "0", &"0".repeat(64)] {
            assert!(manager.approve("byhand", bad).is_err(), "{bad:?} was accepted");
        }
        assert!(!manager.store().join("byhand").join(crate::plugin_approval::FILE).exists());
    }

    #[test]
    fn only_a_plugin_that_waits_or_has_changed_can_be_approved() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "ready", GOOD, b"x", 0o755)).unwrap();
        // Ready: nothing to approve.
        let digest = crate::plugin_approval::read(&manager.store().join("ready")).unwrap().sha256;
        assert_eq!(manager.approve("ready", &digest).unwrap_err(), Refusal("There is nothing to approve for this plugin"));
        // Broken: only removable.
        fs::write(manager.store().join("ready").join(MANIFEST_FILE), b"destroyed").unwrap();
        assert_eq!(manager.approve("ready", &digest).unwrap_err(), Refusal("There is nothing to approve for this plugin"));
        // A name that is not a plugin's, and one with no folder.
        for name in ["../x", ".hidden", "", "nothing"] {
            assert!(manager.approve(name, &digest).is_err(), "{name:?} was accepted");
        }
    }

    #[test]
    fn a_plugin_whose_program_changed_runs_again_only_after_it_is_approved_as_shown() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"first", 0o755)).unwrap();
        fs::write(manager.store().join("thing").join(PROGRAM_FILE), b"second").unwrap();
        let loaded = manager.load();
        assert!(loaded.registry.iter().next().is_none(), "a changed program ran");
        let shown = loaded.held_back[0].program.clone().unwrap();
        assert_eq!(loaded.held_back[0].standing, peersus_core::model::Standing::Changed);
        manager.approve("thing", &shown).unwrap();
        let after = manager.load();
        assert_eq!(after.registry.iter().count(), 1, "{:?}", after.held_back);
        assert!(after.held_back.is_empty());
    }

    #[test]
    fn a_path_for_a_name_never_switches_retires_or_restores_what_is_outside_the_store() {
        // Each of these takes a name and joins it to the store. Something real sits where `../outside` leads, so a refusal
        // that came from "nothing there" or "wrong state" cannot be what holds: only the name can.
        let data = data();
        let manager = Manager::new(data.path());
        fs::create_dir_all(manager.store()).unwrap();
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o700)).unwrap();
        let outside = package(data.path(), "outside", &GOOD.replace("thing", "outside"), b"p", 0o755);
        approve_copy(&outside).unwrap();
        let name = Refusal("That is not the name of a plugin");
        assert_eq!(manager.set_disabled("../outside", true).unwrap_err(), name);
        assert!(!outside.join(crate::plugin_folder::DISABLED_MARKER).exists(), "a switch was written outside the store");
        assert_eq!(manager.retire("../outside").unwrap_err(), name);
        assert!(outside.is_dir(), "a folder outside the store was moved");
        // `restore` takes what is retired too: a folder named like one that was set aside, but of another plugin's name.
        let retired = manager.store().join(".retired-outside-1");
        fs::rename(&outside, &retired).unwrap();
        assert_eq!(manager.restore("../outside", &retired).unwrap_err(), name);
        assert!(retired.is_dir() && !data.path().join("outside").exists(), "something was put outside the store");
    }

    #[test]
    fn a_complete_package_outside_the_store_is_not_approved_through_a_path_for_a_name() {
        // The refusal has to come from the NAME. A folder that is complete and sits where `../outside` leads would otherwise
        // be approved (an approval written outside the store), because nothing else about it is wrong.
        let data = data();
        let manager = Manager::new(data.path());
        fs::create_dir_all(manager.store()).unwrap();
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o700)).unwrap();
        let outside = package(data.path(), "outside", &GOOD.replace("thing", "outside"), b"p", 0o755);
        let digest = crate::plugin_approval::program_digest(&outside.join(PROGRAM_FILE), crate::plugin_folder::MAX_PROGRAM_BYTES).unwrap();
        assert_eq!(manager.approve("../outside", &digest).unwrap_err(), Refusal("That is not the name of a plugin"));
        assert!(!outside.join(crate::plugin_approval::FILE).exists(), "an approval was written outside the store");
    }

    #[test]
    fn a_plugin_whose_program_was_replaced_is_held_back_as_changed_and_one_that_cannot_run_as_broken() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "swapped", GOOD, b"x", 0o755)).unwrap();
        let other = GOOD.replace("thing.service", "other.service");
        manager.install(&package(parent.path(), "broken", &other, b"y", 0o755)).unwrap();
        fs::write(manager.store().join("swapped").join(PROGRAM_FILE), b"a different program").unwrap();
        fs::write(manager.store().join("broken").join(MANIFEST_FILE), b"not a manifest").unwrap();
        let loaded = manager.load();
        assert_eq!(loaded.registry.iter().count(), 0);
        let held: Vec<(&str, peersus_core::model::Standing, bool)> =
            loaded.held_back.iter().map(|h| (h.id.as_str(), h.standing, h.why.is_some())).collect();
        assert_eq!(
            held,
            [
                ("broken", peersus_core::model::Standing::Broken, true),
                ("swapped", peersus_core::model::Standing::Changed, false),
            ]
        );
    }

    #[test]
    fn one_plugin_that_cannot_run_never_stops_another_and_the_order_is_by_name() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "zeta", GOOD, b"x", 0o755)).unwrap();
        let other = GOOD.replace("thing.service", "other.service");
        manager.install(&package(parent.path(), "alpha", &other, b"y", 0o755)).unwrap();
        fs::write(manager.store().join("alpha").join(MANIFEST_FILE), b"broken").unwrap();
        let loaded = manager.load();
        assert_eq!(loaded.registry.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["zeta"]);
        assert_eq!(loaded.held_back.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["alpha"]);
    }

    #[test]
    fn the_stores_own_hidden_folders_are_not_plugins_and_are_neither_run_nor_reported() {
        // An install in progress and a package that was retired live in the store under names that start with a dot. They are
        // the store's own: not a plugin to run, not one that is held back, and not a problem to tell the person about.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"x", 0o755)).unwrap();
        for hidden in [".install-thing-1", ".retired-thing-2"] {
            let folder = package(manager.store(), hidden, &GOOD.replace("thing.service", "other.service"), b"y", 0o755);
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let loaded = manager.load();
        assert_eq!(loaded.registry.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["thing"]);
        assert!(loaded.held_back.is_empty(), "{:?}", loaded.held_back);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.installed.as_ref().map(|set| set.len()), Some(1), "a hidden folder was counted as installed");
    }

    #[test]
    fn a_directory_put_in_the_store_by_hand_is_not_run() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "installed", GOOD, b"x", 0o755))
            .unwrap();
        // A perfectly good package, but nobody approved it: put there by hand, so there is no approval in its folder. What
        // keeps it from running is that nobody approved it, which a record never was: the approval is in the folder.
        let other = GOOD.replace("thing.service", "other.service");
        let by_hand = package(manager.store(), "byhand", &other, b"y", 0o755);
        fs::set_permissions(&by_hand, fs::Permissions::from_mode(0o700)).unwrap();
        let loaded = manager.load();
        let ids: Vec<&str> = loaded.registry.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["installed"], "a plugin nobody approved was loaded");
        // It is not invisible: it is held back, saying what it waits for.
        assert!(
            loaded
                .held_back
                .iter()
                .any(|h| h.id.as_str() == "byhand" && h.standing == peersus_core::model::Standing::NeedsApproval),
            "{:?}",
            loaded.held_back
        );
    }

    #[test]
    fn what_stops_a_plugin_after_it_was_installed_is_its_program_its_right_to_run_or_a_guard_loosened_and_nothing_else() {
        // What the person approved is the program that runs and what each method of it is guarded by. Words that describe
        // it, and files added beside it, are not that: they can be rewritten by anyone and change nothing about what runs.
        use peersus_core::model::Standing;
        // (what was done, what it does to the plugin, whether it must still run)
        type Tampering = (&'static str, fn(&Path), Option<Standing>);
        let ways: [Tampering; 5] = [
            ("description edited", |d| {
                let text = fs::read_to_string(d.join(MANIFEST_FILE)).unwrap();
                fs::set_permissions(d.join(MANIFEST_FILE), fs::Permissions::from_mode(0o600)).unwrap();
                let changed = text.replace("Goes.", "Wins a billion.");
                assert_ne!(changed, text, "the change changed nothing, so the test proves nothing");
                fs::write(d.join(MANIFEST_FILE), changed).unwrap();
            }, None),
            ("guard removed", |d| {
                let text = fs::read_to_string(d.join(MANIFEST_FILE)).unwrap();
                fs::set_permissions(d.join(MANIFEST_FILE), fs::Permissions::from_mode(0o600)).unwrap();
                let changed = text.replace("\"capability\": \"thing.use\", ", "");
                assert_ne!(changed, text, "the change changed nothing, so the test proves nothing");
                fs::write(d.join(MANIFEST_FILE), changed).unwrap();
            }, Some(Standing::NeedsApproval)),
            ("program replaced", |d| {
                fs::write(d.join(PROGRAM_FILE), b"another program").unwrap();
            }, Some(Standing::Changed)),
            ("file added", |d| fs::write(d.join("extra"), b"x").unwrap(), None),
            ("runnable bit dropped", |d| {
                fs::set_permissions(d.join(PROGRAM_FILE), fs::Permissions::from_mode(0o600)).unwrap();
            }, Some(Standing::Broken)),
        ];
        for (name, change, stops) in ways {
            let (parent, data) = (tempfile::tempdir().unwrap(), data());
            let manager = Manager::new(data.path());
            manager
                .install(&package(parent.path(), "thing", GOOD, b"first", 0o755))
                .unwrap();
            let before = manager.load();
            assert_eq!(before.registry.iter().count(), 1, "{name}: not loaded at first");
            change(&manager.store().join("thing"));
            let after = manager.load();
            match stops {
                None => {
                    assert_eq!(after.registry.iter().count(), 1, "{name}: stopped a plugin that nothing about it had changed");
                    assert!(after.held_back.is_empty(), "{name}: {:?}", after.held_back);
                }
                Some(standing) => {
                    assert!(after.registry.is_empty(), "{name}: a plugin that changed was still loaded");
                    let held: Vec<Standing> = after.held_back.iter().map(|h| h.standing).collect();
                    assert_eq!(held, [standing], "{name}: {:?}", after.held_back);
                }
            }
        }
    }

    #[test]
    fn one_changed_plugin_does_not_stop_the_others() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "good", GOOD, b"x", 0o755))
            .unwrap();
        let other = GOOD.replace("thing.service", "other.service");
        manager
            .install(&package(parent.path(), "bad", &other, b"y", 0o755))
            .unwrap();
        fs::write(manager.store().join("bad").join(PROGRAM_FILE), b"swapped").unwrap();
        let loaded = manager.load();
        let ids: Vec<&str> = loaded.registry.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["good"]);
    }

    #[test]
    fn a_damaged_record_neither_stops_an_approved_plugin_nor_lets_an_unapproved_one_run() {
        // What was approved is in each plugin's own folder, so a record that is damaged says nothing about it: an approved
        // plugin still runs, and the opposite danger, a damaged record turning into permission for whatever is on disk, does not
        // arise, because a record never gave permission. A plugin nobody approved still does not run.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let other = GOOD.replace("thing.service", "other.service");
        let by_hand = package(manager.store(), "byhand", &other, b"y", 0o755);
        fs::set_permissions(&by_hand, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(data.path().join("installed.json"), b"damaged").unwrap();
        let loaded = manager.load();
        let ran: Vec<&str> = loaded.registry.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ran, ["thing"], "a damaged record stopped an approved plugin, or let an unapproved one run");
        assert_eq!(loaded.held_back.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["byhand"]);
    }

    #[test]
    fn starting_while_a_short_command_holds_the_store_still_loads_what_is_installed() {
        // The application loads once, at start, and never looks again. A `plugin-list` that
        // happens to be running at that instant holds the store for a few milliseconds; if
        // loading gave up instead of waiting, the application would run the whole session
        // with no plugins and a single line on stderr to say why.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let held = manager.lock().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(held);
        });
        let loaded = manager.load();
        release.join().unwrap();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.registry.iter().count(), 1, "nothing was loaded");
    }

    #[test]
    fn loading_does_not_wait_forever_for_a_store_that_is_never_released() {
        // The wait is bounded: a stuck process holding the store must not hang the start of
        // the application for good, so loading gives up after a while and says so.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let _held = manager.lock().unwrap();
        let began = std::time::Instant::now();
        let loaded = manager.load_waiting(std::time::Duration::from_millis(200));
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "it waited without bound"
        );
        assert!(loaded.registry.is_empty());
        assert!(
            loaded
                .problems
                .iter()
                .any(|p| p.reason == "The plugin store is in use"),
            "{:?}",
            loaded.problems
        );
    }

    #[test]
    fn two_managers_cannot_change_the_store_at_once() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let source = package(parent.path(), "thing", GOOD, b"x", 0o755);
        let held = manager.lock().unwrap();
        assert!(Manager::new(data.path()).install(&source).is_err());
        assert!(Manager::new(data.path()).list().is_err());
        drop(held);
        assert!(manager.install(&source).is_ok());
    }

    fn consent(directory: &Path, who: &str) -> peersus_net::grants::StandaloneGrants {
        use peersus_core::plugins::{Grants, PluginId};
        let mut store =
            peersus_net::grants::StandaloneGrants::open(directory.join("grants.json")).unwrap();
        store
            .set(
                PluginId::new(who).unwrap(),
                Grants::new(
                    [peersus_plugin_api::permission::Permission::new("host", "x").unwrap()],
                )
                .unwrap(),
            )
            .unwrap();
        store
    }
    fn holds(store: &peersus_net::grants::StandaloneGrants, who: &str) -> bool {
        store
            .get(&peersus_core::plugins::PluginId::new(who).unwrap())
            .is_some()
    }
    fn no_devices(directory: &Path) -> peersus_net::peers::Peers {
        peersus_net::peers::Peers::open(directory.join("peers.json")).unwrap()
    }

    #[test]
    fn what_was_granted_to_a_removed_plugin_is_forgotten_at_the_next_start() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let mut standalone = consent(data.path(), "thing");
        manager.remove("thing").unwrap();
        let found = manager.load();
        forget_removed(&found, &mut no_devices(data.path()), &mut standalone).unwrap();
        assert!(!holds(&standalone, "thing"));
    }

    #[test]
    fn what_was_granted_to_an_installed_plugin_survives_even_when_it_did_not_load() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let mut standalone = consent(data.path(), "thing");
        // Still installed, but no longer what was installed: it does not load, today.
        fs::write(manager.store().join("thing").join(PROGRAM_FILE), b"other").unwrap();
        let found = manager.load();
        assert!(found.registry.is_empty(), "the test needs it not to load");
        forget_removed(&found, &mut no_devices(data.path()), &mut standalone).unwrap();
        assert!(
            holds(&standalone, "thing"),
            "a plugin that is installed lost its consent because of a problem that can pass"
        );
    }

    #[test]
    fn when_who_is_installed_cannot_be_known_nothing_is_forgotten() {
        // The principle is unchanged: if it cannot be known who is installed, no permission is forgotten. What cannot be known
        // is now whether the STORE can be listed, since the store is where the plugins are. A store that cannot be read, or is
        // not there at all (an unmounted disk, a first run), says nothing about anyone, and so forgets nothing.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager
            .install(&package(parent.path(), "thing", GOOD, b"x", 0o755))
            .unwrap();
        let mut standalone = consent(data.path(), "thing");
        // Control: the store can be listed, so who is installed IS known and this plugin is among them.
        let known = manager.load();
        assert!(known.installed.as_ref().is_some_and(|set| set.iter().any(|id| id.as_str() == "thing")));
        // The store cannot be read: nothing is known about anyone.
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o000)).unwrap();
        let found = manager.load();
        fs::set_permissions(manager.store(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(found.installed.is_none(), "the test needs it to be unknown");
        forget_removed(&found, &mut no_devices(data.path()), &mut standalone).unwrap();
        assert!(holds(&standalone, "thing"), "a permission was forgotten for a store that could not be listed");
        // And a store that is not there at all is the same.
        let fresh = tempfile::tempdir().unwrap();
        let nothing = Manager::new(fresh.path()).load();
        assert!(nothing.installed.is_none(), "an absent store was taken as knowing who is installed");
    }

    #[test]
    fn what_was_shown_is_what_is_installed_the_digest_changes_when_anything_in_the_package_does() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("thing");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("plugin.json"), "{}").unwrap();
        std::fs::write(package.join("plugin"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(package.join("plugin"), std::fs::Permissions::from_mode(0o700)).unwrap();

        let shown = package_digest(&package).expect("a package that can be read");
        // The same package gives the same digest, so a confirmation of what was shown passes.
        assert_eq!(package_digest(&package).unwrap(), shown);

        // Changing the program between the look and the confirmation changes it.
        std::fs::write(package.join("plugin"), "#!/bin/sh\nrm -rf ~\n").unwrap();
        assert_ne!(package_digest(&package).unwrap(), shown, "a changed program went unnoticed");
        std::fs::write(package.join("plugin"), "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(package_digest(&package).unwrap(), shown);

        // So does making a file runnable that was not, and adding a file.
        std::fs::set_permissions(package.join("plugin"), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_ne!(package_digest(&package).unwrap(), shown, "a change of runnability went unnoticed");
        std::fs::set_permissions(package.join("plugin"), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(package.join("extra"), "x").unwrap();
        assert_ne!(package_digest(&package).unwrap(), shown, "an added file went unnoticed");
    }

    #[test]
    fn a_confirmation_passes_only_for_the_package_that_was_shown() {
        assert!(still_what_was_shown("abc", Ok("abc".into())));
        // Changed between the look and the confirmation: refused.
        assert!(!still_what_was_shown("abc", Ok("abd".into())));
        // Unreadable now: refused too, a package that cannot be read is not what was shown.
        assert!(!still_what_was_shown("abc", Err(Refusal("The package could not be read"))));
        // An empty digest on either side is never a match.
        assert!(!still_what_was_shown("", Ok(String::new())));
    }

    #[test]
    fn an_old_record_file_left_in_the_data_directory_changes_nothing() {
        // Earlier versions kept an installed.json. It is not read any more: the folder is the truth, so whatever it says,
        // and whether or not it is valid, a plugin that was installed now loads and is listed exactly as without it.
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"x", 0o755)).unwrap();
        for old in [&br#"[{"id":"thing","digest":"abc"}]"#[..], b"[]", b"not json {{{"] {
            fs::write(data.path().join("installed.json"), old).unwrap();
            let found = Manager::new(data.path()).load();
            assert!(found.problems.is_empty(), "{:?}", found.problems);
            assert_eq!(found.registry.iter().count(), 1);
            assert_eq!(ids(&manager.list().unwrap()), [("thing", Standing::Ready)]);
        }
        // And it is never written: what is in it is left exactly as the person has it.
        manager.install(&package(parent.path(), "other", &GOOD.replace("thing.service", "other.service"), b"x", 0o755)).unwrap();
        assert_eq!(fs::read(data.path().join("installed.json")).unwrap(), b"not json {{{");
    }

    #[test]
    fn a_plugin_that_was_disabled_stays_installed_but_is_not_started_after_a_restart() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"x", 0o755)).unwrap();
        let different = GOOD.replace("thing.service", "other.service");
        manager.install(&package(parent.path(), "other", &different, b"x", 0o755)).unwrap();

        manager.set_disabled("thing", true).expect("it is installed");
        // A fresh manager over the same directory is what a restart is.
        let found = Manager::new(data.path()).load();
        let installed = found.installed.expect("the record was read");
        let name = |s: &str| peersus_core::plugins::PluginId::new(s).unwrap();
        assert!(installed.contains(&name("thing")), "a disabled plugin stopped being installed");
        assert!(found.registry.find(&name("thing")).is_some(), "a disabled plugin stopped being listed");
        assert!(found.registry.find(&name("other")).is_some(), "the other plugin was not loaded");
        assert_eq!(found.disabled, [name("thing")].into(), "the disabled plugin was not reported");

        // Enabling it makes it start again at the next start, and nothing else changed.
        manager.set_disabled("thing", false).unwrap();
        let again = Manager::new(data.path()).load();
        assert!(again.registry.find(&name("thing")).is_some());
        assert!(again.disabled.is_empty());
        // Asking about something that is not installed is refused, not silently accepted.
        assert!(manager.set_disabled("nothing", true).is_err());
    }

    #[test]
    fn a_retired_package_can_be_put_back_exactly_as_it_was_or_dropped_for_good() {
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        manager.install(&package(parent.path(), "thing", GOOD, b"first", 0o755)).unwrap();
        manager.set_disabled("thing", true).unwrap();
        let before = destination_digest(&manager.store().join("thing")).unwrap();

        let retired = manager.retire("thing").expect("it was installed");
        // Out of use: not installed, no files at its name, and not loadable.
        assert!(manager.list().unwrap().is_empty(), "a retired package is still listed");
        assert!(!manager.store().join("thing").exists());
        assert!(retired.exists(), "what was retired is not kept");

        // Put back: the same files, the same record, and still switched off as it was.
        manager.restore("thing", &retired).expect("it can be put back");
        assert_eq!(destination_digest(&manager.store().join("thing")).unwrap(), before);
        assert_eq!(manager.list().unwrap().len(), 1);
        let again = Manager::new(data.path()).load();
        assert!(again.disabled.contains(&peersus_core::plugins::PluginId::new("thing").unwrap()));

        // Retiring and then dropping for good leaves nothing, and a new package may take the name.
        let retired = manager.retire("thing").unwrap();
        manager.drop_retired(&retired);
        assert!(!retired.exists(), "what was dropped is still there");
        manager.install(&package(parent.path(), "thing", GOOD, b"second", 0o755)).expect("the name is free");
        // A plugin that is not installed cannot be retired, and nothing is touched.
        assert!(manager.retire("nothing").is_err());
    }

    #[test]
    fn incompatible_platform_is_neither_installed_nor_run_and_existing_folder_remains_visible() {
        use peersus_plugin_api::platform::OperatingSystem;
        let Some(current) = OperatingSystem::current() else { return };
        let other = OperatingSystem::ALL.into_iter().find(|os| *os != current).unwrap();
        let (parent, data) = (tempfile::tempdir().unwrap(), data());
        let manager = Manager::new(data.path());
        let mut manifest: serde_json::Value = serde_json::from_str(GOOD).unwrap();
        manifest["platforms"] = serde_json::json!([other]);
        let source = package(parent.path(), "foreign", &manifest.to_string(), b"plugin", 0o755);
        assert_eq!(manager.install(&source), Err(Refusal("It does not support this operating system")));
        assert!(manager.load().registry.is_empty());
        manifest["platforms"] = serde_json::json!([current]);
        fs::write(source.join("plugin.json"), manifest.to_string()).unwrap();
        manager.install(&source).unwrap();
        assert_eq!(manager.load().registry.len(), 1);
        // A declaration changed on disk cannot make the plugin disappear or run on an unsupported OS.
        manifest["platforms"] = serde_json::json!([other]);
        fs::write(manager.store().join("foreign/plugin.json"), manifest.to_string()).unwrap();
        let loaded = manager.load();
        assert!(loaded.registry.is_empty());
        assert_eq!(loaded.held_back.len(), 1);
        assert_eq!(loaded.held_back[0].id.as_str(), "foreign");
        assert_eq!(loaded.held_back[0].why, Some("It does not support this operating system"));
        assert_eq!(manager.list().unwrap().len(), 1);
    }
}
