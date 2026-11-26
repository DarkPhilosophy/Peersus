//! Host-validated plugin declarations. Identity is deliberately not a manifest field.
use crate::{Error, Result, contributions::Contributions, name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Version of the portable manifest protocol (not a Rust ABI).
pub const MANIFEST_VERSION: u32 = 1;
pub const MAX_PARTICIPANTS: usize = 32;
pub const MAX_MEMBERS: usize = 64;
/// Longest description, in characters, of a capability, an export or one of its members. The
/// same bound the settings' help text has: long enough to say what it does, short enough to
/// be read before deciding.
pub const MAX_DESCRIPTION: usize = 280;
/// Most files a manifest may list: the same bound the application puts on a whole package.
pub const MAX_FILES: usize = 512;
/// The two ways to write a manifest, which a file list never names: a package with two manifests is
/// refused outright, so a list that named the other one would make an installed package refuse
/// itself at the next start. The program is reserved too, but under the name it really has, which
/// the manifest may choose: see [`Manifest::program_file`].
const MANIFESTS: [&str; 2] = ["plugin.json", "plugin.toml"];
/// What the program of a package is called when its manifest does not say.
pub const DEFAULT_PROGRAM: &str = "plugin";
/// Longest program file name.
pub const MAX_PROGRAM_NAME: usize = 64;

/// Version of the module host ABI shared by native-library and WASM executors.
pub const EXECUTION_ABI_VERSION: u32 = 1;

/// The executor an extension author must explicitly select for every package.
///
/// The file format never grants capabilities or changes broker contracts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Execution {
    /// Runs as a standalone executable, only when explicitly selected.
    Process,
    /// Loaded by a separate helper, never in the owner or UI process.
    NativeLibrary { abi: u32 },
    /// Interpreted in a separate process with no ambient OS resources.
    Wasm { abi: u32 },
}

/// Named service export with an exact version. Exporting is optional and chosen by the
/// plugin author. `consumers` lists the only participants that may use it, so an
/// export with no consumers is private. The user's grant of a method's capability is
/// checked on top of this list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub service: String,
    pub version: u32,
    /// What this service offers, in words for the person deciding whether to allow it.
    pub description: String,
    #[serde(default)]
    pub consumers: BTreeSet<String>,
    /// Method name to what it needs and does.
    pub methods: BTreeMap<String, Member>,
    /// Event name to what it needs and means.
    pub events: BTreeMap<String, Member>,
}

/// One method or event of an export: the capability the consumer must have been granted, if
/// any, and what it does. The description is required: a member nobody can explain is one
/// nobody can decide to allow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    #[serde(default)]
    pub capability: Option<String>,
    pub description: String,
}

/// Methods and events a participant depends on, from one *named* provider at one exact
/// version. Naming the provider matters for safety: another plugin that happens to
/// export the same service name can never receive these calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    pub provider: String,
    pub service: String,
    pub version: u32,
    pub methods: BTreeSet<String>,
    pub events: BTreeSet<String>,
}

/// What a plugin may ask of the user, by name, and what each name means.
///
/// A name alone is not something a person can decide about: `FlyCar::FlyThatCar` means
/// something to its author and nothing to anyone else. So every capability is declared with
/// a description, and a manifest without one is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    /// Capability name to what it lets a plugin do, in words for the person who grants it.
    pub capabilities: BTreeMap<String, String>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    #[serde(default)]
    pub contributions: Contributions,
    /// The files of the package other than the manifest and the program, which are always part of
    /// it. When the manifest has this, ONLY these are installed, and anything else lying next to
    /// them in the folder or archive is left out, so that choosing a folder never installs what
    /// the plugin did not name. Paths are relative to the package and cannot leave it. When it is
    /// absent, which is every manifest written before this existed, the whole package is taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// The file name of the program the application starts, when it is not called `plugin`. It is
    /// the name the operating system lists the process by, so a plugin can be recognised in a list of
    /// processes. One plain file name in the package itself, never a path; the manifest only says
    /// which file, and the application still checks that it is there, is its owner's and can be run.
    /// When absent, which is every manifest written before this existed, it is `plugin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Explicit OS support. Omitted only for legacy manifests; an explicit empty list is invalid.
    /// A declaration describes compatibility, never grants access to platform resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<crate::platform::OperatingSystem>>,
    /// The author must explicitly choose an executor for every package.
    pub execution: Execution,
    /// The licences and the notice the person reads and accepts before this extension installs. An
    /// extension without them cannot be installed; one installed before they existed keeps running
    /// and is simply never "accepted" (`extension.tos.accepted` answers false for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreements: Option<crate::agreement::Agreements>,
}

