//! What the About page says, as data, so that it can be checked: every project the application
//! stands on, with what it is for, its licence and where to find it, and the notices that the
//! licences ask for. The page only draws this; it holds no text of its own.
//!
//! The licences here are the ones the dependencies declare, read with `cargo metadata` when this
//! was written, and the attribution a licence asks for is kept in `NOTICES`. A line that is
//! wrong here is a wrong claim made to a person, so each is tested for the facts it must carry.

/// One project the application stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Credit {
    /// Its name, as its authors write it.
    pub name: &'static str,
    /// What it does for this application, in a line.
    pub purpose: &'static str,
    /// Its licence, as an SPDX expression, and for a licence with a choice the one that is used.
    pub licence: &'static str,
    /// Where it lives, always an `https` address.
    pub url: &'static str,
}

/// A titled group of credits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub credits: &'static [Credit],
}

/// The licence of the application itself, as the package declares it in `Cargo.toml`. Taken from
/// there, not written here: this file never chooses a licence, and the page can never say one
/// thing while the package says another.
pub const APPLICATION_LICENCE: &str = env!("CARGO_PKG_LICENSE");

/// The project's own notice, the text of the `NOTICE` file at the root of the repository. The
/// licence (Apache-2.0, section 4(d)) requires anyone who redistributes the application to keep
/// it, so it is shown in the application word for word and is read from the file, never retyped.
pub const PROJECT_NOTICE: &str = include_str!("../../../NOTICE");

/// A risk notice, not a promise of absolute security or a replacement for the licence.
pub const DISCLAIMER: &[&str] = &[
    "This software is provided AS IS, without warranties or conditions of any kind, as described in sections 7 and 8 of the Apache License 2.0. To the extent permitted by applicable law, the authors and contributors are not liable for losses arising from its use, including lost, corrupted or disclosed data.",
    "Keep independent backups and verify important files after a transfer. This application is a transfer tool, not a backup service. You remain responsible for the files you choose, the devices you trust and the permissions you grant.",
    "Security measures reduce risk; they cannot guarantee that vulnerabilities, unauthorized access or data disclosure will never occur. Compromised devices, unsafe plugins, configuration mistakes and future vulnerabilities can affect your data.",
    "This application relies on third-party libraries, operating systems, plugins and, when enabled, network relay services. These dependencies introduce risks outside the authors' control. Third-party components retain their own licences, terms and notices.",
    "Nothing in this notice excludes liability that applicable law does not allow to be excluded, or limits rights that cannot legally be waived.",
];

const fn credit(name: &'static str, purpose: &'static str, licence: &'static str, url: &'static str) -> Credit {
    Credit { name, purpose, licence, url }
}

