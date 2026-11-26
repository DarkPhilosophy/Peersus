//! `cargo xtask update-sign`: name one release of the application's files and sign their list.
//!
//! Publisher-only. The signing seed is read ONLY from the `APP_UPDATE_SIGNING_KEY` environment
//! variable (base64 of the 32-byte Ed25519 seed): never from a file, never printed, never written.
//! It is a different key from the extension catalog's, and what is signed begins with a context
//! line, so a signature made here is not a signature of anything else.
//!
//!   cargo xtask update-sign --channel dev --version 0.1.0 --build 7 --out DIR \
//!       --artifact linux:x86_64:PATH [--artifact android:aarch64:PATH ...]
//!
//! How each artifact is applied follows from its extension: `.tar.gz` (a folder of programs),
//! `.AppImage` (one file that is the application) or `.apk` (a package).
//!
//! Written into DIR: each artifact under its release name,
//! `peersus-<version>-<channel>.<build>-<platform>-<architecture>.<extension>`, then `SHA256SUMS`
//! and `SHA256SUMS.sig`. `SHA256SUMS` is in the form `sha256sum -c` reads: one `<hex>  <name>` line
//! per artifact, and for each archive one `<hex>  <archive name without .tar.gz>/<program>` line per
//! program in it. Those names are the only place the release's version, channel and build are
//! written; the application reads them back from the signed list (`peersus_app::app_update`). The
//! files are what a release under the tag `v<version>-<channel>.<build>` holds. Nothing is
//! uploaded, tagged or published by this task.
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Read, path::PathBuf};

/// The same bytes `peersus_app::app_update::SIGNATURE_CONTEXT` verifies with.
pub const SIGNATURE_CONTEXT: &[u8] = b"peersus-sums-v1\n";
/// The same limits the application reads a release with.
const MAX_ARTIFACT_BYTES: usize = 128 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 32;
const MAX_PROGRAMS: usize = 64;
const MAX_UNPACKED: u64 = 512 * 1024 * 1024;

const EXTENSIONS: [&str; 3] = [".tar.gz", ".AppImage", ".apk"];

