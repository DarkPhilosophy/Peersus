//! Portable facts about the host, not OS handles or permission grants.
//!
//! Plugins select adapters using this vocabulary. Platform support is not a promise that
//! a capability is currently authorized: focus, user consent, sandbox and OS policy still apply.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperatingSystem {
    Android,
    Linux,
    Macos,
    Windows,
    Ios,
    Freebsd,
}

impl OperatingSystem {
    pub const ALL: [Self; 6] = [
        Self::Android,
        Self::Linux,
        Self::Macos,
        Self::Windows,
        Self::Ios,
        Self::Freebsd,
    ];

    /// Unknown targets are not represented as another OS and therefore cannot match a declaration.
    pub const fn current() -> Option<Self> {
        if cfg!(target_os = "android") {
            Some(Self::Android)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else if cfg!(target_os = "ios") {
            Some(Self::Ios)
        } else if cfg!(target_os = "freebsd") {
            Some(Self::Freebsd)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    X86,
    Arm,
    Other,
}

impl Architecture {
    pub const fn current() -> Self {
        if cfg!(target_arch = "x86_64") {
            Self::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Self::Aarch64
        } else if cfg!(target_arch = "x86") {
            Self::X86
        } else if cfg!(target_arch = "arm") {
            Self::Arm
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPlatform {
    pub os: Option<OperatingSystem>,
    pub architecture: Architecture,
}

impl HostPlatform {
    /// OS and architecture only. Available facilities are versioned host services in the
    /// broker; they are never inferred from an OS name or hard-coded for a particular plugin.
    pub fn current() -> Self {
        Self {
            os: OperatingSystem::current(),
            architecture: Architecture::current(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_platforms_are_not_silently_accepted() {
        assert!(serde_json::from_str::<OperatingSystem>("\"unknown\"").is_err());
        for os in OperatingSystem::ALL {
            let serialized = serde_json::to_string(&os).unwrap();
            assert_eq!(
                serde_json::from_str::<OperatingSystem>(&serialized).unwrap(),
                os
            );
        }
    }
}
