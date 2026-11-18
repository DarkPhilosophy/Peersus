//! What the person approved for one plugin, kept in the plugin's own folder.
//!
//! `<store>/<id>/approval.json` says which program was approved (the digest of its bytes and of whether it can be run) and
//! which capabilities the person saw and accepted. It is written by the application when the person approves, and it is
//! read every time the plugin is looked at, to see whether what is on disk is still what was approved.
//!
//! LOSING IT COSTS A QUESTION, NEVER A BLOCK. Whatever is wrong with the file, whether it is absent, unreadable, too large,
//! not valid, a link, or says something the application does not understand, it reads as "no approval", and the plugin is
//! asked about again. Nothing here returns an error to the person for a damaged file: a state the person cannot leave is
//! the one thing this exists to avoid.
//!
//! WHAT IT CAN AND CANNOT DO. A plugin is started as the same user, with no sandbox, so a plugin can rewrite its own program
//! and this file. The digest here detects a change that nobody announced (a build that overwrote the program, a bad copy);
//! it is not a guarantee against a plugin that means to change itself.
use crate::plugin_state::Given;
use crate::private_fs;
use peersus_api::manifest::{Execution, Manifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

/// The name of the file, inside the plugin's folder.
pub const FILE: &str = "approval.json";
/// Most bytes of the file that are read. An approval is a few hundred bytes; anything near this is not one.
pub const MAX_BYTES: u64 = 64 * 1024;
/// Most capabilities one approval may name.
pub const MAX_CAPABILITIES: usize = 256;

/// What was approved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    /// The file name of the program that was approved.
    pub program: String,
    /// Omitted legacy records approved executable processes, never a module executor.
    #[serde(default)]
    pub execution: Execution,
    /// Hex SHA-256 over the program's bytes and whether it can be run.
    pub sha256: String,
    /// The capabilities the person saw and accepted, by name.
    pub capabilities: BTreeSet<String>,
    /// What guarded each method when the person approved: `service/method` to the capability that guards it, or `None`
    /// for a method open to whoever may call it. Capability names alone do not say whether a plugin has opened itself up.
    pub guards: BTreeMap<String, Option<String>>,
    /// When it was approved, in Unix seconds. For the person to read; nothing decides by it.
    pub approved_at: u64,
}

/// The digest of a program: its bytes, and whether it can be run, so that making a file runnable changes it as much as
/// changing a byte does. Nothing else about the file counts, not its name, its time, or what is beside it.
///
/// `None` if the program cannot be read as a plain file or is larger than `limit`.
pub fn program_digest(program: &Path, limit: u64) -> Option<String> {
    let metadata = fs::symlink_metadata(program).ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    // What is measured is the file that is there, never what a link points to.
    let file = private_fs::read_no_follow(program).ok()?;
    let mut bytes = Vec::new();
    // One byte past the limit, so that a file that grew since it was measured is seen to be over. NOT HELD BY A TEST: the
    // size asked just above already refuses any plain file over the limit, so this only matters for a file that grows
    // between that look and this read, which cannot be made to happen on demand. It costs nothing and is left in.
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(&bytes);
    hash.update([u8::from(private_fs::is_runnable(&metadata))]);
    Some(hex::encode(hash.finalize()))
}

/// What the folder `directory` holds of an approval, or `None` for anything at all that is wrong with it.
pub fn read(directory: &Path) -> Option<Approval> {
    let path = directory.join(FILE);
    let metadata = fs::symlink_metadata(&path).ok()?;
    // Never a link, and only a plain file: what is read is what is there.
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let file = private_fs::read_no_follow(&path).ok()?;
    let mut text = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut text).ok()?;
    if text.len() as u64 > MAX_BYTES {
        return None;
    }
    let approval: Approval = serde_json::from_slice(&text).ok()?;
    // What is held to the same rules as anything else the application reads: bounded, and names that are only names.
    if approval.capabilities.len() > MAX_CAPABILITIES
        || approval.sha256.len() != 64
        || !approval.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || approval.program.is_empty()
        || approval.program.len() > 64
        || approval.program.contains(['/', '\\', '\0'])
    {
        return None;
    }
    Some(approval)
}

/// Write the approval for the plugin in `directory`: all at once or not at all (a temporary file moved into place), and
/// readable by the owner only.
pub fn write(directory: &Path, approval: &Approval) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(approval).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    peersus_net::atomic::write_private(&directory.join(FILE), &bytes)
}

