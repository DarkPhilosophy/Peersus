//! Finding the plugins a user has installed: one directory each, a manifest and a program.
//!
//! ```text
//! <root>/<id>/plugin.json   the manifest, as the plugin declares it
//! <root>/<id>/plugin        the program the application will start
//! ```
//!
//! Nothing here starts anything, and nothing here decides what a plugin may do: the manifest is
//! only read, and the registry that receives it applies every rule it applies to a plugin built
//! in code. What this decides is narrower, and about the files themselves: whether it is safe to
//! believe them. The plugin's id is the name of its directory, because a manifest has no say in
//! who it is; the program is the file next to the manifest, never a path the manifest names; and
//! a program that someone other than its owner could have changed is not run.
//!
//! One plugin that cannot be loaded is reported and skipped. It never stops the others, and
//! what is reported is a fixed sentence and the directory's name, never anything read from a
//! file, because what a file says is the plugin's own text.
use crate::plugin_host::{PluginSpec, Registry};
use peersus_api::manifest::Manifest;
use peersus_api::package::Package;
use peersus_core::plugins::PluginId;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Name of the file holding a plugin's manifest.
pub const MANIFEST_FILE: &str = "plugin.json";
/// Name of the program inside a plugin's directory.
pub const PROGRAM_FILE: &str = peersus_api::manifest::DEFAULT_PROGRAM;
/// Largest manifest read, in bytes. A manifest is a few kilobytes; anything near this is not one.
pub const MAX_MANIFEST: u64 = 64 * 1024;
/// Most plugin directories looked at. More than this is more than a person installs.
pub const MAX_PLUGINS: usize = 64;
/// Fixed text for a manifest that does not declare an executor, without echoing package text.
pub(crate) const MISSING_EXECUTION: &str = "Its manifest must explicitly declare `execution`";

/// A plugin that was not loaded, and why, in words this application chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The directory's name, which is the only thing about it that is shown.
    pub plugin: String,
    pub reason: &'static str,
}

/// What was found: the plugins that load, and the ones that did not.
#[derive(Default)]
pub struct Discovered {
    pub registry: Registry,
    pub problems: Vec<Problem>,
    /// Every plugin that is INSTALLED, loaded or not, when the record of installations was read
    /// successfully; `None` when it could not be (the store was in use, the record unreadable)
    /// and by default. A plugin can be installed and still absent from `registry`, for a reason
    /// that is gone on the next start, so this and not `registry` is what says who has been
    /// removed. `None` says nothing about anyone and must never be read as "no plugins".
    pub installed: Option<std::collections::BTreeSet<peersus_core::plugins::PluginId>>,
    /// Plugins that are installed and switched off: not in `registry`, and not a problem. The
    /// application lists them as off and starts none of them.
    pub disabled: std::collections::BTreeSet<peersus_core::plugins::PluginId>,
    /// Plugins that were found in the store and are NOT run, with what is wrong or what they wait for: not approved, changed,
    /// broken. A plugin is never missing from the window for being one of these; it is listed with its standing and can be
    /// approved or removed. Switched-off ones are in `disabled` and `registry`, as before, not here.
    pub held_back: Vec<HeldBack>,
}

/// A plugin that was found and is not run.
#[derive(Clone, Debug, PartialEq)]
pub struct HeldBack {
    pub id: peersus_core::plugins::PluginId,
    pub standing: peersus_core::model::Standing,
    /// What it declares, when its manifest could be read and accepted; empty when not, since there is then nothing to show.
    pub manifest: peersus_api::manifest::Manifest,
    /// When it is broken, the fixed sentence that says why. Nothing read from the folder is ever repeated.
    pub why: Option<&'static str>,
    /// The digest of its program as it was when it was found, when it could be measured: what the person is shown and may
    /// approve. Approving is refused if the program is no longer this one.
    pub program: Option<String>,
}

/// Read every plugin under `root`. A missing `root` is simply no plugins.
pub fn discover(root: &Path) -> Discovered {
    let mut found = Discovered::default();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    let mut directories: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    // A fixed order, so that which of two conflicting plugins loads does not depend on the
    // order the filesystem happened to list them in.
    directories.sort();
    for directory in directories.into_iter().take(MAX_PLUGINS) {
        let name = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match load(root, &directory) {
            Ok(spec) => {
                if let Err(error) = found.registry.register(spec) {
                    found.problems.push(Problem {
                        plugin: name,
                        reason: refusal(&error),
                    });
                }
            }
            Err(reason) => found.problems.push(Problem {
                plugin: name,
                reason,
            }),
        }
    }
    found
}

fn refusal(error: &crate::plugin_host::RegistryError) -> &'static str {
    use crate::plugin_host::RegistryError::*;
    match error {
        Duplicate => "Another plugin already has this name",
        ServiceConflict => "It offers a service another plugin already offers",
        Manifest => "Its manifest is not valid",
        ActionUnreachable => "One of its actions could never be reached",
        ReceiveWithoutCapability => "It takes things in without a permission to guard them",
        UnsupportedPlatform => "It does not support this operating system",
    }
}

