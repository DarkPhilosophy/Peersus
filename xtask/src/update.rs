//! `cargo xtask update-sign`: describe one release of the application and sign the description.
//!
//! Publisher-only. The signing seed is read ONLY from the `APP_UPDATE_SIGNING_KEY` environment
//! variable (base64 of the 32-byte Ed25519 seed): never from a file, never printed, never written.
//! It is a different key from the extension catalog's, and what is signed begins with a context
//! line, so a signature made here is not a signature of anything else.
//!
//!   cargo xtask update-sign --channel test --version 0.1.0 --build 1 --out DIR \
//!       --artifact linux:x86_64:archive:PATH [--artifact ...] [--notes TEXT]
//!
//! Written into DIR: `update.json`, `update.sig`, and each artifact under the file name the
//! document pins. Those files are what a release under the tag `app/<channel>` holds. Nothing is
//! uploaded, tagged or published by this task.
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::PathBuf};

/// The same bytes `peersus_app::app_update::SIGNATURE_CONTEXT` verifies with.
pub const SIGNATURE_CONTEXT: &[u8] = b"peersus-app-update-v1\n";

pub struct Artifact {
    pub platform: String,
    pub architecture: String,
    pub kind: String,
    pub file: String,
    pub bytes: Vec<u8>,
}

fn word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn version(text: &str) -> bool {
    let parts: Vec<_> = text.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 20
                && part.bytes().all(|b| b.is_ascii_digit())
                && (*part == "0" || !part.starts_with('0'))
        })
}

/// Build the document and its signature. Pure: no I/O, so it can be tested.
pub fn release(
    seed: &[u8; 32],
    channel: &str,
    release_version: &str,
    build: u64,
    notes: &str,
    artifacts: &[Artifact],
) -> Result<(Vec<u8>, [u8; 64]), String> {
    if channel.is_empty()
        || channel.len() > 32
        || !channel
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("the channel is lower-case letters, digits and '-'".into());
    }
    if !version(release_version) {
        return Err("the version is not major.minor.patch".into());
    }
    if artifacts.is_empty() || artifacts.len() > 32 {
        return Err("a release has between 1 and 32 artifacts".into());
    }
    let mut targets = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut listed = Vec::new();
    for artifact in artifacts {
        if !word(&artifact.platform)
            || !word(&artifact.architecture)
            || !matches!(artifact.kind.as_str(), "archive" | "image" | "package")
        {
            return Err(format!(
                "{}: the target is platform:architecture:kind, kind one of archive, image, package",
                artifact.file
            ));
        }
        if artifact.file.is_empty()
            || artifact.file.len() > 128
            || artifact.file.starts_with('.')
            || !artifact
                .file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(format!("{:?} is not a plain file name", artifact.file));
        }
        if artifact.bytes.is_empty() || artifact.bytes.len() > 128 * 1024 * 1024 {
            return Err(format!("{}: empty or larger than 128 MiB", artifact.file));
        }
        if !targets.insert((&artifact.platform, &artifact.architecture, &artifact.kind))
            || !files.insert(&artifact.file)
        {
            return Err(format!(
                "{}: a target or file name is listed twice",
                artifact.file
            ));
        }
        listed.push(json!({
            "platform": artifact.platform,
            "architecture": artifact.architecture,
            "kind": artifact.kind,
            "file": artifact.file,
            "sha256": hex::encode(Sha256::digest(&artifact.bytes)),
            "size": artifact.bytes.len(),
        }));
    }
    let mut document = json!({
        "schema": 1,
        "channel": channel,
        "version": release_version,
        "build": build,
        "artifacts": listed,
    });
    if !notes.is_empty() {
        document["notes"] = Value::String(notes.to_owned());
    }
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    let mut signed = SIGNATURE_CONTEXT.to_vec();
    signed.extend_from_slice(&bytes);
    let signature = SigningKey::from_bytes(seed).sign(&signed).to_bytes();
    Ok((bytes, signature))
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut channel = None;
    let mut release_version = None;
    let mut build = None;
    let mut out = None;
    let mut notes = String::new();
    let mut artifacts = Vec::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let value = iter
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        let once = |slot: &mut Option<String>| {
            slot.replace(value.clone())
                .map_or(Ok(()), |_| Err(format!("{flag} given twice")))
        };
        match flag.as_str() {
            "--channel" => once(&mut channel)?,
            "--version" => once(&mut release_version)?,
            "--build" => once(&mut build)?,
            "--out" => once(&mut out)?,
            "--notes" => notes = value,
            "--artifact" => {
                let parts: Vec<_> = value.splitn(4, ':').collect();
                let [platform, architecture, kind, path] = parts.as_slice() else {
                    return Err("--artifact is platform:architecture:kind:PATH".into());
                };
                let path = PathBuf::from(path);
                let file = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("the artifact has no plain file name")?
                    .to_owned();
                artifacts.push(Artifact {
                    platform: (*platform).to_owned(),
                    architecture: (*architecture).to_owned(),
                    kind: (*kind).to_owned(),
                    file,
                    bytes: fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
                });
            }
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let channel = channel.ok_or("--channel is required")?;
    let release_version = release_version.ok_or("--version is required")?;
    let build: u64 = build
        .ok_or("--build is required")?
        .parse()
        .map_err(|_| "--build is a whole number")?;
    let out = PathBuf::from(out.ok_or("--out is required")?);
    let seed = crate::catalog::seed_from("APP_UPDATE_SIGNING_KEY")?;
    let (document, signature) =
        release(&seed, &channel, &release_version, build, &notes, &artifacts)?;
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    fs::write(out.join("update.json"), &document).map_err(|e| e.to_string())?;
    fs::write(out.join("update.sig"), signature).map_err(|e| e.to_string())?;
    for artifact in &artifacts {
        fs::write(out.join(&artifact.file), &artifact.bytes).map_err(|e| e.to_string())?;
    }
    println!(
        "Signed {release_version} build {build} for channel {channel}: {} file(s) in {}. Release tag: app/{channel}. Nothing was published.",
        artifacts.len() + 2,
        out.display()
    );
    Ok(())
}