/// Every project the application stands on that a person would want to know of, grouped by what
/// it does for them. Versions are not written here: they are in `Cargo.lock`, and a number kept in
/// two places is wrong in one of them.
pub const SECTIONS: &[Section] = &[
    Section {
        title: "INTERFACE",
        credits: &[
            credit("Freya", "The windows, layout and components of this application.", "MIT", "https://github.com/marc2332/freya"),
            credit("rfd", "The file and folder choosers of the operating system.", "MIT", "https://github.com/PolyMeilex/rfd"),
            credit("qrcodegen", "Makes the code that another device scans to join.", "MIT", "https://github.com/nayuki/QR-Code-generator"),
        ],
    },
    Section {
        title: "RENDERING",
        credits: &[
            credit("Skia", "Draws everything on the screen, text and shapes alike.", "BSD-3-Clause", "https://skia.org"),
            credit("FreeType", "Shapes the letters of the text.", "FTL", "https://freetype.org"),
            credit("tiny-skia", "A small drawing library used where Skia is not.", "BSD-3-Clause", "https://github.com/RazrFalcon/tiny-skia"),
        ],
    },
    Section {
        title: "NETWORK",
        credits: &[
            credit("iroh", "Encrypted connections between your devices.", "MIT OR Apache-2.0", "https://github.com/n0-computer/iroh"),
            credit("rustls", "The encryption of those connections.", "Apache-2.0 OR ISC OR MIT", "https://github.com/rustls/rustls"),
            credit("ring", "The cryptographic building blocks under rustls.", "Apache-2.0 AND ISC", "https://github.com/briansmith/ring"),
            credit("attohttpc", "Fetches what the application needs over HTTPS.", "MPL-2.0", "https://github.com/sbstp/attohttpc"),
            credit("reqwest", "Downloads the extension catalog and approved extensions, and uploads files, over HTTPS.", "MIT OR Apache-2.0", "https://github.com/seanmonstar/reqwest"),
        ],
    },
    Section {
        title: "ENGINE",
        credits: &[
            credit("Tokio", "Runs many transfers at once without waiting on each other.", "MIT", "https://github.com/tokio-rs/tokio"),
            credit("Serde", "Reads and writes the application's saved files and messages.", "MIT OR Apache-2.0", "https://github.com/serde-rs/serde"),
            credit("keyring", "Keeps this device's identity in the system keyring.", "MIT OR Apache-2.0", "https://github.com/hwchen/keyring-rs"),
            credit("RustCrypto", "Hashes and message codes that check files and pairing.", "MIT OR Apache-2.0", "https://github.com/RustCrypto"),
            credit("ed25519-dalek", "Checks the maintainer's signature on the extension catalog. It can only verify: this application holds no signing key.", "BSD-3-Clause", "https://github.com/dalek-cryptography/curve25519-dalek"),
            credit("object", "Reads who made an extension out of its program file, without running it.", "Apache-2.0 OR MIT", "https://github.com/gimli-rs/object"),
            credit("wasmparser", "Reads the same facts out of a WebAssembly extension, without running it.", "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT", "https://github.com/bytecodealliance/wasm-tools"),
        ],
    },
];

