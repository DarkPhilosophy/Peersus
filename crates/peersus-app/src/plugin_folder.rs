//! Looking at one plugin's folder and saying what state it is in.
//!
//! This is the only place that reads a folder to answer that. It gathers what is on disk as plain facts
//! ([`crate::plugin_state::Found`]) and hands them to the pure rule ([`crate::plugin_state::classify`]). It never returns
//! an error for anything wrong with the folder: whatever is wrong is a fact, and a fact is a state, and every state has a
//! way out. A folder that cannot be read at all is broken; it is not an error the person is stuck behind.
//!
//! The manifest is read here only to learn what the plugin asks for and whether the application would accept it. What the
//! person approved is not in the manifest and never was: it is in `approval.json` (see [`crate::plugin_approval`]), and the
//! program is judged by its own digest, so rewriting the manifest changes nothing about whether the plugin may run.
#![cfg(unix)]

use crate::{
    discovery,
    plugin_approval::{self, Approval as Approved},
    plugin_host::check_manifest,
    plugin_state::{self, Approval, Folder, Found, Manifest, Program, State},
};
use std::{collections::BTreeSet, fs, path::Path};

/// The marker that says the person switched the plugin off. Its presence is the whole of the fact.
pub const DISABLED_MARKER: &str = "disabled";
/// Most bytes of a program that are measured. A plugin's program is a few megabytes; this leaves room above that and stops a
/// file of a different kind altogether from being read whole.
pub const MAX_PROGRAM_BYTES: u64 = 256 * 1024 * 1024;

/// What was learned of a plugin's folder, with what is needed to act on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Looked {
    pub state: State,
    /// What the manifest asks for, by name, when it could be read; empty otherwise.
    pub asks: BTreeSet<String>,
    /// What was approved, when an approval could be read; used to show the person what they agreed to.
    pub approved: Option<Approved>,
}