impl Manifest {
    /// Start an empty declaration with an explicitly chosen executor.
    pub fn new(execution: Execution) -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            capabilities: BTreeMap::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            contributions: Contributions::default(),
            files: None,
            program: None,
            platforms: None,
            execution,
            agreements: None,
        }
    }
}

fn ok(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(Error::InvalidManifest)
}

/// Text for a person: non-blank, bounded, with no control characters.
fn described(text: &str) -> bool {
    name::text(text, MAX_DESCRIPTION)
}

/// A path a manifest lists: relative to the package, made only of ordinary names, with nothing that
/// could mean something else on the way down (a backslash, a control character, an empty part, a
/// trailing slash), and not one of the two files every package has.
fn listed_path(path: &str) -> bool {
    use std::path::{Component, Path};
    !path.is_empty()
        && path.len() <= 255
        && !path.chars().any(|c| c.is_control() || c == '\\')
        && !path.ends_with('/')
        && !path.contains("//")
        && !MANIFESTS.contains(&path)
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        // `./a` normalises to `a`, which would let one file be named two ways.
        && !path.starts_with("./")
}

/// A program file name: one plain name that cannot mean another file, a path, an option or a hidden
/// file, and not one of the two manifests. Letters, digits, `-`, `_` and `.`: what programs are
/// called by. The name is what the operating system will list the process as, which is why a
/// manifest may choose it.
fn program_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PROGRAM_NAME
        && !name.starts_with(['-', '.'])
        && crate::name::plain(name)
        && !MANIFESTS.contains(&name)
}

impl Manifest {
    /// Whether this manifest declares `capability`, and so describes it to the user.
    pub fn declares(&self, capability: &str) -> bool {
        self.capabilities.contains_key(capability)
    }

    /// The file name of this package's program: what the manifest says, or [`DEFAULT_PROGRAM`]. The
    /// ONE place that answers, so discovery, installing and unpacking cannot disagree. It is also what
    /// the operating system lists the running process as, because that is the name of the file.
    pub fn program_file(&self) -> &str {
        self.program.as_deref().unwrap_or(DEFAULT_PROGRAM)
    }

    /// Runtime compatibility, independent of capability grants and executable validation.
    /// Legacy omission preserves existing installations; new plugins declare support explicitly.
    pub fn supports(&self, platform: crate::platform::OperatingSystem) -> bool {
        self.platforms
            .as_ref()
            .is_none_or(|platforms| platforms.contains(&platform))
    }
}