/// `cargo xtask update-keygen --out PATH`: make the publisher's update signing key.
pub fn keygen(args: &[String]) -> Result<(), String> {
    crate::catalog::keygen_for(
        args,
        "update-keygen",
        "UPDATE_PUBLIC_KEY in crates/peersus-app/src/app_update.rs",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    fn artifact(architecture: &str, kind: &str, file: &str) -> Artifact {
        Artifact {
            platform: "linux".into(),
            architecture: architecture.into(),
            kind: kind.into(),
            file: file.into(),
            bytes: file.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_signature_covers_the_context_and_the_document_and_pins_every_artifact() {
        let seed = [9; 32];
        let (document, signature) = release(
            &seed,
            "test",
            "0.1.0",
            4,
            "notes",
            &[
                artifact("x86_64", "archive", "a.tar.gz"),
                artifact("any", "image", "b.image"),
            ],
        )
        .unwrap();
        let key = SigningKey::from_bytes(&seed).verifying_key();
        let signature = Signature::from_bytes(&signature);
        let mut signed = SIGNATURE_CONTEXT.to_vec();
        signed.extend_from_slice(&document);
        assert!(key.verify(&signed, &signature).is_ok());
        // Not a signature of the bare document: another purpose cannot borrow it.
        assert!(key.verify(&document, &signature).is_err());
        let value: Value = serde_json::from_slice(&document).unwrap();
        assert_eq!(value["build"], 4);
        assert_eq!(
            value["artifacts"][0]["sha256"],
            hex::encode(Sha256::digest(b"a.tar.gz"))
        );
        assert_eq!(value["artifacts"][1]["size"], 7);
    }

    #[test]
    fn a_release_that_the_application_would_refuse_is_not_signed() {
        let seed = [9; 32];
        let good = || vec![artifact("x86_64", "archive", "a.tar.gz")];
        assert!(release(&seed, "Test", "0.1.0", 0, "", &good()).is_err());
        assert!(release(&seed, "test", "0.1", 0, "", &good()).is_err());
        assert!(release(&seed, "test", "0.01.0", 0, "", &good()).is_err());
        assert!(release(&seed, "test", "0.1.0", 0, "", &[]).is_err());
        assert!(
            release(
                &seed,
                "test",
                "0.1.0",
                0,
                "",
                &[artifact("x86_64", "script", "a")]
            )
            .is_err()
        );
        assert!(
            release(
                &seed,
                "test",
                "0.1.0",
                0,
                "",
                &[artifact("x86_64", "archive", "../a")]
            )
            .is_err()
        );
        let twice = vec![
            artifact("x86_64", "archive", "a"),
            artifact("x86_64", "archive", "b"),
        ];
        assert!(release(&seed, "test", "0.1.0", 0, "", &twice).is_err());
    }
}