/// What an approval, if there is one, says of the plugin NOW: whether the program in `program` is the one that was
/// approved, and whether every capability in `asked` was among those approved. Nothing here reads the folder: it is given
/// the digest that is there now (`None` if there is none to be had).
///
/// Capabilities are compared by NAME. The text that describes a capability is for the person to read and anyone may
/// rewrite it, so a changed description asks nothing new; a capability that was not approved before does.
pub fn compare(
    approved: &Approval,
    manifest: &Manifest,
    program_name: &str,
    digest_now: Option<&str>,
    asked: &BTreeSet<String>,
) -> Given {
    Given {
        program_unchanged: approved.program == program_name
            && approved.execution == manifest.execution
            && digest_now == Some(approved.sha256.as_str()),
        covers_what_is_asked: covers(approved, manifest, asked),
    }
}

/// What guards each method of each service a manifest exports, as `(service, method) -> the capability that guards it, if
/// any`. A method with no guard is open to whoever may call it, which is why `None` is recorded and not left out.
pub fn guards_of(manifest: &Manifest) -> BTreeMap<String, Option<String>> {
    manifest
        .exports
        .iter()
        .flat_map(|export| {
            export.methods.iter().map(|(method, member)| {
                (
                    format!("{}/{method}", export.service),
                    member.capability.clone(),
                )
            })
        })
        .collect()
}

/// Whether what a manifest asks for now is covered by `approved`: every capability asked was approved, and no method is
/// guarded more weakly than it was.
///
/// Capability NAMES alone are not enough. A manifest may drop the guard from a method, or swap it for another, and still
/// declare the very same names, so a plugin could open itself up and look unchanged. The guard of every method is
/// therefore compared: a method that was approved must have the guard it had; a method that is new must be guarded by a
/// capability that was approved (open to anyone is a new way in, and asks). The words that describe a capability are not
/// compared: they are for a person to read and anyone may rewrite them.
pub fn covers(approved: &Approval, manifest: &Manifest, asked: &BTreeSet<String>) -> bool {
    if !asked.is_subset(&approved.capabilities) {
        return false;
    }
    guards_of(manifest)
        .iter()
        .all(|(method, guard_now)| match approved.guards.get(method) {
            // Approved before: the guard it had is the guard it must still have.
            Some(guard_then) => guard_then == guard_now,
            // New: it must be guarded, and by something that was approved.
            None => guard_now
                .as_ref()
                .is_some_and(|guard| approved.capabilities.contains(guard)),
        })
}