impl Manifest {
    /// Fail-closed validation, including that every contribution addresses a real
    /// export and requires exactly the capability the exporter enforces.
    pub fn validate(&self) -> Result<()> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err(Error::Version);
        }
        if matches!(self.execution, Execution::NativeLibrary { abi } | Execution::Wasm { abi } if abi != EXECUTION_ABI_VERSION)
        {
            return Err(Error::Version);
        }
        if let Some(platforms) = &self.platforms {
            let unique: BTreeSet<_> = platforms.iter().collect();
            ok(!platforms.is_empty()
                && platforms.len() <= crate::platform::OperatingSystem::ALL.len()
                && unique.len() == platforms.len())?;
        }
        ok(self
            .capabilities
            .iter()
            .all(|(capability, what)| name::valid(capability) && described(what)))?;
        let mut exported = BTreeSet::new();
        for e in &self.exports {
            ok(name::valid(&e.service)
                && described(&e.description)
                && e.version != 0
                && exported.insert(&e.service)
                && e.consumers.len() <= MAX_PARTICIPANTS
                && e.consumers.iter().all(|c| name::valid(c))
                && e.methods.len() <= MAX_MEMBERS
                && e.events.len() <= MAX_MEMBERS
                && e.methods.iter().chain(&e.events).all(|(member, details)| {
                    // A member says what it does, and whatever it asks of the user is
                    // something the user can be shown: a capability this manifest declares,
                    // and therefore describes.
                    name::valid(member)
                        && described(&details.description)
                        && details
                            .capability
                            .as_deref()
                            .is_none_or(|c| name::valid(c) && self.capabilities.contains_key(c))
                }))?;
        }
        let mut imported = BTreeSet::new();
        for i in &self.imports {
            ok(name::valid(&i.provider)
                && name::valid(&i.service)
                && i.version != 0
                && imported.insert((&i.provider, &i.service))
                && i.methods.len() <= MAX_MEMBERS
                && i.events.len() <= MAX_MEMBERS
                && i.methods.iter().chain(&i.events).all(|m| name::valid(m)))?;
        }
        self.contributions.validate()?;
        if let Some(agreements) = &self.agreements {
            agreements.validate()?;
        }
        if let Some(program) = &self.program {
            ok(program_name(program))?;
        }
        if let Some(files) = &self.files {
            let mut seen = BTreeSet::new();
            ok(files.len() <= MAX_FILES
                && files.iter().all(|path| {
                    listed_path(path) && path != self.program_file() && seen.insert(path.as_str())
                }))?;
        }
        for action in &self.contributions.actions {
            let required = self.exported_method(&action.target.service, &action.target.method)?;
            ok(action.capability.as_ref() == required)?;
        }
        if let Some(input) = &self.contributions.input {
            let required = self.exported_method(&input.source.service, &input.source.method)?;
            ok(required == Some(&input.capability))?;
            let _ = input.pressure_setting(&self.contributions.settings)?;
        }
        // A page names the systems it is offered on; one the plugin does not run on is simply never
        // shown. It is not refused: a package built for one system carries the pages of all of them.
        for page in &self.contributions.pages {
            if let Some(source) = &page.source {
                let required = self.exported_method(&source.service, &source.method)?;
                // A page that sends input is held to its method's guard, as an action is.
                if page.surface.is_some() {
                    ok(page.capability.as_ref() == required)?;
                }
            }
        }
        Ok(())
    }

    fn exported_method(&self, service: &str, method: &str) -> Result<Option<&String>> {
        self.exports
            .iter()
            .find(|e| e.service == service)
            .and_then(|e| e.methods.get(method))
            .map(|member| member.capability.as_ref())
            .ok_or(Error::InvalidManifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contributions::{Action, Page, Placement, Source};

    fn names(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }
    /// A member that needs `capability` (or nothing) and says what it does.
    fn member(capability: Option<&str>) -> Member {
        Member {
            capability: capability.map(str::to_owned),
            description: "Does the thing it is named for.".into(),
        }
    }
    fn exporter() -> Manifest {
        Manifest {
            capabilities: [("use.echo".to_owned(), "Lets it echo text back.".to_owned())].into(),
            exports: vec![Export {
                service: "echo".into(),
                version: 1,
                description: "Echoes text.".into(),
                consumers: names(&["host"]),
                methods: [("echo".into(), member(Some("use.echo")))].into(),
                events: BTreeMap::new(),
            }],
            ..Manifest::new(Execution::Process)
        }
    }
    fn action(service: &str, method: &str, capability: Option<&str>) -> Manifest {
        let mut m = exporter();
        m.contributions.actions.push(Action {
            id: "run".into(),
            label: "Run".into(),
            short_label: None,
            input: crate::contributions::ActionInput::None,
            target: Source {
                service: service.into(),
                method: method.into(),
            },
            capability: capability.map(str::to_owned),
            needs_device: false,
            destructive: false,
            timeout_ms: crate::contributions::default_action_timeout(),
            chord: None,
            help: None,
        });
        m
    }

    #[test]
    fn action_must_address_a_real_export_with_the_same_capability() {
        action("echo", "echo", Some("use.echo")).validate().unwrap();
        for bad in [
            action("echo", "missing", Some("use.echo")),
            action("other", "echo", Some("use.echo")),
            // A weaker or stronger requirement than the exporter enforces would let a
            // frontend advertise an action the broker silently refuses or widens.
            action("echo", "echo", None),
            action("echo", "echo", Some("use.other")),
        ] {
            assert_eq!(bad.validate(), Err(Error::InvalidManifest));
        }
    }

    #[test]
    fn page_source_must_address_a_real_export() {
        let page = |service: &str, method: &str| {
            let mut m = exporter();
            m.contributions.pages.push(Page {
                id: "p".into(),
                label: "P".into(),
                placement: Placement::Primary,
                icon: None,
                source: Some(Source {
                    service: service.into(),
                    method: method.into(),
                }),
                surface: None,
                needs_device: false,
                capability: None,
                platforms: Vec::new(),
            });
            m
        };
        page("echo", "echo").validate().unwrap();
        assert!(page("echo", "nope").validate().is_err());
    }

    #[test]
    fn input_transport_is_independent_of_pages_but_requires_the_exact_export_guard() {
        let mut manifest = exporter();
        manifest.contributions.input = Some(crate::contributions::Input {
            source: Source {
                service: "echo".into(),
                method: "echo".into(),
            },
            capability: "use.echo".into(),
            edge_pressure: crate::contributions::EdgePressure::Off {},
        });
        manifest.validate().unwrap();
        let mut missing = manifest.clone();
        missing.contributions.input.as_mut().unwrap().source.method = "missing".into();
        assert!(missing.validate().is_err());
        let mut unguarded = manifest.clone();
        unguarded.exports[0]
            .methods
            .get_mut("echo")
            .unwrap()
            .capability = None;
        assert!(unguarded.validate().is_err());
        for capability in ["", "use.other"] {
            let mut invalid = manifest.clone();
            invalid.contributions.input.as_mut().unwrap().capability = capability.into();
            assert!(invalid.validate().is_err());
        }
    }

    fn input_manifest(policy: serde_json::Value) -> serde_json::Value {
        let mut value = serde_json::to_value(exporter()).unwrap();
        value["contributions"]["input"] = serde_json::json!({
            "source": {"service":"echo", "method":"echo"},
            "capability":"use.echo",
            "edge_pressure":policy,
        });
        value["contributions"]["settings"] = serde_json::json!([{
            "id":"pressure",
            "label":"Pressure",
            "scope":"global",
            "platforms":["linux"],
            "kind": {"type":"number", "min":0, "max":400, "default":80},
        }]);
        value
    }

    #[test]
    fn input_pressure_is_an_explicit_off_or_a_bounded_global_number_in_the_same_manifest() {
        let bound = input_manifest(serde_json::json!({"type":"setting","id":"pressure"}));
        serde_json::from_value::<Manifest>(bound.clone())
            .unwrap()
            .validate()
            .unwrap();
        serde_json::from_value::<Manifest>(input_manifest(serde_json::json!({"type":"off"})))
            .unwrap()
            .validate()
            .unwrap();
        // Non-Linux archives share the same manifest declarations; packaging narrows only
        // the top-level platform, not the setting's Linux-only applicability.
        let mut windows_archive = bound.clone();
        windows_archive["platforms"] = serde_json::json!(["windows"]);
        serde_json::from_value::<Manifest>(windows_archive)
            .unwrap()
            .validate()
            .unwrap();

        for (field, value) in [
            ("id", serde_json::json!("different")),
            ("platforms", serde_json::json!(["windows"])),
            ("platforms", serde_json::json!(["linux", "windows"])),
            ("platforms", serde_json::json!([])),
            ("scope", serde_json::json!("device")),
            ("kind", serde_json::json!({"type":"toggle","default":false})),
            (
                "kind",
                serde_json::json!({"type":"number","min":-1,"max":400,"default":80}),
            ),
            (
                "kind",
                serde_json::json!({"type":"number","min":0,"max":401,"default":80}),
            ),
            (
                "kind",
                serde_json::json!({"type":"number","min":0,"max":25,"default":80}),
            ),
        ] {
            let mut invalid = bound.clone();
            invalid["contributions"]["settings"][0][field] = value;
            assert!(
                serde_json::from_value::<Manifest>(invalid)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
        for policy in [
            serde_json::json!(80),
            serde_json::json!({"type":"setting","id":"pressure","value":80}),
            serde_json::json!({"type":"off","id":"pressure"}),
        ] {
            assert!(serde_json::from_value::<Manifest>(input_manifest(policy)).is_err());
        }
    }

    #[test]
    fn a_carrier_cannot_leave_its_edge_pressure_unspecified() {
        let mut manifest = input_manifest(serde_json::json!({"type":"off"}));
        manifest["contributions"]["input"]
            .as_object_mut()
            .unwrap()
            .remove("edge_pressure");
        assert!(serde_json::from_value::<Manifest>(manifest).is_err());
    }

    #[test]
    fn manifest_without_contributions_decodes_and_unknown_fields_fail() {
        let mut value = serde_json::to_value(exporter()).unwrap();
        value.as_object_mut().unwrap().remove("contributions");
        let m: Manifest = serde_json::from_value(value.clone()).unwrap();
        m.validate().unwrap();
        value["identity"] = "host".into();
        assert!(serde_json::from_value::<Manifest>(value).is_err());
    }

    #[test]
    fn execution_is_required_and_every_kind_survives_serialization() {
        for execution in [
            Execution::Process,
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            },
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            },
        ] {
            let mut json = serde_json::to_value(Manifest::new(execution)).unwrap();
            assert_eq!(
                serde_json::from_value::<Manifest>(json.clone())
                    .unwrap()
                    .execution,
                execution
            );
            json.as_object_mut().unwrap().remove("execution");
            let error = serde_json::from_value::<Manifest>(json).unwrap_err();
            assert!(error.to_string().contains("missing field `execution`"));
        }
    }

    #[test]
    fn duplicate_exports_and_zero_versions_are_rejected() {
        let mut dup = exporter();
        dup.exports.push(dup.exports[0].clone());
        assert!(dup.validate().is_err());
        let mut zero = exporter();
        zero.exports[0].version = 0;
        assert!(zero.validate().is_err());
        let mut version = exporter();
        version.manifest_version = 2;
        assert_eq!(version.validate(), Err(Error::Version));
    }

    // What a person reads when deciding whether to grant a permission. Every one of these is
    // the same failure: a name, such as `FlyCar::FlyThatCar`, that nobody but its author can
    // explain.

    #[test]
    fn every_capability_must_say_what_it_lets_a_plugin_do() {
        for bad in ["", "   ", "\u{7}bell", &"x".repeat(MAX_DESCRIPTION + 1)] {
            let mut m = exporter();
            m.capabilities.insert("use.echo".into(), bad.into());
            assert_eq!(m.validate(), Err(Error::InvalidManifest), "{bad:?}");
        }
        let mut longest = exporter();
        longest
            .capabilities
            .insert("use.echo".into(), "x".repeat(MAX_DESCRIPTION));
        longest.validate().unwrap();
    }

    #[test]
    fn an_export_must_say_what_it_offers() {
        for bad in ["", "  ", "bell\u{7}", &"x".repeat(MAX_DESCRIPTION + 1)] {
            let mut m = exporter();
            m.exports[0].description = bad.into();
            assert_eq!(m.validate(), Err(Error::InvalidManifest), "{bad:?}");
        }
    }

    #[test]
    fn every_method_and_every_event_must_say_what_it_does() {
        for bad in ["", "  ", "bell\u{7}", &"x".repeat(MAX_DESCRIPTION + 1)] {
            let mut method = exporter();
            method.exports[0]
                .methods
                .get_mut("echo")
                .unwrap()
                .description = bad.into();
            assert_eq!(
                method.validate(),
                Err(Error::InvalidManifest),
                "method {bad:?}"
            );
            let mut event = exporter();
            event.exports[0].events.insert(
                "heard".into(),
                Member {
                    capability: None,
                    description: bad.into(),
                },
            );
            assert_eq!(
                event.validate(),
                Err(Error::InvalidManifest),
                "event {bad:?}"
            );
        }
    }

    #[test]
    fn a_member_cannot_require_a_capability_the_manifest_does_not_declare() {
        // Otherwise the user could never be shown, or grant, what the member asks for.
        let mut m = exporter();
        m.exports[0]
            .methods
            .insert("peek".into(), member(Some("undeclared.peek")));
        assert_eq!(m.validate(), Err(Error::InvalidManifest));
        let mut event = exporter();
        event.exports[0]
            .events
            .insert("heard".into(), member(Some("undeclared.heard")));
        assert_eq!(event.validate(), Err(Error::InvalidManifest));
    }

    #[test]
    fn a_declared_capability_nobody_uses_is_still_valid() {
        // An author may declare what a later method will need; the user sees its description.
        let mut m = exporter();
        m.capabilities
            .insert("spare.use".into(), "Reserved for a later method.".into());
        m.validate().unwrap();
    }

    #[test]
    fn a_member_with_no_capability_still_needs_a_description() {
        let mut m = exporter();
        m.exports[0].methods.insert(
            "open".into(),
            Member {
                capability: None,
                description: String::new(),
            },
        );
        assert_eq!(m.validate(), Err(Error::InvalidManifest));
        m.exports[0].methods.insert("open".into(), member(None));
        m.validate().unwrap();
    }

    #[test]
    fn the_old_shapes_are_refused_when_read_so_nothing_is_silently_left_undescribed() {
        // Capabilities as a bare list of names, and members as a bare capability: both are
        // the format that let a permission have no explanation.
        let list: Result<Manifest, _> = serde_json::from_str(
            r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":["use.echo"],"imports":[],"exports":[]}"#,
        );
        assert!(
            list.is_err(),
            "a list of bare capability names was accepted"
        );
        let bare = serde_json::from_str::<Manifest>(
            r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{"use.echo":"Echo."},"imports":[],
            "exports":[{"service":"echo","version":1,"description":"E.","consumers":["host"],
            "methods":{"echo":"use.echo"},"events":{}}]}"#,
        );
        assert!(
            bare.is_err(),
            "a member given as a bare capability name was accepted"
        );
    }

    #[test]
    fn the_new_shape_round_trips_through_json() {
        let text = serde_json::to_string(&exporter()).unwrap();
        let back: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, exporter());
        back.validate().unwrap();
    }

    #[test]
    fn a_manifest_without_a_file_list_reads_as_before_and_names_no_files() {
        let json = r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[]}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            m.files, None,
            "an older manifest must keep meaning 'the whole folder'"
        );
        m.validate().unwrap();
    }

    #[test]
    fn a_file_list_is_read_and_kept_in_the_order_written() {
        let json = r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[],
                       "files":["lib/libwork.so","helper","settings.toml"]}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            m.files.as_deref(),
            Some(
                &[
                    "lib/libwork.so".to_owned(),
                    "helper".to_owned(),
                    "settings.toml".to_owned()
                ][..]
            )
        );
        m.validate().unwrap();
    }

    #[test]
    fn a_file_list_cannot_name_a_way_out_or_anything_odd() {
        let hostile = [
            "../escape",
            "a/../../escape",
            "/etc/passwd",
            "./a/../../b",
            "a\\b",
            "tab\tname",
            "nul\0byte",
            "",
            ".",
            "a//b",
            "dir/",
            "plugin.json", // a manifest, written one way
            "plugin.toml", // a manifest, written the other way: both in one package refuse it
            "plugin",      // the program, likewise
        ];
        for bad in hostile {
            let mut m = exporter();
            m.files = Some(vec![bad.to_owned()]);
            assert_eq!(
                m.validate(),
                Err(Error::InvalidManifest),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn a_manifest_without_a_program_name_means_the_program_called_plugin() {
        let json = r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[]}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            m.program, None,
            "an older manifest must keep meaning the file called plugin"
        );
        assert_eq!(m.program_file(), "plugin");
        m.validate().unwrap();
    }

    #[test]
    fn a_manifest_may_name_its_program_and_the_name_is_what_the_process_will_be_called() {
        let json = r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[],
                       "program":"peersus-clipboard"}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.program_file(), "peersus-clipboard");
        m.validate().unwrap();
    }

    #[test]
    fn a_program_name_is_one_plain_file_name_and_nothing_that_could_mean_another_file() {
        let hostile = [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "/bin/sh",
            "a\\b",
            "tab\tname",
            "nul\0byte",
            "-flag", // read as an option by anything that is handed it later
            ".hidden",
            "plugin.json", // a manifest
            "plugin.toml", // the other manifest
            "has space",
        ];
        for bad in hostile {
            let mut m = exporter();
            m.program = Some(bad.to_owned());
            assert_eq!(
                m.validate(),
                Err(Error::InvalidManifest),
                "{bad:?} was accepted"
            );
        }
        let mut longest = exporter();
        longest.program = Some("a".repeat(65));
        assert_eq!(longest.validate(), Err(Error::InvalidManifest));
        // Plain names, with the characters real programs are called by, are fine.
        for good in [
            "plugin",
            "peersus-clipboard",
            "my_helper",
            "tool.v2",
            "Tool9",
        ] {
            let mut m = exporter();
            m.program = Some(good.to_owned());
            m.validate()
                .unwrap_or_else(|_| panic!("{good:?} was refused"));
        }
    }

    #[test]
    fn the_program_cannot_also_be_listed_as_a_file_and_the_old_name_is_an_ordinary_file_once_it_is_renamed()
     {
        // The program is always part of the package, under whatever name it has, so a list that names
        // it again says nothing and would hide that two files were meant.
        let mut m = exporter();
        m.program = Some("peersus-clipboard".into());
        m.files = Some(vec!["peersus-clipboard".into()]);
        assert_eq!(m.validate(), Err(Error::InvalidManifest));
        // With a program of another name, a file called `plugin` is an ordinary file and may be listed.
        m.files = Some(vec!["plugin".into()]);
        m.validate().unwrap();
    }

    #[test]
    fn a_file_list_with_the_same_file_twice_or_too_many_is_refused() {
        let mut twice = exporter();
        twice.files = Some(vec!["a".into(), "a".into()]);
        assert_eq!(twice.validate(), Err(Error::InvalidManifest));

        let mut many = exporter();
        many.files = Some((0..=MAX_FILES).map(|n| format!("f{n}")).collect());
        assert_eq!(many.validate(), Err(Error::InvalidManifest));
        many.files = Some((0..MAX_FILES).map(|n| format!("f{n}")).collect());
        many.validate().unwrap();

        // An empty list is a statement: "nothing besides the manifest and the program".
        let mut none = exporter();
        none.files = Some(Vec::new());
        none.validate().unwrap();
    }

    #[test]
    fn explicit_platform_declarations_are_checked_without_inventing_support() {
        use crate::platform::OperatingSystem::{Android, Linux, Windows};
        let mut manifest = exporter();
        assert!(
            manifest.supports(Windows),
            "legacy omission must not break installed plugins"
        );
        manifest.platforms = Some(vec![Android, Linux]);
        manifest.validate().unwrap();
        assert!(manifest.supports(Android));
        assert!(!manifest.supports(Windows));
        manifest.platforms = Some(vec![]);
        assert_eq!(manifest.validate(), Err(Error::InvalidManifest));
        manifest.platforms = Some(vec![Linux, Linux]);
        assert_eq!(manifest.validate(), Err(Error::InvalidManifest));
        let invalid = r#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[],"platforms":["made-up"]}"#;
        assert!(serde_json::from_str::<Manifest>(invalid).is_err());
    }

    #[test]
    fn an_unknown_module_abi_or_executor_is_refused_before_loading_code() {
        for execution in [
            serde_json::json!({"kind": "wasm", "abi": 0}),
            serde_json::json!({"kind": "wasm", "abi": 2}),
            serde_json::json!({"kind": "native_library", "abi": 2}),
            serde_json::json!({"kind": "native_library"}),
            serde_json::json!({"kind": "made_up", "abi": 1}),
            serde_json::json!({"kind": "wasm", "abi": 1, "allow_native": true}),
        ] {
            let mut declaration = serde_json::to_value(exporter()).unwrap();
            declaration["execution"] = execution;
            assert!(
                !serde_json::from_value::<Manifest>(declaration)
                    .is_ok_and(|manifest| manifest.validate().is_ok())
            );
        }
    }

    #[test]
    fn a_manifest_may_carry_an_agreement_and_an_invalid_one_refuses_the_whole_manifest() {
        let agreement = serde_json::json!({"pages": [
            {"kind": "licence", "title": "MIT", "explains": "You may use and share it.",
             "changes": "Nothing changes on your devices.", "spdx": "MIT",
             "links": [{"label": "Text", "url": "https://opensource.org/license/mit"}]},
            {"kind": "notice", "title": "What it does", "explains": "It echoes text.",
             "changes": "A page is added.", "body": [{"k": "paragraph", "text": "Echo."}]}
        ]});
        let mut declaration = serde_json::to_value(exporter()).unwrap();
        declaration["agreements"] = agreement.clone();
        let manifest: Manifest = serde_json::from_value(declaration.clone()).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.agreements.as_ref().map(|a| a.pages.len()), Some(2));
        // Without one the manifest is still a valid manifest: the host decides whether it may be installed.
        assert!(exporter().agreements.is_none());
        exporter().validate().unwrap();
        // And what is written without one is written without the field, so older readers see what they always saw.
        assert!(
            serde_json::to_value(exporter())
                .unwrap()
                .get("agreements")
                .is_none()
        );
        let mut bad = declaration;
        bad["agreements"]["pages"][0]["links"] = serde_json::json!([]);
        let broken: Manifest = serde_json::from_value(bad).unwrap();
        assert_eq!(broken.validate(), Err(Error::InvalidManifest));
    }
}