/// What the licences above ask to be said, in the application's own words. Each is a statement
/// about the application as it is built, so each is kept short and checkable.
pub const NOTICES: &[&str] = &[
    "attohttpc is used under MPL-2.0, unchanged. That licence asks that changes to its own files be shared; none are made.",
    "FreeType is used under the FreeType Licence (FTL). Portions of this software are copyright The FreeType Project (freetype.org). All rights reserved.",
    "Skia is used under the BSD 3-Clause licence. The full licence texts of the components built from source are being added to this page.",
    "The sidebar and menu pictures are derived from IconPark (github.com/bytedance/IconPark), Apache-2.0, Copyright 2019-present Bytedance Inc. They were changed: split into layers, with colours replaced so the theme colours them.",
    "rqrr, used only to check in tests that the pairing picture can be read back, is licensed (MIT OR Apache-2.0) AND ISC.",
    "Everything else listed is under MIT, Apache-2.0, BSD, ISC, Zlib or Unicode licences, which ask that their notices travel with the software.",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credits_come_in_these_sections_in_this_order() {
        let titles: Vec<&str> = SECTIONS.iter().map(|section| section.title).collect();
        assert_eq!(titles, ["INTERFACE", "RENDERING", "NETWORK", "ENGINE"]);
        assert!(SECTIONS.iter().all(|section| !section.credits.is_empty()), "an empty section");
    }

    #[test]
    fn every_credit_says_what_it_is_for_names_its_licence_and_links_to_it_safely() {
        for section in SECTIONS {
            for credit in section.credits {
                let name = credit.name;
                assert!(!name.trim().is_empty(), "a credit without a name");
                assert!(credit.purpose.len() >= 12, "{name}: says nothing of what it is for");
                assert!(!credit.licence.trim().is_empty(), "{name}: no licence");
                assert!(credit.url.starts_with("https://"), "{name}: not an https address");
                assert!(!credit.url.contains(' '), "{name}: a space in its address");
            }
        }
    }

    #[test]
    fn no_project_is_credited_twice() {
        let mut names: Vec<&str> = SECTIONS.iter().flat_map(|s| s.credits.iter().map(|c| c.name)).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a project is listed twice");
    }

    #[test]
    fn the_one_copyleft_dependency_is_named_with_what_that_licence_asks() {
        // The only dependency whose licence asks anything beyond a notice is attohttpc (MPL-2.0):
        // changes to its own files must be shared. It is used unchanged, and the page says so.
        let found = SECTIONS
            .iter()
            .flat_map(|s| s.credits.iter())
            .find(|credit| credit.licence.contains("MPL"))
            .expect("the MPL dependency is credited");
        assert_eq!(found.name, "attohttpc");
        assert!(NOTICES.iter().any(|n| n.contains("MPL-2.0") && n.contains("unchanged")));
    }

    #[test]
    fn freetype_is_credited_under_the_licence_that_is_used_and_not_the_other_one() {
        // FreeType is offered under its own licence or under the GPL; the application uses the
        // first, and a page that named the second would be claiming obligations it does not have.
        let freetype = SECTIONS
            .iter()
            .flat_map(|s| s.credits.iter())
            .find(|credit| credit.name == "FreeType")
            .expect("FreeType is credited");
        assert_eq!(freetype.licence, "FTL");
        assert!(NOTICES.iter().any(|n| n.contains("FreeType") && n.to_lowercase().contains("portions of this software")));
    }

    #[test]
    fn the_icon_set_is_credited_with_what_apache_2_asks_to_be_said() {
        // Apache-2.0 asks for the source, the copyright and a statement that files were changed. `glyph.rs` says the licence is shown on
        // this page, so a page without it would be a wrong claim.
        let notice = NOTICES.iter().find(|n| n.contains("IconPark")).expect("IconPark is credited");
        assert!(notice.contains("Apache-2.0"), "the licence is not named");
        assert!(notice.contains("Bytedance"), "the copyright holder is not named");
        assert!(notice.to_lowercase().contains("changed"), "the changes are not stated");
    }

    #[test]
    fn the_licence_the_page_shows_is_the_one_the_package_declares_and_none_is_invented() {
        // Not a line of text kept here: the page repeats what `Cargo.toml` declares, so the two
        // can never disagree, and changing the licence there changes the page.
        assert_eq!(APPLICATION_LICENCE, env!("CARGO_PKG_LICENSE"));
        assert!(!APPLICATION_LICENCE.is_empty(), "the package declares no licence");
        // The notices speak only of the licences of what the application uses.
        assert!(NOTICES.iter().all(|n| !n.contains("GPL-3.0")));
    }

    /// The names of the packages in `Cargo.lock`, read as text so that no parser is added for it.
    fn locked_packages() -> Vec<String> {
        include_str!("../../../Cargo.lock")
            .lines()
            .filter_map(|line| line.strip_prefix("name = \"")?.strip_suffix('"').map(str::to_owned))
            .collect()
    }

    #[test]
    fn every_crate_that_is_credited_is_really_in_the_build_so_the_list_cannot_drift() {
        // Credits that are not Rust crates (the C++ drawing library and its font engine), and
        // groups named for a family of crates rather than one, are checked by their own prefix.
        let packages = locked_packages();
        fn lookup(credit: &str) -> &str {
            match credit {
                "Freya" => "freya",
                "Tokio" => "tokio",
                "Serde" => "serde",
                "RustCrypto" => "sha2",
                other => other,
            }
        }
        for section in SECTIONS {
            for credit in section.credits {
                if matches!(credit.name, "Skia" | "FreeType") {
                    continue;
                }
                let wanted = lookup(credit.name);
                assert!(
                    packages.iter().any(|p| p == wanted),
                    "{} is credited but no package called {wanted} is in Cargo.lock",
                    credit.name
                );
            }
        }
    }

    #[test]
    fn the_notice_the_licence_requires_redistributors_to_keep_is_the_file_itself() {
        // Apache-2.0 4(d): the NOTICE file travels with the software, and the page shows it as a
        // display of the application. It is the file's own text, so that the two cannot differ.
        assert_eq!(PROJECT_NOTICE, include_str!("../../../NOTICE"));
        assert!(PROJECT_NOTICE.starts_with("Peersus\nCopyright "));
        assert!(PROJECT_NOTICE.contains("must include a readable copy of this notice"));
    }

    /// The other direction of the drift test above. Every direct third-party dependency in the workspace is either
    /// credited by name, covered by a credited family, or left out here WITH THE REASON. A new dependency fails this
    /// test until one of the three is chosen on purpose, so the page cannot silently stop being true.
    #[test]
    fn every_direct_dependency_is_credited_covered_by_a_family_or_left_out_for_a_stated_reason() {
        const WORKSPACE: &str = include_str!("../../../Cargo.toml");
        // (dependency, the credited project that covers it)
        const COVERED: &[(&str, &str)] = &[
            ("iroh-base", "iroh"),
            ("serde_json", "Serde"),
            ("sha2", "RustCrypto"),
            ("hmac", "RustCrypto"),
            ("subtle", "RustCrypto"),
            ("zeroize", "RustCrypto"),
            ("freya-android", "Freya"),
            ("freya-skia-safe", "Skia"),
            ("winit", "Freya"),
        ];
        // (dependency, why a person has no need to be told of it)
        const LEFT_OUT: &[(&str, &str)] = &[
            ("thiserror", "derive macros only; generates this application's own error text"),
            ("tracing", "log macros only; no behaviour a person relies on"),
            ("tempfile", "private scratch files; nothing a person sees"),
            ("rqrr", "tests only; named in the notices below"),
            ("rpassword", "reads one line at a terminal for a developer tool"),
            ("fs2", "a lock on one file; invisible to a person"),
            ("rustix", "operating-system calls under the application's own code"),
            ("jni", "Android entry plumbing; invisible to a person"),
            ("hex", "text form of a digest; invisible to a person"),
            ("data-encoding", "text form of keys and codes; invisible to a person"),
            ("rand", "random numbers for the application's own codes"),
            ("clap", "reads the command line"),
            ("toml", "reads one of the two manifest spellings"),
            ("url", "parses addresses; invisible to a person"),
            ("image", "reads the profile picture a person chose"),
            ("arboard", "the clipboard plugin's own access to the system clipboard"),
            ("wl-clipboard-rs", "the clipboard plugin hears that something was copied, on desktops that tell it"),
            ("x11rb", "the clipboard plugin hears that something was copied, on X11 and under Xwayland"),
            ("flate2", "unpacks the archive of an extension a person chose"),
            ("tar", "unpacks the archive of an extension a person chose"),
        ];
        // The keys of [workspace.dependencies], read as text so that no parser is added for it: a key is a line that
        // starts a name and an `=`, inside that table, up to the next table header.
        let dependencies: Vec<&str> = WORKSPACE
            .lines()
            .skip_while(|line| line.trim() != "[workspace.dependencies]")
            .skip(1)
            .take_while(|line| !line.trim_start().starts_with('['))
            .filter_map(|line| {
                let (key, _) = line.split_once(" = ")?;
                (!key.starts_with('#') && !key.starts_with(' ') && !key.contains(' ')).then_some(key)
            })
            .collect();
        assert!(dependencies.len() > 30, "the dependency table was not read: {dependencies:?}");
        let credited: Vec<&str> = SECTIONS.iter().flat_map(|s| s.credits.iter().map(|c| c.name)).collect();
        let unexplained: Vec<&str> = dependencies
            .iter()
            .copied()
            // This project's own crates are not third-party.
            .filter(|name| !name.starts_with("peersus-"))
            .filter(|name| {
                let direct = credited.iter().any(|c| c.eq_ignore_ascii_case(name));
                let covered = COVERED.iter().any(|(n, family)| n == name && credited.contains(family));
                let left_out = LEFT_OUT.iter().any(|(n, why)| n == name && !why.trim().is_empty());
                !(direct || covered || left_out)
            })
            .collect();
        assert!(unexplained.is_empty(), "credit these on the About page, cover them by a family, or leave them out with a reason: {unexplained:?}");
        // And the lists above cannot rot: what they name must still be a dependency.
        for (name, _) in COVERED.iter().chain(LEFT_OUT) {
            assert!(dependencies.contains(name), "{name} is listed here but is not a workspace dependency any more");
        }
    }
}