pub struct Artifact {
    pub platform: String,
    pub architecture: String,
    /// The artifact's extension, one of [`EXTENSIONS`].
    pub extension: &'static str,
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

fn channel_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The programs of a release archive: plain files at its top, as the application's installer takes
/// them, each with its SHA-256. Anything else in the archive is refused.
fn programs(archive: &[u8]) -> Result<Vec<(String, String)>, String> {
    let decoded = flate2::read::GzDecoder::new(archive).take(MAX_UNPACKED);
    let mut entries = tar::Archive::new(decoded);
    let mut found = Vec::new();
    let mut names = BTreeSet::new();
    for entry in entries.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let name = path.to_str().unwrap_or_default().to_owned();
        if entry.header().entry_type() != tar::EntryType::Regular || !plain(&name) {
            return Err(format!(
                "{}: the archive holds only plain files at its top",
                path.display()
            ));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if !names.insert(name.clone()) || names.len() > MAX_PROGRAMS {
            return Err(format!(
                "{name}: listed twice, or more than {MAX_PROGRAMS} programs"
            ));
        }
        found.push((name, hex::encode(Sha256::digest(&bytes))));
    }
    if found.is_empty() {
        return Err("the archive holds no program".into());
    }
    Ok(found)
}

/// One signed release: the artifacts' release names (in the order given), the list and its signature.
pub struct Signed {
    pub names: Vec<String>,
    pub list: Vec<u8>,
    pub signature: [u8; 64],
}

/// Name every artifact, list them with their programs and sign the list. Pure: no I/O, so it can
/// be tested.
pub fn release(
    seed: &[u8; 32],
    channel: &str,
    release_version: &str,
    build: u64,
    artifacts: &[Artifact],
) -> Result<Signed, String> {
    if !channel_name(channel) {
        return Err("the channel is lower-case letters and digits".into());
    }
    if !version(release_version) {
        return Err("the version is not major.minor.patch".into());
    }
    if build == 0 {
        return Err("the build number starts at 1".into());
    }
    if artifacts.is_empty() || artifacts.len() > MAX_ARTIFACTS {
        return Err(format!(
            "a release has between 1 and {MAX_ARTIFACTS} artifacts"
        ));
    }
    let mut targets = BTreeSet::new();
    let mut names = Vec::new();
    let mut list = String::new();
    for artifact in artifacts {
        if !word(&artifact.platform)
            || !word(&artifact.architecture)
            || !EXTENSIONS.contains(&artifact.extension)
        {
            return Err(format!(
                "{}:{}: the target is platform:architecture, lower-case letters, digits and '_'",
                artifact.platform, artifact.architecture
            ));
        }
        if artifact.bytes.is_empty() || artifact.bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(format!(
                "{}:{}: empty or larger than 128 MiB",
                artifact.platform, artifact.architecture
            ));
        }
        if !targets.insert((
            &artifact.platform,
            &artifact.architecture,
            artifact.extension,
        )) {
            return Err(format!(
                "{}:{}{}: listed twice",
                artifact.platform, artifact.architecture, artifact.extension
            ));
        }
        let stem = format!(
            "peersus-{release_version}-{channel}.{build}-{}-{}",
            artifact.platform, artifact.architecture
        );
        let name = format!("{stem}{}", artifact.extension);
        list.push_str(&format!(
            "{}  {name}\n",
            hex::encode(Sha256::digest(&artifact.bytes))
        ));
        if artifact.extension == ".tar.gz" {
            for (program, digest) in programs(&artifact.bytes)? {
                list.push_str(&format!("{digest}  {stem}/{program}\n"));
            }
        }
        names.push(name);
    }
    let mut signed = SIGNATURE_CONTEXT.to_vec();
    signed.extend_from_slice(list.as_bytes());
    let signature = SigningKey::from_bytes(seed).sign(&signed).to_bytes();
    Ok(Signed {
        names,
        list: list.into_bytes(),
        signature,
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut channel = None;
    let mut release_version = None;
    let mut build = None;
    let mut out = None;
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
            "--artifact" => {
                let parts: Vec<_> = value.splitn(3, ':').collect();
                let [platform, architecture, path] = parts.as_slice() else {
                    return Err("--artifact is platform:architecture:PATH".into());
                };
                let extension = EXTENSIONS
                    .into_iter()
                    .find(|extension| path.ends_with(extension))
                    .ok_or("the artifact ends in .tar.gz, .AppImage or .apk")?;
                let path = PathBuf::from(path);
                artifacts.push(Artifact {
                    platform: (*platform).to_owned(),
                    architecture: (*architecture).to_owned(),
                    extension,
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
    let Signed {
        names,
        list,
        signature,
    } = release(&seed, &channel, &release_version, build, &artifacts)?;
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    for (name, artifact) in names.iter().zip(&artifacts) {
        fs::write(out.join(name), &artifact.bytes).map_err(|e| e.to_string())?;
    }
    fs::write(out.join("SHA256SUMS"), list).map_err(|e| e.to_string())?;
    fs::write(out.join("SHA256SUMS.sig"), signature).map_err(|e| e.to_string())?;
    println!(
        "Signed {release_version} build {build} for channel {channel}: {} file(s) in {}. Release tag: v{release_version}-{channel}.{build}. Nothing was published.",
        names.len() + 2,
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
    use std::io::Write;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *bytes).unwrap();
        }
        let mut encoder = builder.into_inner().unwrap();
        encoder.flush().unwrap();
        encoder.finish().unwrap()
    }

    fn artifact(architecture: &str, extension: &'static str, bytes: Vec<u8>) -> Artifact {
        Artifact {
            platform: "linux".into(),
            architecture: architecture.into(),
            extension,
            bytes,
        }
    }

    #[test]
    fn the_list_names_every_artifact_and_program_and_is_signed_for_its_purpose() {
        let seed = [9; 32];
        let programs = archive(&[("peersus", b"owner"), ("peersus-ui", b"window")]);
        let artifacts = [
            artifact("x86_64", ".tar.gz", programs.clone()),
            artifact("x86_64", ".AppImage", b"image".to_vec()),
        ];
        let Signed {
            names,
            list,
            signature,
        } = release(&seed, "dev", "0.1.0", 7, &artifacts).unwrap();
        let hex = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
        assert_eq!(
            names,
            [
                "peersus-0.1.0-dev.7-linux-x86_64.tar.gz",
                "peersus-0.1.0-dev.7-linux-x86_64.AppImage"
            ]
        );
        assert_eq!(
            String::from_utf8(list.clone()).unwrap(),
            format!(
                "{}  peersus-0.1.0-dev.7-linux-x86_64.tar.gz\n\
                 {}  peersus-0.1.0-dev.7-linux-x86_64/peersus\n\
                 {}  peersus-0.1.0-dev.7-linux-x86_64/peersus-ui\n\
                 {}  peersus-0.1.0-dev.7-linux-x86_64.AppImage\n",
                hex(&programs),
                hex(b"owner"),
                hex(b"window"),
                hex(b"image"),
            )
        );
        let key = SigningKey::from_bytes(&seed).verifying_key();
        let signature = Signature::from_bytes(&signature);
        let mut signed = SIGNATURE_CONTEXT.to_vec();
        signed.extend_from_slice(&list);
        assert!(key.verify(&signed, &signature).is_ok());
        // Not a signature of the bare list: another purpose cannot borrow it.
        assert!(key.verify(&list, &signature).is_err());
    }

    #[test]
    fn a_release_that_the_application_would_refuse_is_not_signed() {
        let seed = [9; 32];
        let good = || vec![artifact("x86_64", ".tar.gz", archive(&[("peersus", b"a")]))];
        assert!(release(&seed, "dev", "0.1.0", 1, &good()).is_ok());
        for channel in ["Dev", "de-v", "", "dev.1"] {
            assert!(release(&seed, channel, "0.1.0", 1, &good()).is_err());
        }
        assert!(release(&seed, "dev", "0.1", 1, &good()).is_err());
        assert!(release(&seed, "dev", "0.01.0", 1, &good()).is_err());
        assert!(release(&seed, "dev", "0.1.0", 0, &good()).is_err());
        assert!(release(&seed, "dev", "0.1.0", 1, &[]).is_err());
        let bad = |architecture, extension, bytes| {
            release(
                &seed,
                "dev",
                "0.1.0",
                1,
                &[artifact(architecture, extension, bytes)],
            )
            .is_err()
        };
        assert!(bad("X86", ".tar.gz", archive(&[("peersus", b"a")])));
        assert!(bad("x86-64", ".tar.gz", archive(&[("peersus", b"a")])));
        assert!(bad("x86_64", ".zip", b"a".to_vec()));
        assert!(bad("x86_64", ".tar.gz", b"not an archive".to_vec()));
        assert!(bad("x86_64", ".tar.gz", archive(&[])));
        assert!(bad("x86_64", ".tar.gz", archive(&[("bin/peersus", b"a")])));
        assert!(bad("x86_64", ".tar.gz", archive(&[(".hidden", b"a")])));
        assert!(bad("x86_64", ".AppImage", Vec::new()));
        let twice = [
            artifact("x86_64", ".AppImage", b"a".to_vec()),
            artifact("x86_64", ".AppImage", b"b".to_vec()),
        ];
        assert!(release(&seed, "dev", "0.1.0", 1, &twice).is_err());
    }
}