// The cases use Unix modes and links; the Windows rules are covered in `private_fs`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn caps(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn approval(program: &str, sha: &str, names: &[&str]) -> Approval {
        Approval {
            program: program.to_owned(),
            execution: Execution::Process,
            sha256: sha.to_owned(),
            capabilities: caps(names),
            guards: BTreeMap::new(),
            approved_at: 1_700_000_000,
        }
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn program(directory: &Path, bytes: &[u8], mode: u32) -> std::path::PathBuf {
        let path = directory.join("plug");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn manifest_with(method: &str) -> peersus_api::manifest::Manifest {
        // One export with one method; `method` is the rest of that method's JSON (its guard and its description).
        let text = format!(
            r#"{{"manifest_version":1,"capabilities":{{"thing.use":"Lets the thing.","other.use":"Another."}},"imports":[],
            "exports":[{{"service":"thing.service","version":1,"description":"d","consumers":["host"],
            "methods":{{"go":{{{method}}}}},"events":{{}}}}]}}"#
        );
        serde_json::from_str(&text).unwrap()
    }

    fn approval_of(manifest: &peersus_api::manifest::Manifest) -> Approval {
        Approval {
            program: "plug".to_owned(),
            execution: manifest.execution,
            sha256: SHA.to_owned(),
            capabilities: manifest.capabilities.keys().cloned().collect(),
            guards: guards_of(manifest),
            approved_at: 1,
        }
    }

    #[test]
    fn changing_executor_requires_new_approval_even_when_bytes_and_permissions_match() {
        use peersus_api::manifest::{EXECUTION_ABI_VERSION, Execution};
        let mut manifest = manifest_with(r#""capability":"thing.use","description":"Goes.""#);
        let approved = approval_of(&manifest);
        let asked = manifest.capabilities.keys().cloned().collect();
        assert!(compare(&approved, &manifest, "plug", Some(SHA), &asked).program_unchanged);
        for execution in [
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            },
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            },
        ] {
            manifest.execution = execution;
            assert!(
                !compare(&approved, &manifest, "plug", Some(SHA), &asked).program_unchanged,
                "an executable approval must not authorize a different executor"
            );
        }
    }

    #[test]
    fn a_plugin_that_loosens_its_own_guards_is_asked_about_again_and_a_changed_description_is_not()
    {
        let guarded = manifest_with(r#""capability":"thing.use","description":"Goes.""#);
        let approved = approval_of(&guarded);
        let asks = |m: &peersus_api::manifest::Manifest| {
            m.capabilities.keys().cloned().collect::<BTreeSet<_>>()
        };
        let covered = |m: &peersus_api::manifest::Manifest| covers(&approved, m, &asks(m));
        // The same manifest is covered, and so is one with only its words changed: the text is for a person and anyone may
        // rewrite it.
        assert!(covered(&guarded));
        assert!(covered(&manifest_with(
            r#""capability":"thing.use","description":"Wins a billion.""#
        )));
        // The guard removed: the method is open to anyone, which is less safe than what was approved, and capability NAMES
        // are unchanged, so only the guards can tell.
        let unguarded = manifest_with(r#""description":"Goes.""#);
        assert_eq!(
            asks(&unguarded),
            asks(&guarded),
            "the capability names alone do not tell, which is the point"
        );
        assert!(
            !covered(&unguarded),
            "a method lost its guard and the plugin still counted as approved"
        );
        // The guard swapped for another capability: a different question, not one the application may answer.
        assert!(!covered(&manifest_with(
            r#""capability":"other.use","description":"Goes.""#
        )));
    }

    #[test]
    fn a_method_that_is_new_must_be_guarded_by_what_was_approved_or_it_asks_again() {
        let one = manifest_with(r#""capability":"thing.use","description":"Goes.""#);
        let approved = approval_of(&one);
        let with_second = |second: &str| {
            let text = format!(
                r#"{{"manifest_version":1,"capabilities":{{"thing.use":"Lets the thing.","other.use":"Another."}},"imports":[],
                "exports":[{{"service":"thing.service","version":1,"description":"d","consumers":["host"],
                "methods":{{"go":{{"capability":"thing.use","description":"Goes."}},"more":{{{second}}}}},"events":{{}}}}]}}"#
            );
            serde_json::from_str::<peersus_api::manifest::Manifest>(&text).unwrap()
        };
        let asks = |m: &peersus_api::manifest::Manifest| {
            m.capabilities.keys().cloned().collect::<BTreeSet<_>>()
        };
        // New and guarded by something already approved: nothing more is asked of anyone.
        let guarded = with_second(r#""capability":"thing.use","description":"More.""#);
        assert!(covers(&approved, &guarded, &asks(&guarded)));
        // New and open to anyone: a new way in, so it asks.
        let open = with_second(r#""description":"More.""#);
        assert!(!covers(&approved, &open, &asks(&open)));
        // New and guarded by a capability that was not approved at all.
        let narrower = Approval {
            capabilities: caps(&["thing.use"]),
            ..approved.clone()
        };
        let by_other = with_second(r#""capability":"other.use","description":"More.""#);
        assert!(!covers(&narrower, &by_other, &asks(&by_other)));
    }

    #[test]
    fn what_is_written_is_what_is_read_back_and_is_private() {
        let folder = tempfile::tempdir().unwrap();
        let wrote = approval("plug", SHA, &["clipboard.use", "net.use"]);
        write(folder.path(), &wrote).unwrap();
        assert_eq!(read(folder.path()), Some(wrote));
        let mode = fs::metadata(folder.path().join(FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the approval is readable by others");
        // Written again, it is replaced whole, and no temporary file is left in the folder.
        let again = approval("plug", SHA, &["clipboard.use"]);
        write(folder.path(), &again).unwrap();
        assert_eq!(read(folder.path()), Some(again));
        let left: Vec<_> = fs::read_dir(folder.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            [std::ffi::OsString::from(FILE)],
            "something besides the approval was left behind"
        );
    }

    #[test]
    fn installed_legacy_approval_without_executor_still_authorizes_only_processes() {
        let folder = tempfile::tempdir().unwrap();
        let approved = approval("plug", SHA, &[]);
        let mut legacy = serde_json::to_value(&approved).unwrap();
        legacy.as_object_mut().unwrap().remove("execution");
        fs::write(
            folder.path().join(FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert_eq!(read(folder.path()), Some(approved));
    }

    #[test]
    fn anything_wrong_with_the_file_reads_as_no_approval_and_never_as_an_error() {
        let folder = tempfile::tempdir().unwrap();
        // Nothing there at all.
        assert_eq!(read(folder.path()), None);
        let file = folder.path().join(FILE);
        let good = serde_json::to_string(&approval("plug", SHA, &["a.b"])).unwrap();
        for (what, text) in [
            ("empty", String::new()),
            ("not json", "this is not json {{{".to_owned()),
            ("not utf-8", "\u{fffd}".to_owned()),
            ("a list", "[]".to_owned()),
            (
                "a field nobody declared",
                good.replace("\"program\"", "\"extra\":1,\"program\""),
            ),
            ("a short digest", good.replace(SHA, "abc")),
            (
                "a digest that is not hex",
                good.replace(SHA, &"z".repeat(64)),
            ),
            (
                "a program that is a path",
                good.replace("\"plug\"", "\"../plug\""),
            ),
            ("a program with no name", good.replace("\"plug\"", "\"\"")),
            (
                "too large",
                format!("{good}{}", " ".repeat(MAX_BYTES as usize)),
            ),
        ] {
            fs::write(&file, text).unwrap();
            assert_eq!(read(folder.path()), None, "{what} was read as an approval");
        }
        fs::write(&file, &good).unwrap();
        assert!(
            read(folder.path()).is_some(),
            "the control was not read, so the cases above prove nothing"
        );
    }

    #[test]
    fn a_link_or_a_directory_in_place_of_the_file_is_no_approval_and_is_never_followed() {
        let folder = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        // A perfectly good approval, but somewhere else, with the folder's file a link to it.
        write(elsewhere.path(), &approval("plug", SHA, &["a.b"])).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join(FILE), folder.path().join(FILE)).unwrap();
        assert_eq!(read(folder.path()), None, "a link was followed");
        fs::remove_file(folder.path().join(FILE)).unwrap();
        fs::create_dir(folder.path().join(FILE)).unwrap();
        assert_eq!(
            read(folder.path()),
            None,
            "a directory was read as an approval"
        );
    }

    #[test]
    fn the_digest_of_a_program_changes_with_a_byte_and_with_being_runnable_and_with_nothing_else() {
        let folder = tempfile::tempdir().unwrap();
        let path = program(folder.path(), b"#!/bin/sh\necho one\n", 0o700);
        let base = program_digest(&path, 1 << 20).unwrap();
        assert_eq!(base.len(), 64);
        // The same bytes, again: the same digest.
        assert_eq!(program_digest(&path, 1 << 20).unwrap(), base);
        // Another file beside it, a different name for it, a different time: none of it counts.
        fs::write(folder.path().join("readme.txt"), b"anything").unwrap();
        let renamed = folder.path().join("renamed");
        fs::rename(&path, &renamed).unwrap();
        assert_eq!(
            program_digest(&renamed, 1 << 20).unwrap(),
            base,
            "the name or what is beside it changed the digest"
        );
        // One byte different.
        let changed = program(folder.path(), b"#!/bin/sh\necho two\n", 0o700);
        assert_ne!(
            program_digest(&changed, 1 << 20).unwrap(),
            base,
            "a changed byte was not noticed"
        );
        // The same bytes but no longer runnable.
        let unrunnable = program(folder.path(), b"#!/bin/sh\necho one\n", 0o600);
        assert_ne!(
            program_digest(&unrunnable, 1 << 20).unwrap(),
            base,
            "losing the right to run was not noticed"
        );
    }

    #[test]
    fn a_program_that_cannot_be_measured_has_no_digest() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(
            program_digest(&folder.path().join("missing"), 1 << 20),
            None
        );
        assert_eq!(
            program_digest(folder.path(), 1 << 20),
            None,
            "a directory was measured"
        );
        let path = program(folder.path(), &[7; 100], 0o700);
        assert!(
            program_digest(&path, 100).is_some(),
            "a file exactly at the limit is not over it"
        );
        assert_eq!(
            program_digest(&path, 99),
            None,
            "a file over the limit was measured"
        );
        // A link is not followed: its target is a different file, and the digest says nothing of it.
        let link = folder.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(program_digest(&link, 1 << 20), None, "a link was followed");
    }

    #[test]
    fn comparing_says_whether_the_program_is_the_approved_one_and_whether_everything_asked_was_approved()
     {
        // A manifest with no methods, so that only the names and the program are in question here (the guards of methods
        // have their own tests).
        let none = Manifest::default();
        let approved = approval("plug", SHA, &["clipboard.use"]);
        let same = compare(
            &approved,
            &none,
            "plug",
            Some(SHA),
            &caps(&["clipboard.use"]),
        );
        assert_eq!(
            same,
            Given {
                program_unchanged: true,
                covers_what_is_asked: true
            }
        );
        // Asking for less than was approved is covered.
        assert!(compare(&approved, &none, "plug", Some(SHA), &caps(&[])).covers_what_is_asked);
        // Asking for something that was not approved is not.
        assert!(
            !compare(
                &approved,
                &none,
                "plug",
                Some(SHA),
                &caps(&["clipboard.use", "net.use"])
            )
            .covers_what_is_asked
        );
        // A different digest, a different name, or none to be had, is not the approved program.
        assert!(
            !compare(&approved, &none, "plug", Some(&"f".repeat(64)), &caps(&[])).program_unchanged
        );
        assert!(!compare(&approved, &none, "other", Some(SHA), &caps(&[])).program_unchanged);
        assert!(!compare(&approved, &none, "plug", None, &caps(&[])).program_unchanged);
    }
}