/// Whether only this user can rename or replace what is inside `directory`.
///
/// Checking a program checks that file, but what is run is whatever is at that path when it
/// is started, and anyone who can write the directory holding it can rename another file into
/// its place in between. So the directories on the way are held to the same standard as the
/// program: owned by this user, and not writable by group or others. A directory that is
/// itself a link is not looked through.
#[cfg(unix)]
pub(crate) fn check_directory(directory: &Path) -> Result<(), &'static str> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(directory).map_err(|_| "It could not be read")?;
    if !metadata.is_dir() {
        return Err("It is not a directory");
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err("Its directory can be changed by others");
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("Its directory belongs to someone else");
    }
    Ok(())
}

/// Windows. The plugin folders are made by the application inside this user's own data folder in
/// the profile, whose inherited access list gives full control to this user (and the system and
/// administrators, who own the machine anyway) and nothing to other users; that inheritance is
/// what keeps others from renaming something into place, as the owner and mode bits do on Unix.
/// What can still redirect a path is a link or junction, so a directory that is a reparse point is
/// refused and never looked through.
#[cfg(windows)]
pub(crate) fn check_directory(directory: &Path) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| "It could not be read")?;
    if !metadata.is_dir() {
        return Err("It is not a directory");
    }
    if is_reparse_point(&metadata) {
        return Err("It is not a directory");
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_REPARSE_POINT`: a symbolic link, junction or other redirection.
    const REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn check_directory(_directory: &Path) -> Result<(), &'static str> {
    Err("Plugins cannot be run on this system")
}

/// Whether the package in `directory`, inside `root`, is one `discover` would load.
///
/// The plugin's id is the name of its directory, which the manifest has no say in.
fn load(root: &Path, directory: &Path) -> Result<PluginSpec, &'static str> {
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Its name is not usable")?;
    load_as(root, directory, name)
}

/// Whether the package in `directory`, inside `root`, is one `discover` would load if its id
/// were `name`: the same rules, in one place, so that what installing accepts and what loading
/// accepts cannot drift apart.
///
/// `plugin_manager` uses this on a copy it has staged under a name of its own, where the id
/// the package will have is not the staging directory's name.
pub(crate) fn load_as(
    root: &Path,
    directory: &Path,
    name: &str,
) -> Result<PluginSpec, &'static str> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| "It could not be read")?;
    // A link could point anywhere, including somewhere another user can write.
    if !metadata.is_dir() {
        return Err("It is not a directory");
    }
    // A leading dot is refused as the other applications of this family refuse it, and
    // because a directory that starts with one is how the plugin manager marks what it is
    // staging or retiring: a plugin named that way would run without ever being listed.
    if name.starts_with('.') {
        return Err("Its name is not usable");
    }
    let id = PluginId::new(name).map_err(|_| "Its name is not usable")?;
    // Both the directory and the root it sits in: whoever can change either can put another
    // program where this one was checked, after it has been checked.
    check_directory(root)?;
    check_directory(directory)?;
    let manifest = read_manifest(&manifest_file(directory)?)?;
    crate::plugin_host::check_external_execution(manifest.execution)?;
    let program = check_program(&directory.join(manifest.program_file()), manifest.execution)?;
    Ok(PluginSpec {
        id,
        manifest,
        program: Some(program),
        arguments: Vec::new(),
        restart_policy: None,
        languages: crate::package_languages::load(directory),
    })
}

/// The ways a manifest may be written, as the name of its file. Exactly one of them is in a package.
pub const MANIFEST_FILES: [&str; 2] = ["plugin.toml", MANIFEST_FILE];

/// Which file in `directory` is its manifest: the ONE place that says. Exactly one of the names in
/// [`MANIFEST_FILES`] may be there. A package that has both is refused, not read in some order:
/// which of two files is the manifest is not something to leave to chance, because a person looks at
/// one and the application would run the other. A package with neither has no manifest.
pub(crate) fn manifest_file(directory: &Path) -> Result<PathBuf, &'static str> {
    let mut found = MANIFEST_FILES
        .iter()
        .map(|name| directory.join(name))
        .filter(|path| fs::symlink_metadata(path).is_ok());
    match (found.next(), found.next()) {
        (Some(only), None) => Ok(only),
        (Some(_), Some(_)) => Err("It has two manifests"),
        (None, _) => Err("It has no manifest"),
    }
}

/// Read `bytes` as the manifest a file called `name` holds: TOML for `.toml`, JSON otherwise. The
/// same types read both, so both refuse a field nobody declared and both mean the same thing.
/// Anything that cannot be read as the manifest it claims to be is refused, never read as the other.
///
/// A `package` block (name, version, author, link: text to SHOW, never a right) is taken out first and
/// validated by [`peersus_api::package::split`], then the rest is read as a manifest, which
/// still refuses any key it does not know. The block is returned beside the manifest so the store can
/// compare it with the catalog and with what the program itself carries.
pub(crate) fn parse_package_manifest(
    name: &str,
    bytes: &[u8],
) -> Result<(Manifest, Option<Package>), &'static str> {
    const INVALID: &str = "Its manifest is not valid";
    // Both formats become one JSON value so the one `split` validates both.
    let json = if name.ends_with(".toml") {
        let text = std::str::from_utf8(bytes).map_err(|_| INVALID)?;
        let value: toml::Value = toml::from_str(text).map_err(|_| INVALID)?;
        serde_json::to_vec(&value).map_err(|_| INVALID)?
    } else {
        bytes.to_vec()
    };
    let split = peersus_api::package::split(&json).map_err(|error| match error {
        peersus_api::package::SplitError::MissingExecution => MISSING_EXECUTION,
        _ => INVALID,
    })?;
    let manifest = serde_json::from_slice(&split.manifest).map_err(|_| INVALID)?;
    Ok((manifest, split.package))
}

/// [`parse_package_manifest`] without the package block, for the callers that only need what a plugin may do.
pub(crate) fn parse_manifest(name: &str, bytes: &[u8]) -> Result<Manifest, &'static str> {
    parse_package_manifest(name, bytes).map(|(manifest, _)| manifest)
}

/// The one bounded reader of a manifest file: checks it is a plain file, reads at most
/// [`MAX_MANIFEST`] bytes, and returns the manifest with its `package` block, if it has one.
fn read_manifest_and_package(path: &Path) -> Result<(Manifest, Option<Package>), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "It has no manifest")?;
    if !metadata.is_file() {
        return Err("Its manifest is not a file");
    }
    let mut text = Vec::new();
    fs::File::open(path)
        .map_err(|_| "Its manifest could not be read")?
        // One byte past the limit, so that a file exactly at it is read whole and one over is
        // seen to be over, without ever reading the rest of a huge one.
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut text)
        .map_err(|_| "Its manifest could not be read")?;
    if text.len() as u64 > MAX_MANIFEST {
        return Err("Its manifest is too large");
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    parse_package_manifest(name, &text)
}

fn read_manifest(path: &Path) -> Result<Manifest, &'static str> {
    read_manifest_and_package(path).map(|(manifest, _)| manifest)
}

/// What a package in `directory` says it is and what it asks for, read so that the person can see
/// it BEFORE it is installed. It reads one file, the manifest, and nothing else: it does not look
/// at the program and it runs nothing. Installing is a separate step with its own checks.
pub(crate) fn read_package(directory: &Path) -> Result<Manifest, &'static str> {
    read_package_and_info(directory).map(|(manifest, _)| manifest)
}

/// [`read_package`], and the `package` block the manifest carries (name, version, author, link), if any.
pub(crate) fn read_package_and_info(
    directory: &Path,
) -> Result<(Manifest, Option<Package>), &'static str> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| "It could not be read")?;
    if !metadata.is_dir() {
        return Err("It is not a directory");
    }
    read_manifest_and_package(&manifest_file(directory)?)
}

/// A read-only package shipped beside the application, not an installed or approved plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub id: String,
    pub source: PathBuf,
}

/// Find canonical packages relative to the selected executable, never the working directory
/// or PATH. The caller runs this disk work off its rendering thread. Installation still
/// goes through StageInstall/ConfirmInstall and grants nothing automatically.
pub fn bundles_near(executable: &Path) -> Vec<Bundle> {
    let Some(prefix) = executable.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    let root = prefix.join("share/peersus/plugins");
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .take(MAX_PLUGINS)
        .filter_map(|source| {
            let id = source.file_name()?.to_str()?.to_owned();
            PluginId::new(&id).ok()?;
            let manifest = read_package(&source).ok()?;
            crate::plugin_host::check_manifest(&manifest).ok()?;
            let metadata = fs::symlink_metadata(source.join(manifest.program_file())).ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some(Bundle { id, source })
        })
        .collect()
}

#[cfg(unix)]
pub(crate) fn check_program(
    path: &Path,
    execution: peersus_api::manifest::Execution,
) -> Result<PathBuf, &'static str> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path).map_err(|_| "It has no program")?;
    if !metadata.is_file() {
        return Err("Its program is not a file");
    }
    let mode = metadata.permissions().mode();
    #[cfg(target_os = "android")]
    if matches!(
        execution,
        peersus_api::manifest::Execution::NativeLibrary { .. }
    ) && mode & 0o222 != 0
    {
        return Err("An installed native module must be read-only");
    }
    if matches!(execution, peersus_api::manifest::Execution::Process) && mode & 0o111 == 0 {
        return Err("Its program cannot be run");
    }
    // Someone else able to change the program could change what the application runs.
    if mode & 0o022 != 0 {
        return Err("Its program can be changed by others");
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("Its program belongs to someone else");
    }
    fs::canonicalize(path).map_err(|_| "It has no program")
}

/// Windows: see [`check_directory`] for why the folder's inherited access list stands in for the
/// Unix owner and mode checks. A program that is a link is refused rather than followed.
#[cfg(windows)]
pub(crate) fn check_program(
    path: &Path,
    _execution: peersus_api::manifest::Execution,
) -> Result<PathBuf, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "It has no program")?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err("Its program is not a file");
    }
    fs::canonicalize(path).map_err(|_| "It has no program")
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn check_program(
    _path: &Path,
    _execution: peersus_api::manifest::Execution,
) -> Result<PathBuf, &'static str> {
    Err("Plugins cannot be run on this system")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A manifest that exports one guarded service to the application, which is valid.
    pub(super) const GOOD: &str = r#"{
        "manifest_version": 1, "execution": {"kind": "process"},
        "capabilities": {"thing.use": "Lets the thing do what it does."},
        "imports": [],
        "exports": [{
            "service": "thing.service", "version": 1, "description": "A test service.",
            "consumers": ["host"],
            "methods": {"act": {"capability": "thing.use", "description": "Acts."}},
            "events": {}
        }]
    }"#;

    fn install(root: &Path, name: &str, manifest: &str, mode: u32) -> PathBuf {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(MANIFEST_FILE), manifest).unwrap();
        let program = directory.join(PROGRAM_FILE);
        fs::write(&program, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(mode)).unwrap();
        directory
    }

    fn names(found: &Discovered) -> Vec<String> {
        found
            .registry
            .iter()
            .map(|spec| spec.id.as_str().to_owned())
            .collect()
    }

    fn reasons(found: &Discovered) -> Vec<(String, &'static str)> {
        found
            .problems
            .iter()
            .map(|p| (p.plugin.clone(), p.reason))
            .collect()
    }

    #[test]
    fn an_installed_plugin_is_found_with_its_name_manifest_and_program() {
        let root = tempfile::tempdir().unwrap();
        let directory = install(root.path(), "thing", GOOD, 0o755);
        let found = discover(root.path());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(names(&found), ["thing"]);
        let spec = found.registry.iter().next().unwrap();
        assert_eq!(
            spec.program,
            Some(fs::canonicalize(directory.join(PROGRAM_FILE)).unwrap())
        );
        assert_eq!(spec.manifest.exports[0].service, "thing.service");
    }

    #[test]
    fn module_packages_are_data_not_executables_but_keep_ownership_checks() {
        use peersus_api::manifest::{EXECUTION_ABI_VERSION, Execution};
        for execution in [
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            },
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            },
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut manifest: Manifest = serde_json::from_str(GOOD).unwrap();
            manifest.execution = execution;
            let directory = install(
                root.path(),
                "thing",
                &serde_json::to_string(&manifest).unwrap(),
                0o600,
            );
            let found = discover(root.path());
            assert!(
                found.problems.is_empty(),
                "a private non-executable module was refused: {:?}",
                found.problems
            );
            assert_eq!(names(&found), ["thing"]);
            for mode in [0o620, 0o602] {
                fs::set_permissions(
                    directory.join(PROGRAM_FILE),
                    fs::Permissions::from_mode(mode),
                )
                .unwrap();
                assert!(
                    discover(root.path()).registry.is_empty(),
                    "a module writable by others was admitted"
                );
            }
        }
    }

    #[test]
    fn no_directory_is_simply_no_plugins() {
        let root = tempfile::tempdir().unwrap();
        let found = discover(&root.path().join("does-not-exist"));
        assert!(found.registry.is_empty() && found.problems.is_empty());
        let empty = discover(root.path());
        assert!(empty.registry.is_empty() && empty.problems.is_empty());
    }

    #[test]
    fn one_plugin_that_cannot_load_is_reported_and_the_others_still_load() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "aaa", GOOD, 0o755);
        install(root.path(), "bad", "{ not json", 0o755);
        // A second good plugin must not be affected by the one between them.
        let other = GOOD.replace("thing", "other");
        install(root.path(), "zzz", &other, 0o755);
        let found = discover(root.path());
        assert_eq!(names(&found), ["aaa", "zzz"]);
        assert_eq!(
            reasons(&found),
            [("bad".to_owned(), "Its manifest is not valid")]
        );
    }

    #[test]
    fn the_id_is_the_name_of_the_directory_and_a_bad_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "has space", GOOD, 0o755);
        install(root.path(), "ünï", &GOOD.replace("thing", "x"), 0o755);
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(found.problems.len(), 2);
        assert!(
            found
                .problems
                .iter()
                .all(|p| p.reason == "Its name is not usable")
        );
    }

    #[test]
    fn a_manifest_cannot_name_the_program_or_anything_else_it_was_not_given_a_place_for() {
        let root = tempfile::tempdir().unwrap();
        // Fields this application does not know are refused, not ignored.
        let sneaky = GOOD.replace(
            r#""manifest_version": 1,"#,
            r#""manifest_version": 1, "program": "/bin/sh", "id": "host","#,
        );
        install(root.path(), "thing", &sneaky, 0o755);
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(
            reasons(&found),
            [("thing".to_owned(), "Its manifest is not valid")]
        );
    }

    #[test]
    fn discovery_reports_a_missing_executor_without_loading_the_plugin() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(GOOD).unwrap();
        manifest
            .as_object_mut()
            .unwrap()
            .remove("execution")
            .unwrap();
        install(root.path(), "thing", &manifest.to_string(), 0o755);
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(reasons(&found), [("thing".to_owned(), MISSING_EXECUTION)]);
    }

    #[test]
    fn a_manifest_over_the_limit_is_refused_without_being_read_whole() {
        let root = tempfile::tempdir().unwrap();
        let padding = " ".repeat(MAX_MANIFEST as usize);
        install(root.path(), "thing", &format!("{GOOD}{padding}"), 0o755);
        let found = discover(root.path());
        assert_eq!(
            reasons(&found),
            [("thing".to_owned(), "Its manifest is too large")]
        );
    }

    #[test]
    fn a_manifest_exactly_at_the_limit_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let padding = " ".repeat(MAX_MANIFEST as usize - GOOD.len());
        install(root.path(), "thing", &format!("{GOOD}{padding}"), 0o755);
        let found = discover(root.path());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(names(&found), ["thing"]);
    }

    // NOT COVERED: that a program or a directory owned by someone else is refused ("Its program
    // belongs to someone else", "Its directory belongs to someone else"). Every file a test can
    // make belongs to the user running the test, and giving one to another user takes
    // privileges tests must not rely on. Removing either check leaves every test here passing
    // (checked by mutation for the program), so they rest on review alone.
    #[test]
    fn a_program_that_cannot_run_or_that_others_could_change_is_not_run() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "plain", GOOD, 0o644);
        install(root.path(), "groupw", &GOOD.replace("thing", "g"), 0o775);
        install(root.path(), "worldw", &GOOD.replace("thing", "w"), 0o757);
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        let mut got = reasons(&found);
        got.sort();
        assert_eq!(
            got,
            [
                ("groupw".to_owned(), "Its program can be changed by others"),
                ("plain".to_owned(), "Its program cannot be run"),
                ("worldw".to_owned(), "Its program can be changed by others"),
            ]
        );
    }

    #[test]
    fn a_link_in_place_of_a_program_manifest_or_directory_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        install(target.path(), "real", GOOD, 0o755);
        // The whole plugin directory is a link.
        std::os::unix::fs::symlink(target.path().join("real"), root.path().join("linked")).unwrap();
        // The program is a link.
        let d = install(root.path(), "proglink", &GOOD.replace("thing", "p"), 0o755);
        fs::remove_file(d.join(PROGRAM_FILE)).unwrap();
        std::os::unix::fs::symlink("/bin/sh", d.join(PROGRAM_FILE)).unwrap();
        // The manifest is a link.
        let d = install(root.path(), "manlink", &GOOD.replace("thing", "m"), 0o755);
        fs::remove_file(d.join(MANIFEST_FILE)).unwrap();
        std::os::unix::fs::symlink(
            target.path().join("real").join(MANIFEST_FILE),
            d.join(MANIFEST_FILE),
        )
        .unwrap();
        let found = discover(root.path());
        assert!(found.registry.is_empty(), "{:?}", names(&found));
        let mut got = reasons(&found);
        got.sort();
        assert_eq!(
            got,
            [
                ("linked".to_owned(), "It is not a directory"),
                ("manlink".to_owned(), "Its manifest is not a file"),
                ("proglink".to_owned(), "Its program is not a file"),
            ]
        );
    }

    #[test]
    fn a_plugin_with_a_missing_manifest_or_program_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let d = install(root.path(), "noman", GOOD, 0o755);
        fs::remove_file(d.join(MANIFEST_FILE)).unwrap();
        let d = install(root.path(), "noprog", &GOOD.replace("thing", "n"), 0o755);
        fs::remove_file(d.join(PROGRAM_FILE)).unwrap();
        let found = discover(root.path());
        let mut got = reasons(&found);
        got.sort();
        assert_eq!(
            got,
            [
                ("noman".to_owned(), "It has no manifest"),
                ("noprog".to_owned(), "It has no program"),
            ]
        );
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_program_that_is_safe_in_a_directory_others_can_change_is_not_run() {
        // The file itself is the user's and 0755, but whoever can write its directory can
        // rename something else into its place after it has been checked.
        for mode in [0o777, 0o775, 0o757, 0o770] {
            let root = tempfile::tempdir().unwrap();
            let directory = install(root.path(), "thing", GOOD, 0o755);
            set_mode(&directory, mode);
            let found = discover(root.path());
            assert!(
                found.registry.is_empty(),
                "directory mode {mode:o} was trusted"
            );
            assert_eq!(
                reasons(&found),
                [("thing".to_owned(), "Its directory can be changed by others")],
                "directory mode {mode:o}"
            );
            set_mode(&directory, 0o755); // so the temporary directory can be removed
        }
    }

    #[test]
    fn a_directory_whose_name_starts_with_a_dot_is_not_run() {
        // The other applications of this family refuse such an id, and here a leading dot is
        // what marks a directory the plugin manager is staging or retiring, so a plugin named
        // that way would be invisible to `list` yet still run.
        let root = tempfile::tempdir().unwrap();
        for name in [".hidden", ".install-thing-1", ".retired-thing-1", "."] {
            if name == "." {
                continue;
            }
            install(root.path(), name, GOOD, 0o755);
        }
        let found = discover(root.path());
        assert!(found.registry.is_empty(), "{:?}", names(&found));
        assert_eq!(found.problems.len(), 3);
        assert!(
            found
                .problems
                .iter()
                .all(|p| p.reason == "Its name is not usable"),
            "{:?}",
            found.problems
        );
    }

    #[test]
    fn dots_inside_a_name_are_fine() {
        // The control: only a LEADING dot is refused, so a reverse-domain id still loads.
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "example.live-agent", GOOD, 0o755);
        let found = discover(root.path());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(names(&found), ["example.live-agent"]);
    }

    #[test]
    fn the_same_plugin_in_a_directory_only_its_owner_can_change_is_run() {
        // The control for the test above: nothing differs but the directory's mode.
        for mode in [0o700, 0o750, 0o755, 0o555] {
            let root = tempfile::tempdir().unwrap();
            let directory = install(root.path(), "thing", GOOD, 0o755);
            set_mode(&directory, mode);
            let found = discover(root.path());
            assert!(
                found.problems.is_empty(),
                "directory mode {mode:o}: {:?}",
                found.problems
            );
            assert_eq!(names(&found), ["thing"], "directory mode {mode:o}");
            set_mode(&directory, 0o755);
        }
    }

    #[test]
    fn a_root_that_others_can_change_is_not_trusted_either() {
        // Whoever can write the root can rename a whole plugin directory out and another in.
        for mode in [0o777, 0o775, 0o757] {
            let root = tempfile::tempdir().unwrap();
            install(root.path(), "thing", GOOD, 0o755);
            set_mode(root.path(), mode);
            let found = discover(root.path());
            assert!(found.registry.is_empty(), "root mode {mode:o} was trusted");
            assert_eq!(
                reasons(&found),
                [("thing".to_owned(), "Its directory can be changed by others")],
                "root mode {mode:o}"
            );
            set_mode(root.path(), 0o755);
        }
    }

    #[test]
    fn a_root_only_its_owner_can_change_is_trusted() {
        for mode in [0o700, 0o750, 0o755, 0o555] {
            let root = tempfile::tempdir().unwrap();
            install(root.path(), "thing", GOOD, 0o755);
            set_mode(root.path(), mode);
            let found = discover(root.path());
            assert!(
                found.problems.is_empty(),
                "root mode {mode:o}: {:?}",
                found.problems
            );
            assert_eq!(names(&found), ["thing"], "root mode {mode:o}");
            set_mode(root.path(), 0o755);
        }
    }

    #[test]
    fn the_registry_still_applies_its_own_rules_to_what_is_found() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "first", GOOD, 0o755);
        // The same service from a second plugin conflicts, and the one found first keeps it.
        install(root.path(), "second", GOOD, 0o755);
        let found = discover(root.path());
        assert_eq!(names(&found), ["first"]);
        assert_eq!(
            reasons(&found),
            [(
                "second".to_owned(),
                "It offers a service another plugin already offers"
            )]
        );
    }

    #[test]
    fn what_is_reported_is_never_text_read_from_a_file() {
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "thing",
            r#"{"manifest_version": 1, "execution": {"kind": "process"}, "secret_note": "hunter2 /home/alex/.ssh"}"#,
            0o755,
        );
        let found = discover(root.path());
        let shown = format!("{:?}", found.problems);
        assert!(
            !shown.contains("hunter2") && !shown.contains(".ssh"),
            "{shown}"
        );
    }

    #[test]
    fn only_the_plugins_up_to_the_limit_are_looked_at() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..MAX_PLUGINS + 5 {
            let name = format!("p{n:03}");
            install(root.path(), &name, &GOOD.replace("thing", &name), 0o755);
        }
        let found = discover(root.path());
        assert_eq!(found.registry.len() + found.problems.len(), MAX_PLUGINS);
    }

    /// The same manifest as `GOOD`, written as TOML.
    const GOOD_TOML: &str = r#"
        manifest_version = 1
        execution = { kind = "process" }
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

    /// A package whose manifest is written under `file`, with a runnable program.
    fn install_as(root: &Path, name: &str, file: &str, manifest: &str) -> PathBuf {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(file), manifest).unwrap();
        let program = directory.join(PROGRAM_FILE);
        fs::write(&program, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        directory
    }

    #[test]
    fn a_package_whose_manifest_is_toml_is_found_and_means_what_the_json_one_means() {
        let root = tempfile::tempdir().unwrap();
        install_as(root.path(), "thing", "plugin.toml", GOOD_TOML);
        let found = discover(root.path());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(names(&found), ["thing"]);

        // The same words in JSON give the very same manifest: one meaning, two ways to write it.
        let other = tempfile::tempdir().unwrap();
        install_as(other.path(), "thing", "plugin.json", GOOD);
        let as_json = discover(other.path());
        assert_eq!(
            found.registry.iter().next().unwrap().manifest,
            as_json.registry.iter().next().unwrap().manifest
        );
    }

    #[test]
    fn a_package_with_both_manifests_is_refused_and_neither_is_guessed() {
        // Someone sees one and the other is run: which of two is the manifest cannot be left to
        // chance or to an order, so a package that has both is not loaded at all.
        let root = tempfile::tempdir().unwrap();
        let directory = install_as(root.path(), "thing", "plugin.toml", GOOD_TOML);
        fs::write(directory.join("plugin.json"), GOOD).unwrap();
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(
            reasons(&found),
            [("thing".to_owned(), "It has two manifests")]
        );
    }

    #[test]
    fn a_toml_manifest_that_is_damaged_is_reported_and_does_not_fall_back_to_a_json_one() {
        let root = tempfile::tempdir().unwrap();
        install_as(root.path(), "thing", "plugin.toml", "this = is = not toml");
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(
            reasons(&found),
            [("thing".to_owned(), "Its manifest is not valid")]
        );
    }

    #[test]
    fn a_toml_manifest_with_a_field_nobody_declared_is_refused_as_a_json_one_is() {
        let root = tempfile::tempdir().unwrap();
        let strict = format!("{GOOD_TOML}\nsurprise = true\n");
        install_as(root.path(), "thing", "plugin.toml", &strict);
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(
            reasons(&found),
            [("thing".to_owned(), "Its manifest is not valid")]
        );
    }

    #[test]
    fn a_package_block_is_read_in_both_formats_and_changes_nothing_about_what_the_plugin_may_do() {
        let json_block = r#""package": {"name": "Thing", "version": "1.2.3", "author": "Someone", "link": "https://example.org/thing"},"#;
        let with_json = GOOD.replacen('{', &format!("{{{json_block}"), 1);
        let toml_block = "[package]\nname = \"Thing\"\nversion = \"1.2.3\"\nauthor = \"Someone\"\nlink = \"https://example.org/thing\"\n";
        let with_toml = format!("{GOOD_TOML}\n{toml_block}");
        let (plain, none) = parse_package_manifest("plugin.json", GOOD.as_bytes()).unwrap();
        assert_eq!(none, None, "no block, no package");
        for (file, text) in [("plugin.json", with_json), ("plugin.toml", with_toml)] {
            let (manifest, package) = parse_package_manifest(file, text.as_bytes())
                .unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(
                manifest, plain,
                "{file}: the block must not alter the manifest"
            );
            let package = package.unwrap_or_else(|| panic!("{file}: the block was dropped"));
            assert_eq!(
                (
                    package.name.as_str(),
                    package.version.as_str(),
                    package.author.as_str()
                ),
                ("Thing", "1.2.3", "Someone")
            );
            assert_eq!(package.link, "https://example.org/thing");
        }
    }

    #[test]
    fn a_package_block_that_claims_an_identity_or_an_unsafe_link_refuses_the_whole_package() {
        let hostile = [
            r#""package": {"name": "T", "version": "1.0.0", "id": "clipboard"},"#,
            r#""package": {"name": "T", "version": "1.0.0", "link": "http://example.org"},"#,
            r#""package": {"name": "T", "version": "1.0.0", "link": "https://u:p@example.org/x"},"#,
            r#""package": {"name": "T", "version": "1.0.0", "link": "javascript:alert(1)"},"#,
            r#""package": {"name": "T", "version": "latest"},"#,
            r#""package": {"name": "T", "version": "1.0.0", "surprise": true},"#,
        ];
        for block in hostile {
            let text = GOOD.replacen('{', &format!("{{{block}"), 1);
            assert_eq!(
                parse_package_manifest("plugin.json", text.as_bytes()).unwrap_err(),
                "Its manifest is not valid",
                "{block}"
            );
        }
        // A misspelt top-level key is not swallowed by the block handling.
        let misspelt = GOOD.replacen('{', r#"{"packages": {"name": "T", "version": "1.0.0"},"#, 1);
        assert!(parse_package_manifest("plugin.json", misspelt.as_bytes()).is_err());
    }

    #[test]
    fn a_manifest_too_large_to_be_one_is_refused_whichever_way_it_is_written() {
        for (file, text) in [("plugin.json", GOOD), ("plugin.toml", GOOD_TOML)] {
            let root = tempfile::tempdir().unwrap();
            let padding = " ".repeat(MAX_MANIFEST as usize);
            install_as(root.path(), "thing", file, &format!("{text}{padding}"));
            let found = discover(root.path());
            assert_eq!(
                reasons(&found),
                [("thing".to_owned(), "Its manifest is too large")],
                "{file}"
            );
        }
    }

    #[test]
    fn a_manifest_that_is_a_link_is_refused_whichever_way_it_is_written() {
        for file in ["plugin.json", "plugin.toml"] {
            let root = tempfile::tempdir().unwrap();
            let directory = install_as(root.path(), "thing", "other-name", GOOD);
            std::os::unix::fs::symlink(directory.join("other-name"), directory.join(file)).unwrap();
            let found = discover(root.path());
            assert!(found.registry.is_empty(), "{file} as a link was loaded");
        }
    }

    /// `GOOD`, naming its program.
    fn good_with_program(name: &str) -> String {
        GOOD.replacen('{', &format!("{{\"program\":{name:?},"), 1)
    }

    #[test]
    fn a_package_whose_manifest_names_its_program_runs_that_file_and_is_listed_by_its_name() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("thing");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(MANIFEST_FILE),
            good_with_program("peersus-thing"),
        )
        .unwrap();
        let program = directory.join("peersus-thing");
        fs::write(&program, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

        let found = discover(root.path());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        let spec = found.registry.iter().next().unwrap();
        // What is started is the named file, so the operating system lists the process by that name.
        assert_eq!(spec.program, Some(fs::canonicalize(&program).unwrap()));
        assert_eq!(
            spec.program.as_ref().unwrap().file_name().unwrap(),
            "peersus-thing"
        );
    }

    #[test]
    fn when_the_manifest_names_a_program_the_file_called_plugin_is_not_it() {
        // Only a file called `plugin` is there, and the manifest says the program is another file:
        // it is refused, not run in its place.
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "thing",
            &good_with_program("peersus-thing"),
            0o755,
        );
        let found = discover(root.path());
        assert!(found.registry.is_empty());
        assert_eq!(reasons(&found), [("thing".to_owned(), "It has no program")]);
    }

    #[test]
    fn a_program_that_is_a_link_or_not_runnable_or_changeable_by_others_is_refused_under_its_own_name()
     {
        for (what, mode, link) in [
            ("not runnable", 0o644, false),
            ("changeable by others", 0o777, false),
            ("a link", 0o755, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("thing");
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(MANIFEST_FILE),
                good_with_program("peersus-thing"),
            )
            .unwrap();
            let program = directory.join("peersus-thing");
            if link {
                fs::write(root.path().join("elsewhere"), b"#!/bin/sh\n").unwrap();
                fs::set_permissions(
                    root.path().join("elsewhere"),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                std::os::unix::fs::symlink(root.path().join("elsewhere"), &program).unwrap();
            } else {
                fs::write(&program, b"#!/bin/sh\n").unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(mode)).unwrap();
            }
            let found = discover(root.path());
            assert!(found.registry.is_empty(), "{what} was loaded");
        }
    }
}

#[cfg(all(test, unix))]
mod read_package_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(dir: &Path, manifest: &str) {
        fs::write(dir.join(MANIFEST_FILE), manifest).unwrap();
        fs::write(dir.join(PROGRAM_FILE), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(dir.join(PROGRAM_FILE), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn reading_a_package_gives_what_it_asks_for_and_refuses_what_it_cannot_read() {
        let root = tempfile::tempdir().unwrap();
        let good = root.path().join("clipboard");
        fs::create_dir(&good).unwrap();
        write(&good, super::tests::GOOD);
        let manifest = read_package(&good).expect("a readable package");
        assert!(manifest.capabilities.contains_key("thing.use"));

        // A directory without a manifest, a manifest that is not valid, and something that is not a
        // directory at all are refused, each with a reason in the application's own words.
        let bare = root.path().join("bare");
        fs::create_dir(&bare).unwrap();
        assert_eq!(read_package(&bare).unwrap_err(), "It has no manifest");
        let broken = root.path().join("broken");
        fs::create_dir(&broken).unwrap();
        fs::write(broken.join(MANIFEST_FILE), "not json").unwrap();
        assert_eq!(
            read_package(&broken).unwrap_err(),
            "Its manifest is not valid"
        );
        let file = root.path().join("afile");
        fs::write(&file, "x").unwrap();
        assert_eq!(read_package(&file).unwrap_err(), "It is not a directory");
    }

    #[test]
    fn bundled_packages_are_only_offered_and_never_treated_as_installed_or_approved() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("share/peersus/plugins/example");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join(MANIFEST_FILE), super::tests::GOOD).unwrap();
        fs::write(package.join(PROGRAM_FILE), b"program").unwrap();
        let executable = root.path().join("bin/peersus-ui");
        let available = bundles_near(&executable);
        assert_eq!(
            available,
            vec![Bundle {
                id: "example".into(),
                source: package.clone()
            }]
        );
        assert!(!package.join(crate::plugin_approval::FILE).exists());
        assert!(!root.path().join("data/plugins").exists());
        fs::write(package.join(MANIFEST_FILE), b"broken").unwrap();
        assert!(bundles_near(&executable).is_empty());
    }
}

/// The starting point offered to extension authors must be one the application accepts: a template whose manifest the
/// install-time parser refuses is a broken promise to everyone who copies it.
#[cfg(test)]
mod template_tests {
    use super::*;

    const TEMPLATE_MANIFEST: &[u8] = include_bytes!("../../../templates/hello-guest/plugin.json");
    const TEMPLATE_CARGO: &str = include_str!("../../../templates/hello-guest/Cargo.toml");

    #[test]
    fn the_template_manifest_is_one_the_application_accepts_and_asks_for_nothing() {
        let (manifest, package) = parse_package_manifest("plugin.json", TEMPLATE_MANIFEST)
            .expect("the template manifest parses");
        assert!(
            package.is_some_and(|package| package.validate()),
            "its package block is valid"
        );
        assert!(
            manifest.validate().is_ok(),
            "the application's own rules accept it"
        );
        assert!(
            manifest.imports.is_empty(),
            "a starting point must not ask the application for anything"
        );
        assert!(
            manifest.capabilities.is_empty(),
            "so installing it grants nothing"
        );
        assert!(
            peersus_api::platform::OperatingSystem::ALL
                .iter()
                .all(|system| manifest.supports(*system)),
            "a starting point installs everywhere: a restriction would refuse it for no reason an author could see"
        );
        assert!(
            matches!(
                manifest.execution,
                peersus_api::manifest::Execution::Wasm { abi: 1 }
            ),
            "it is a WebAssembly guest of the current interface"
        );
    }

    /// The file the manifest names as its program is the one the crate really builds: a crate named one thing and a
    /// manifest naming another would install without a program.
    #[test]
    fn the_template_manifest_names_the_file_its_crate_builds() {
        let (manifest, _) = parse_package_manifest("plugin.json", TEMPLATE_MANIFEST).unwrap();
        let name = TEMPLATE_CARGO
            .lines()
            .find_map(|line| line.strip_prefix("name = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("the crate has a name");
        assert_eq!(
            manifest.program_file(),
            format!("{}.wasm", name.replace('-', "_"))
        );
    }
}