/// Look at the plugin folder `directory` and say what state it is in.
pub fn look(directory: &Path) -> Looked {
    // Keep a valid manifest even when its OS declaration excludes this host: unsupported
    // is not a corrupt manifest, and the window must be able to explain the distinction.
    let manifest = discovery::read_package(directory).ok();
    let manifest_state = match manifest.as_ref().map(check_manifest) {
        Some(Ok(())) => Manifest::Good,
        Some(Err(crate::plugin_host::RegistryError::UnsupportedPlatform)) => Manifest::UnsupportedPlatform,
        _ => Manifest::Unusable,
    };
    let asks: BTreeSet<String> = manifest.as_ref().map(|m| m.capabilities.keys().cloned().collect()).unwrap_or_default();
    // The program: the file the manifest names, held to the same rules as a program the application will run. Without a
    // usable manifest there is no name to look for, and the plugin is broken anyway.
    let program_name = manifest.as_ref().map(|m| m.program_file().to_owned());
    let program_path = program_name.as_ref().map(|name| directory.join(name));
    let program_fit = program_path.as_ref().zip(manifest.as_ref())
        .is_some_and(|(path, manifest)| discovery::check_program(path, manifest.execution).is_ok());
    let approved = plugin_approval::read(directory);
    let approval = match (&approved, &manifest, &program_name, &program_path) {
        (Some(approved), Some(manifest), Some(name), Some(path)) => {
            let digest = plugin_approval::program_digest(path, MAX_PROGRAM_BYTES);
            Approval::Given(plugin_approval::compare(approved, manifest, name, digest.as_deref(), &asks))
        }
        _ => Approval::None,
    };
    let disabled = fs::symlink_metadata(directory.join(DISABLED_MARKER)).is_ok();
    // The folder and the store it sits in, by the very rule a plugin has always been loaded under: only this user may be
    // able to change either, since what is run is whatever is at the program's path when it is started. A folder with no
    // store above it cannot be shown to be safe, and so is not. NOT HELD BY A TEST: the only path with no parent is the root,
    // which a test cannot be made to look at; a plugin's folder is always `<store>/<id>`, so the case does not arise in use.
    let folder_safe = directory.parent().is_some_and(|store| {
        discovery::check_directory(store).is_ok() && discovery::check_directory(directory).is_ok()
    });
    let found = Found {
        folder: if folder_safe { Folder::Safe } else { Folder::NotSafe },
        manifest: manifest_state,
        program: if program_fit { Program::Good } else { Program::Unusable },
        approval,
        disabled,
    };
    Looked { state: plugin_state::classify(&found), asks, approved }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_state::Why;
    use std::os::unix::fs::PermissionsExt;

    const MANIFEST: &str = r#"{
        "manifest_version": 1,
        "capabilities": {"thing.use": "Lets the thing do what it does."},
        "imports": [],
        "exports": [{
            "service": "thing.service", "version": 1, "description": "A test service.",
            "consumers": ["host"],
            "methods": {"act": {"capability": "thing.use", "description": "Acts."}},
            "events": {}
        }]
    }"#;

    /// A plugin's folder as it is in use: inside a store that only this user can change, holding a manifest and a runnable
    /// program called `plugin`. `path()` is the plugin's folder, so that a test reads as it always did.
    struct Plugin {
        _store: tempfile::TempDir,
        folder: std::path::PathBuf,
    }

    impl Plugin {
        fn path(&self) -> &Path {
            &self.folder
        }
    }

    fn folder() -> Plugin {
        let store = tempfile::tempdir().unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let folder = store.path().join("thing");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(folder.join("plugin.json"), MANIFEST).unwrap();
        let program = folder.join("plugin");
        fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        Plugin { _store: store, folder }
    }

    /// Approve what is in the folder now, the way installing does: by the very same function, so that what these tests call
    /// approved is what the product writes.
    fn approve(folder: &Path) {
        crate::plugin_manager::approve_copy(folder).expect("the folder could not be approved");
    }

    #[test]
    fn a_folder_with_a_manifest_and_a_program_and_no_approval_needs_approval() {
        let folder = folder();
        assert_eq!(look(folder.path()).state, State::NeedsApproval);
        // What it asks for is read so that the person can be shown it before approving.
        assert_eq!(look(folder.path()).asks, BTreeSet::from(["thing.use".to_owned()]));
    }

    #[test]
    fn once_approved_it_is_ready_and_may_run() {
        let folder = folder();
        approve(folder.path());
        let looked = look(folder.path());
        assert_eq!(looked.state, State::Ready);
        assert!(looked.state.may_run());
        assert!(looked.approved.is_some());
    }

    #[test]
    fn a_nonexecutable_module_becomes_ready_only_after_its_executor_is_approved() {
        use peersus_plugin_api::manifest::{Execution, EXECUTION_ABI_VERSION};
        for execution in [Execution::Wasm { abi: EXECUTION_ABI_VERSION }, Execution::NativeLibrary { abi: EXECUTION_ABI_VERSION }] {
            let folder = folder();
            let mut manifest: peersus_plugin_api::manifest::Manifest = serde_json::from_str(MANIFEST).unwrap();
            manifest.execution = execution;
            fs::write(folder.path().join("plugin.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            fs::set_permissions(folder.path().join("plugin"), fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(look(folder.path()).state, State::NeedsApproval);
            approve(folder.path());
            assert_eq!(look(folder.path()).state, State::Ready);
            manifest.execution = Execution::Process;
            fs::write(folder.path().join("plugin.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert!(!look(folder.path()).state.may_run(), "changing the executor must never inherit a module approval");
        }
    }

    #[test]
    fn rewriting_the_manifest_changes_nothing_about_whether_it_may_run() {
        // The case that started this: the manifest was rewritten after approval and the plugin was shut out. What was
        // approved is the program and what it asks for, so a manifest with another description, other whitespace, or
        // another order changes nothing.
        let folder = folder();
        approve(folder.path());
        fs::write(
            folder.path().join("plugin.json"),
            MANIFEST.replace("Lets the thing do what it does.", "A new description, written later.").replace("  ", "\t"),
        )
        .unwrap();
        assert_eq!(look(folder.path()).state, State::Ready, "rewriting the manifest shut the plugin out");
    }

    #[test]
    fn a_manifest_that_now_asks_for_more_needs_approval_again() {
        let folder = folder();
        approve(folder.path());
        fs::write(
            folder.path().join("plugin.json"),
            MANIFEST
                .replace(
                    r#""capabilities": {"thing.use": "Lets the thing do what it does."}"#,
                    r#""capabilities": {"thing.use": "x", "more.use": "Asks for more."}"#,
                ),
        )
        .unwrap();
        assert_eq!(look(folder.path()).state, State::NeedsApproval);
    }

    #[test]
    fn a_program_that_is_not_the_approved_one_is_changed() {
        let folder = folder();
        approve(folder.path());
        let program = folder.path().join("plugin");
        fs::write(&program, b"#!/bin/sh\necho replaced\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(look(folder.path()).state, State::Changed);
        assert!(!look(folder.path()).state.may_run());
    }

    #[test]
    fn a_file_added_beside_the_program_changes_nothing() {
        let folder = folder();
        approve(folder.path());
        fs::write(folder.path().join("notes.txt"), b"anything").unwrap();
        fs::create_dir(folder.path().join("data")).unwrap();
        assert_eq!(look(folder.path()).state, State::Ready);
    }

    #[test]
    fn a_lost_or_damaged_approval_costs_a_question_and_never_a_block() {
        let folder = folder();
        approve(folder.path());
        fs::write(folder.path().join(plugin_approval::FILE), b"damaged {{{").unwrap();
        assert_eq!(look(folder.path()).state, State::NeedsApproval);
        fs::remove_file(folder.path().join(plugin_approval::FILE)).unwrap();
        assert_eq!(look(folder.path()).state, State::NeedsApproval);
        // And approving again puts it right.
        approve(folder.path());
        assert_eq!(look(folder.path()).state, State::Ready);
    }

    #[test]
    fn a_plugin_the_person_switched_off_is_disabled_and_enabling_it_is_taking_the_marker_away() {
        let folder = folder();
        approve(folder.path());
        fs::write(folder.path().join(DISABLED_MARKER), b"").unwrap();
        assert_eq!(look(folder.path()).state, State::Disabled);
        fs::remove_file(folder.path().join(DISABLED_MARKER)).unwrap();
        assert_eq!(look(folder.path()).state, State::Ready);
    }

    #[test]
    fn a_folder_with_no_usable_manifest_is_broken_and_says_which_part() {
        let folder = folder();
        approve(folder.path());
        fs::write(folder.path().join("plugin.json"), b"not a manifest").unwrap();
        assert_eq!(look(folder.path()).state, State::Broken(Why::NoManifest));
        fs::remove_file(folder.path().join("plugin.json")).unwrap();
        assert_eq!(look(folder.path()).state, State::Broken(Why::NoManifest));
        // A manifest that parses but that the application refuses is not usable either.
        fs::write(folder.path().join("plugin.json"), MANIFEST.replace("\"manifest_version\": 1", "\"manifest_version\": 99")).unwrap();
        assert_eq!(look(folder.path()).state, State::Broken(Why::NoManifest));
    }

    #[test]
    fn a_folder_with_no_program_fit_to_run_is_broken() {
        for (what, change) in [
            ("no program", (|p: &Path| fs::remove_file(p).unwrap()) as fn(&Path)),
            ("not runnable", |p| fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap()),
            ("others can change it", |p| fs::set_permissions(p, fs::Permissions::from_mode(0o777)).unwrap()),
        ] {
            let folder = folder();
            approve(folder.path());
            change(&folder.path().join("plugin"));
            assert_eq!(look(folder.path()).state, State::Broken(Why::NoProgram), "{what}");
        }
    }

    #[test]
    fn a_folder_that_others_can_change_is_broken_even_when_everything_in_it_was_approved() {
        // What is run is whatever is at the program's path when it is started, and anyone who can write the folder can put
        // another program there after this was looked at. So a folder that group or others can write is never ready, however
        // well approved: the rules are the ones a plugin has always been loaded under.
        for mode in [0o770, 0o707, 0o777, 0o720] {
            let folder = folder();
            approve(folder.path());
            assert_eq!(look(folder.path()).state, State::Ready, "the control was not ready, so what follows proves nothing");
            fs::set_permissions(folder.path(), fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(look(folder.path()).state, State::Broken(Why::NotSafe), "mode {mode:o} was trusted");
        }
        // A folder only its owner can change, and one others can read but not change, are fine.
        for mode in [0o700, 0o750, 0o755] {
            let folder = folder();
            approve(folder.path());
            fs::set_permissions(folder.path(), fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(look(folder.path()).state, State::Ready, "mode {mode:o} was refused");
        }
    }

    #[test]
    fn a_store_that_others_can_change_makes_a_perfect_private_folder_broken() {
        // The folder is private and everything in it is approved. But anyone who can change the STORE it sits in can rename
        // the whole folder away and put another in its place, which is as good as changing the program. So the store is
        // held to the same rule as the folder, and a plugin in a store others can change is never ready.
        // NOT HELD BY A TEST: a store or folder that belongs to someone else. That is told by the owner's id, which a test
        // cannot make different without being root; the check is the same call that decides the mode.
        for mode in [0o770, 0o707, 0o777, 0o720] {
            let plugin = folder();
            approve(plugin.path());
            assert_eq!(look(plugin.path()).state, State::Ready, "the control was not ready, so what follows proves nothing");
            let store = plugin.path().parent().unwrap();
            fs::set_permissions(store, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(look(plugin.path()).state, State::Broken(Why::NotSafe), "a store of mode {mode:o} was trusted");
        }
        // A store only this user can change, whoever may read it, is fine.
        for mode in [0o700, 0o750, 0o755] {
            let plugin = folder();
            approve(plugin.path());
            fs::set_permissions(plugin.path().parent().unwrap(), fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(look(plugin.path()).state, State::Ready, "a store of mode {mode:o} was refused");
        }
    }

    #[test]
    fn a_folder_that_cannot_be_read_at_all_is_broken_and_is_never_an_error() {
        // Whatever is wrong, the answer is a state, never a failure the person is stuck behind. In a store only this user can
        // change, a folder that is not there, and a file where a folder should be, are both broken, and which words say why
        // is whatever the first check that fails says (a folder that cannot be looked at is not shown to be safe).
        let store = tempfile::tempdir().unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let missing = look(&store.path().join("missing")).state;
        assert!(matches!(missing, State::Broken(_)), "a missing folder was {missing:?}");
        let file = store.path().join("afile");
        fs::write(&file, b"x").unwrap();
        let not_a_folder = look(&file).state;
        assert!(matches!(not_a_folder, State::Broken(_)), "a file in place of a folder was {not_a_folder:?}");
        // Neither may run, and neither is an error.
        assert!(!missing.may_run() && !not_a_folder.may_run());
        // In a place others can write (a shared temporary directory), the same is true, and it is the folder's being unsafe
        // that is said.
        let shared = tempfile::tempdir().unwrap();
        fs::set_permissions(shared.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(look(&shared.path().join("missing")).state, State::Broken(Why::NotSafe));
    }

    #[test]
    fn a_manifest_that_names_another_program_is_judged_by_that_program() {
        let folder = folder();
        fs::write(
            folder.path().join("plugin.json"),
            MANIFEST.replace("\"imports\": []", "\"imports\": [], \"program\": \"peersus-thing\""),
        )
        .unwrap();
        // The program it names is not there: broken, whatever file called `plugin` lies beside it.
        assert_eq!(look(folder.path()).state, State::Broken(Why::NoProgram));
        let named = folder.path().join("peersus-thing");
        fs::write(&named, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&named, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(look(folder.path()).state, State::NeedsApproval);
    }
}
