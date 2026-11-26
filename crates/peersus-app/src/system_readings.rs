//! How this device is doing, for an extension that shows it, one part at a time: processor, graphics, memory, storage,
//! network and battery. Each part is its own method behind its own permission, granted by the person for that part alone
//! (`device.info.cpu`, `device.info.ram`, …): an extension allowed to see the battery learns nothing of the processor.
//! The hub refuses a call whose permission was not granted before it reaches this file.
//!
//! Read what the operating system publishes; other than the short in-memory battery history,
//! counters are returned as they stand. Nothing is sampled unless a caller asks for it.
use serde::Serialize;
use std::path::Path;

mod battery;
pub(crate) use battery::BatteryHistory;
pub use battery::{BatteryReading, battery_said};

/// The service, offered to an extension that imports it.
pub const SERVICE: &str = "peersus.system";
/// What the service is, in the application's manifest.
pub const DESCRIPTION: &str = "How this device is doing, one part at a time, each behind its own permission: processor, graphics, memory, storage, network and battery, as the operating system publishes them.";

/// One part of the device an extension may be allowed to see.
pub struct Part {
    /// The method that answers it.
    pub method: &'static str,
    /// The permission that guards it, granted for that part alone.
    pub capability: &'static str,
    /// What that permission lets an extension see, in the words a person reads when granting it.
    pub permission: &'static str,
    /// What the method answers, in the application's manifest.
    pub answers: &'static str,
}

pub const PROCESSOR: Part = Part {
    method: "processor",
    capability: "device.info.cpu",
    permission: "See this device's processor: its name, how many cores it has, how fast it runs, how busy it is and how warm.",
    answers: "Answers `{name, cores, mhz, busy, total, celsius}`: `mhz` is the cores' clock now, on average; `busy` and `total` are ticks since the system started.",
};
pub const GRAPHICS: Part = Part {
    method: "graphics",
    capability: "device.info.gpu",
    permission: "See this device's graphics: who made it, how fast it runs, how much of its memory is used, how busy it is and how warm.",
    answers: "Answers `{name, mhz, memory_used, memory_total, busy, celsius}`: memory in bytes, `busy` in hundredths of a percent.",
};
pub const MEMORY: Part = Part {
    method: "memory",
    capability: "device.info.ram",
    permission: "See how much memory this device has and how much of it is free.",
    answers: "Answers `{total, available}` in bytes.",
};
pub const STORAGE: Part = Part {
    method: "storage",
    capability: "device.info.storage",
    permission: "See how large the storage of the folder received files go to is, how much of it is free, and the name the system gives its disk. It sees no files or folder names.",
    answers: "Answers `{device, total, available}`: `device` is the system's name of the disk (`nvme0n1p2`), sizes in bytes.",
};
pub const NETWORK: Part = Part {
    method: "network",
    capability: "device.info.network",
    permission: "See how many bytes this device's network received and sent since it started. It sees no networks, addresses or what was carried.",
    answers: "Answers `{received, sent}` in bytes, every interface but the loopback.",
};
pub const BATTERY: Part = Part {
    method: "battery",
    capability: "device.info.battery",
    permission: "See this device's battery: its charge and status, whether it is plugged in, and its estimated time remaining.",
    answers: "Answers `{charge, status, plugged, minutes}`: `charge` in hundredths of a percent; `minutes` is an estimate over recent use, or `null` until one exists.",
};

/// Every part, in the order a person reads them.
pub const PARTS: [&Part; 6] = [&PROCESSOR, &GRAPHICS, &MEMORY, &STORAGE, &NETWORK, &BATTERY];

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Processor {
    pub name: Option<String>,
    pub cores: Option<u32>,
    /// The cores' clock now, on average, in megahertz.
    pub mhz: Option<u32>,
    /// Time spent busy and in all, in the system's ticks since it started: the share busy between two answers is the
    /// difference of `busy` over the difference of `total`.
    pub busy: Option<u64>,
    pub total: Option<u64>,
    pub celsius: Option<f32>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Graphics {
    pub name: Option<String>,
    /// The clock now, in megahertz.
    pub mhz: Option<u32>,
    /// Its own memory, used and in all, in bytes.
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    /// Share of the last moment it was busy, in hundredths of a percent.
    pub busy: Option<u16>,
    pub celsius: Option<f32>,
}

/// A space and how much of it is free, in bytes.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Space {
    /// The system's name of the disk the space is on, where it is a storage (never a memory).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub total: Option<u64>,
    pub available: Option<u64>,
}

/// Bytes received and sent by every interface but the loopback since the system started.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Network {
    pub received: Option<u64>,
    pub sent: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    Charging,
    Discharging,
    NotCharging,
    Full,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Battery {
    /// Charge, in hundredths of a percent.
    pub charge: Option<u16>,
    pub status: Option<BatteryStatus>,
    /// Whether external power is connected, independent of the state of the battery.
    pub plugged: Option<bool>,
    /// Estimated minutes until empty while discharging or until full while charging.
    pub minutes: Option<u32>,
}

/// What a platform says of the device through its own interface, where the files this module reads are closed to an
/// application (a phone's battery, network counters and processor ticks). Each `None` reads the files instead.
pub trait PlatformReadings: Send + Sync {
    fn battery(&self) -> Option<BatteryReading>;
    fn network(&self) -> Option<Network>;
    /// The first line of `/proc/stat` (`cpu …`), read where the platform can when the application may not.
    fn processor_stat(&self) -> Option<String>;
    /// The processor's name as the platform gives it, where `/proc/cpuinfo` names none (a phone's chip: `QTI SM8650`).
    fn processor_name(&self) -> Option<String>;
}

/// The answer of `method` now, or `None` for a method this service does not have. `storage` is the folder whose volume
/// [`STORAGE`] reports, unknown without one; `platform` says what its own interface knows where the files are closed, before
/// they are read. Only the part asked for is read.
pub(crate) fn answer(
    method: &str,
    storage: Option<&Path>,
    platform: Option<&dyn PlatformReadings>,
    battery_history: &mut BatteryHistory,
) -> Option<serde_json::Value> {
    let root = Path::new("/");
    let value = match method {
        m if m == PROCESSOR.method => serde_json::to_value(processor(root, platform)),
        m if m == GRAPHICS.method => serde_json::to_value(platform::graphics(root)),
        m if m == MEMORY.method => serde_json::to_value(platform::memory(root)),
        m if m == STORAGE.method => serde_json::to_value(Space {
            device: storage.and_then(platform::disk),
            total: storage.and_then(|path| fs2::total_space(path).ok()),
            available: storage.and_then(|path| fs2::available_space(path).ok()),
        }),
        m if m == NETWORK.method => serde_json::to_value(
            platform
                .and_then(PlatformReadings::network)
                .unwrap_or_else(|| platform::network(root)),
        ),
        m if m == BATTERY.method => {
            serde_json::to_value(battery_history.sample(std::time::Instant::now, || {
                let reading = platform
                    .and_then(PlatformReadings::battery)
                    .unwrap_or_else(|| platform::battery(root));
                (battery::sample_time(), reading)
            }))
        }
        _ => return None,
    };
    value.ok()
}

fn processor(root: &Path, platform: Option<&dyn PlatformReadings>) -> Processor {
    let mut processor = platform::processor(root);
    processor.cores = std::thread::available_parallelism()
        .ok()
        .and_then(|n| u32::try_from(n.get()).ok());
    // No name in `/proc/cpuinfo` (a phone): the platform's name for its chip, bounded and printable.
    if processor.name.is_none() {
        processor.name = platform
            .and_then(PlatformReadings::processor_name)
            .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|name| {
                !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
            });
    }
    // `/proc/stat` closed (a phone): the same line, read where the platform can, read the same way.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if processor.total.is_none()
        && let Some((busy, total)) = platform
            .and_then(PlatformReadings::processor_stat)
            .as_deref()
            .and_then(linux::processor_time)
    {
        processor.busy = Some(busy);
        processor.total = Some(total);
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = platform;
    processor
}

/// Where a system publishes nothing this file reads: every part unknown.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod platform {
    use super::{BatteryReading, Graphics, Network, Processor, Space};
    use std::path::Path;
    pub(super) fn processor(_: &Path) -> Processor {
        Processor::default()
    }
    pub(super) fn graphics(_: &Path) -> Graphics {
        Graphics::default()
    }
    pub(super) fn memory(_: &Path) -> Space {
        Space::default()
    }
    pub(super) fn network(_: &Path) -> Network {
        Network::default()
    }
    pub(super) fn battery(_: &Path) -> BatteryReading {
        BatteryReading::default()
    }
    pub(super) fn disk(_: &Path) -> Option<String> {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
use linux as platform;

/// Linux and Android: `/proc` and `/sys`, where readable, under `root` (another folder in tests).
#[cfg(any(target_os = "linux", target_os = "android", test))]
mod linux {
    use super::{
        Battery, BatteryReading, BatteryStatus, Graphics, Network, Processor, Space,
        battery::{Counter, Source},
    };
    use std::path::Path;

    /// A published file, read whole when it is small; `None` when it cannot be read or is not text.
    fn small(path: &Path) -> Option<String> {
        use std::io::Read;
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(256 * 1024)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    }
    fn number<T: std::str::FromStr>(path: &Path) -> Option<T> {
        small(path)?.trim().parse().ok()
    }
    /// The entries of a folder, sorted, so that the first of several is always the same one.
    fn entries(path: &Path) -> Vec<std::path::PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .take(256)
            .collect();
        found.sort();
        found
    }
    /// A temperature published in thousandths of a degree, if it is a plausible one.
    fn celsius(path: &Path) -> Option<f32> {
        let milli: i64 = number(path)?;
        let degrees = milli as f32 / 1000.;
        (-40. ..=150.).contains(&degrees).then_some(degrees)
    }

    pub(super) fn processor(root: &Path) -> Processor {
        let proc = root.join("proc");
        let (busy, total) = small(&proc.join("stat"))
            .and_then(|stat| processor_time(&stat))
            .unzip();
        let info = small(&proc.join("cpuinfo"));
        Processor {
            name: info.as_deref().and_then(processor_name),
            cores: None,
            mhz: info
                .as_deref()
                .and_then(processor_mhz)
                .or_else(|| processor_frequency(&root.join("sys/devices/system/cpu"))),
            busy,
            total,
            celsius: processor_celsius(&root.join("sys")),
        }
    }

    /// The average of the cores' `cpu MHz` lines (a computer's `/proc/cpuinfo`).
    pub(super) fn processor_mhz(info: &str) -> Option<u32> {
        let clocks: Vec<f64> = info
            .lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                (name.trim() == "cpu MHz").then(|| value.trim().parse::<f64>().ok())?
            })
            .filter(|mhz| mhz.is_finite() && (1. ..100_000.).contains(mhz))
            .collect();
        average(&clocks)
    }

    /// The average of the cores' current frequency where the system publishes it per core (in kilohertz), as phones do.
    fn processor_frequency(cpus: &Path) -> Option<u32> {
        let clocks: Vec<f64> = entries(cpus)
            .iter()
            .filter(|cpu| {
                cpu.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix("cpu"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter_map(|cpu| number::<u64>(&cpu.join("cpufreq/scaling_cur_freq")))
            .map(|khz| khz as f64 / 1000.)
            .filter(|mhz| (1. ..100_000.).contains(mhz))
            .collect();
        average(&clocks)
    }

    fn average(values: &[f64]) -> Option<u32> {
        (!values.is_empty())
            .then(|| (values.iter().sum::<f64>() / values.len() as f64).round() as u32)
    }

    /// The system's name of the disk `path` is on: the source of the deepest mount that holds it, when that source is a
    /// device (`/dev/nvme0n1p2` gives `nvme0n1p2`); a memory or overlay file system has none.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn disk(path: &Path) -> Option<String> {
        let path = std::fs::canonicalize(path).ok()?;
        disk_of(&small(Path::new("/proc/self/mountinfo"))?, &path)
    }

    pub(super) fn disk_of(mountinfo: &str, path: &Path) -> Option<String> {
        mountinfo
            .lines()
            .filter_map(|line| {
                let (left, right) = line.split_once(" - ")?;
                let point = left.split_whitespace().nth(4)?;
                let source = right.split_whitespace().nth(1)?;
                // Mount points escape spaces and the like as octal; such a point is never this application's folder's.
                path.starts_with(point).then_some((point.len(), source))
            })
            .max_by_key(|(depth, _)| *depth)
            .and_then(|(_, source)| source.strip_prefix("/dev/"))
            .and_then(|device| device.rsplit('/').next())
            .filter(|name| !name.is_empty() && name.len() <= 64)
            .map(str::to_owned)
    }

    pub(super) fn memory(root: &Path) -> Space {
        let Some(info) = small(&root.join("proc/meminfo")) else {
            return Space::default();
        };
        let field = |name: &str| {
            info.lines().find_map(|line| {
                let rest = line.strip_prefix(name)?.strip_prefix(':')?;
                let kib: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
                kib.checked_mul(1024)
            })
        };
        Space {
            device: None,
            total: field("MemTotal"),
            available: field("MemAvailable"),
        }
    }

    pub(super) fn network(root: &Path) -> Network {
        let (received, sent) = small(&root.join("proc/net/dev"))
            .and_then(|dev| network_bytes(&dev))
            .unzip();
        Network { received, sent }
    }

    /// Busy and total ticks of all processors together, from the first line of `/proc/stat`: user, nice, system, idle,
    /// iowait, irq, softirq, steal (guest time is already inside user and nice).
    pub(super) fn processor_time(stat: &str) -> Option<(u64, u64)> {
        let line = stat.lines().next()?.strip_prefix("cpu ")?;
        let fields: Vec<u64> = line
            .split_whitespace()
            .take(8)
            .map(|n| n.parse().ok())
            .collect::<Option<_>>()?;
        let total = fields.iter().try_fold(0u64, |sum, n| sum.checked_add(*n))?;
        let idle = fields.get(3)? + fields.get(4).copied().unwrap_or(0);
        Some((total.checked_sub(idle)?, total))
    }

    /// The processor's name as the system gives it (`model name` on a computer, `Hardware` on many phones).
    pub(super) fn processor_name(info: &str) -> Option<String> {
        ["model name", "Hardware", "Processor"]
            .iter()
            .find_map(|key| {
                info.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    let value = value.trim();
                    (name.trim() == *key && !value.is_empty() && value.len() <= 128)
                        .then(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
                })
            })
    }

    /// The processor's temperature from the sensor its driver names, else from the thermal zones that say they are the
    /// processor's: the package's where there is one (a computer), else the hottest of its cores' (a phone has one per core).
    fn processor_celsius(sys: &Path) -> Option<f32> {
        const SENSORS: [&str; 4] = ["coretemp", "k10temp", "zenpower", "cpu_thermal"];
        entries(&sys.join("class/hwmon"))
            .iter()
            .find(|hwmon| {
                small(&hwmon.join("name")).is_some_and(|name| SENSORS.contains(&name.trim()))
            })
            .and_then(|hwmon| celsius(&hwmon.join("temp1_input")))
            .or_else(|| hottest_zone(sys, |kind| kind == "x86_pkg_temp"))
            .or_else(|| hottest_zone(sys, |kind| kind.starts_with("cpu")))
    }

    /// The hottest readable thermal zone whose type (in lower case) `kind` accepts. Only `thermal_zone*`: the cooling devices
    /// beside them have types too (`cpu-hotplug4` on a phone) and no temperature, and one taken first hid every zone.
    fn hottest_zone(sys: &Path, kind: impl Fn(&str) -> bool) -> Option<f32> {
        entries(&sys.join("class/thermal"))
            .iter()
            .filter(|zone| {
                zone.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("thermal_zone"))
                    && small(&zone.join("type"))
                        .is_some_and(|said| kind(&said.trim().to_ascii_lowercase()))
            })
            .filter_map(|zone| celsius(&zone.join("temp")))
            .reduce(f32::max)
    }

    /// Received and sent bytes of every interface but the loopback, from `/proc/net/dev`.
    pub(super) fn network_bytes(dev: &str) -> Option<(u64, u64)> {
        let mut sums = None::<(u64, u64)>;
        for line in dev.lines().skip(2) {
            let Some((name, counters)) = line.split_once(':') else {
                continue;
            };
            if name.trim() == "lo" {
                continue;
            }
            let counters: Vec<u64> = counters
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if let (Some(received), Some(sent)) = (counters.first(), counters.get(8)) {
                let (r, s) = sums.unwrap_or_default();
                sums = Some((r.saturating_add(*received), s.saturating_add(*sent)));
            }
        }
        sums
    }

    /// The first graphics card that says who made it: its maker as its name, how busy it is when its driver says so,
    /// and its temperature. A phone has no such card, or one that says nothing: its graphics are the one `kgsl` device of
    /// its driver, when the system lets it be read.
    pub(super) fn graphics(root: &Path) -> Graphics {
        let card = entries(&root.join("sys/class/drm"))
            .into_iter()
            .find_map(|card| {
                let name = card.file_name()?.to_str()?;
                let digits = name.strip_prefix("card")?;
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let device = card.join("device");
                let vendor = small(&device.join("vendor"))?;
                Some((device, vendor.trim().to_owned()))
            });
        let Some((device, vendor)) = card else {
            return phone_graphics(root);
        };
        let card = Graphics {
            name: match vendor.as_str() {
                "0x10de" => Some("NVIDIA".to_owned()),
                "0x1002" => Some("AMD".to_owned()),
                "0x8086" => Some("Intel".to_owned()),
                _ => None,
            },
            mhz: graphics_mhz(&device),
            memory_used: number(&device.join("mem_info_vram_used")),
            memory_total: number::<u64>(&device.join("mem_info_vram_total")).filter(|n| *n > 0),
            busy: number::<u16>(&device.join("gpu_busy_percent"))
                .filter(|n| *n <= 100)
                .map(|n| n * 100),
            celsius: entries(&device.join("hwmon"))
                .iter()
                .find_map(|hwmon| celsius(&hwmon.join("temp1_input"))),
        };
        if card == Graphics::default() {
            phone_graphics(root)
        } else {
            card
        }
    }

    /// A phone's graphics, from its `kgsl` driver: the model it names, how busy it was over the driver's last window
    /// (`busy total`; both zero while it is powered down, which is idle), and the hottest of its thermal zones. Its clock
    /// and memory are not readable to an application, so they stay unknown.
    fn phone_graphics(root: &Path) -> Graphics {
        let kgsl = root.join("sys/class/kgsl/kgsl-3d0");
        let busy = small(&kgsl.join("gpubusy")).and_then(|said| {
            let mut numbers = said.split_whitespace().map(|n| n.parse::<u64>().ok());
            let (busy, total) = (numbers.next()??, numbers.next()??);
            if total == 0 {
                return (busy == 0).then_some(0);
            }
            u16::try_from(busy.min(total).saturating_mul(10_000) / total).ok()
        });
        let name = small(&kgsl.join("gpu_model"))
            .map(|model| model.trim().to_owned())
            .filter(|model| {
                !model.is_empty()
                    && model.len() <= 64
                    && model.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
            });
        if name.is_none() && busy.is_none() {
            return Graphics::default();
        }
        Graphics {
            name,
            mhz: None,
            memory_used: None,
            memory_total: None,
            busy,
            celsius: hottest_zone(&root.join("sys"), |kind| kind.starts_with("gpu")),
        }
    }

    /// The graphics clock now: the `sclk` its driver's sensor gives (hertz), else the level its driver marks as the
    /// current one (`1: 2375Mhz *`).
    fn graphics_mhz(device: &Path) -> Option<u32> {
        entries(&device.join("hwmon"))
            .iter()
            .find_map(|hwmon| number::<u64>(&hwmon.join("freq1_input")))
            .map(|hz| hz / 1_000_000)
            .or_else(|| {
                small(&device.join("pp_dpm_sclk"))
                    .as_deref()
                    .and_then(current_level)
            })
            .and_then(|mhz| u32::try_from(mhz).ok())
            .filter(|mhz| (1..100_000).contains(mhz))
    }

    pub(super) fn current_level(levels: &str) -> Option<u64> {
        levels.lines().find_map(|line| {
            let (_, rest) = line.split_once(':')?;
            let rest = rest.trim().strip_suffix('*')?.trim();
            rest.to_ascii_lowercase()
                .strip_suffix("mhz")?
                .trim()
                .parse()
                .ok()
        })
    }

    /// The first present battery's state and whether system power inputs are online.
    pub(super) fn battery(root: &Path) -> BatteryReading {
        let mut battery = None;
        let mut plugged = None;
        let mut unread = false;
        for supply in entries(&root.join("sys/class/power_supply")) {
            let kind = small(&supply.join("type"));
            match kind.as_deref().map(str::trim) {
                Some("Battery") if battery.is_none() => {
                    if number::<u8>(&supply.join("present")) != Some(0) {
                        battery = Some(supply);
                    }
                }
                Some(kind)
                    if matches!(kind, "Mains" | "Wireless" | "USB") || kind.starts_with("USB_") =>
                {
                    let scope = small(&supply.join("scope"));
                    let scope = scope.as_deref().map(str::trim);
                    if scope == Some("Device") {
                        continue;
                    }
                    let online = number::<u8>(&supply.join("online"));
                    if kind.starts_with("USB") {
                        // The power-supply ABI makes usb_type writable on a source and
                        // read-only on an input. Without it, system scope or an incoming
                        // current limit also identifies an input (e.g. AXP288).
                        match std::fs::metadata(supply.join("usb_type")) {
                            Ok(metadata) if !metadata.permissions().readonly() => continue,
                            Err(_)
                                if scope != Some("System")
                                    && number::<u64>(&supply.join("input_current_limit"))
                                        .is_none() =>
                            {
                                unread |= online != Some(0);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    match online {
                        Some(0) => {
                            plugged.get_or_insert(false);
                        }
                        // The kernel reports both fixed and programmable supplies as online.
                        Some(1 | 2) => plugged = Some(true),
                        _ => unread = true,
                    }
                }
                _ => {}
            }
        }
        // One online supply proves external power; offline requires a report from every known supply.
        let plugged = plugged.filter(|online| *online || !unread);
        let Some(supply) = battery else {
            return BatteryReading {
                battery: Battery {
                    plugged,
                    ..Battery::default()
                },
                counter: None,
            };
        };
        let status = small(&supply.join("status")).and_then(|status| match status.trim() {
            "Charging" => Some(BatteryStatus::Charging),
            "Discharging" => Some(BatteryStatus::Discharging),
            "Not charging" => Some(BatteryStatus::NotCharging),
            "Full" => Some(BatteryStatus::Full),
            _ => None,
        });
        let read = |name: &str| number::<u64>(&supply.join(name));
        let counter = match status {
            Some(status @ (BatteryStatus::Charging | BatteryStatus::Discharging)) => {
                battery_id(&supply).and_then(|id| {
                    let level = |now: &str, full: &str, source| {
                        let remaining = read(now)?;
                        let full = match status {
                            BatteryStatus::Charging => {
                                Some(read(full).filter(|n| *n >= remaining)?)
                            }
                            _ => None,
                        };
                        Some(Counter {
                            source,
                            remaining,
                            full,
                        })
                    };
                    level("energy_now", "energy_full", Source::Energy(id))
                        .or_else(|| level("charge_now", "charge_full", Source::Charge(id)))
                })
            }
            _ => None,
        };
        BatteryReading {
            battery: Battery {
                charge: number::<u16>(&supply.join("capacity"))
                    .filter(|n| *n <= 100)
                    .map(|n| n * 100),
                status,
                plugged,
                minutes: None,
            },
            counter,
        }
    }

    // Sysfs can reuse a supply name after replacement. Keep only an in-memory identifier, not
    // the optional serial itself. The unit is tracked separately by Source.
    fn battery_id(supply: &Path) -> Option<u64> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut id = DefaultHasher::new();
        supply.hash(&mut id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = supply.metadata().ok()?;
            (metadata.dev(), metadata.ino()).hash(&mut id);
        }
        small(&supply.join("serial_number"))
            .as_deref()
            .map(str::trim)
            .hash(&mut id);
        Some(id.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed to the application (a phone): the same line, through the platform, read the same way; never over the file. Only
    /// where `/proc` is the system's: elsewhere every part is unknown and the platform is not asked (`platform` above).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_phone_that_closes_proc_gives_the_same_counters_through_the_platform() {
        let stat = "cpu  100 5 50 800 40 3 2 0 7 0\ncpu0 1 2 3 4 5 6 7 8\n";
        struct Said(Option<&'static str>);
        impl PlatformReadings for Said {
            fn battery(&self) -> Option<BatteryReading> {
                None
            }
            fn network(&self) -> Option<Network> {
                None
            }
            fn processor_stat(&self) -> Option<String> {
                self.0.map(str::to_owned)
            }
            fn processor_name(&self) -> Option<String> {
                Some(" QTI   SM8650 ".to_owned())
            }
        }
        let closed = std::env::temp_dir().join(format!("peersus-closed-{}", std::process::id()));
        let through = processor(&closed, Some(&Said(Some(stat))));
        assert_eq!((through.busy, through.total), (Some(160), Some(1000)));
        // No `/proc/cpuinfo` name: the platform's, tidied.
        assert_eq!(through.name.as_deref(), Some("QTI SM8650"));
        let nothing = processor(&closed, Some(&Said(Some("intr 1 2\n"))));
        assert_eq!((nothing.busy, nothing.total), (None, None));
        assert_eq!(processor(&closed, None).total, None);
    }

    #[test]
    fn counters_are_read_as_the_system_writes_them_and_nothing_is_made_up() {
        let stat = "cpu  100 5 50 800 40 3 2 0 7 0\ncpu0 1 2 3 4 5 6 7 8\n";
        // Idle and waiting are not busy; guest time is already inside user time.
        assert_eq!(linux::processor_time(stat), Some((160, 1000)));
        assert_eq!(linux::processor_time("cpu0 1 2 3\n"), None);
        assert_eq!(linux::processor_time("cpu  1 x 3 4\n"), None);
        let dev = "Inter-|   Receive\n face |bytes\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n  eth0: 100 1 0 0 0 0 0 0 40 1 0 0 0 0 0 0\n wlan0: 5 1 0 0 0 0 0 0 6 1 0 0 0 0 0 0\n";
        assert_eq!(linux::network_bytes(dev), Some((105, 46)));
        assert_eq!(
            linux::network_bytes("a\nb\n    lo: 1 0 0 0 0 0 0 0 1\n"),
            None
        );
        assert_eq!(
            linux::processor_name("processor\t: 0\nmodel name\t: AMD  Ryzen 7   5800H\n")
                .as_deref(),
            Some("AMD Ryzen 7 5800H")
        );
        assert_eq!(
            linux::processor_name("Hardware\t: Qualcomm\n").as_deref(),
            Some("Qualcomm")
        );
        assert_eq!(linux::processor_name("flags: x\n"), None);
    }

    #[test]
    fn clocks_and_the_disk_are_read_as_published_and_unknown_otherwise() {
        assert_eq!(
            linux::processor_mhz("cpu MHz\t\t: 3000.000\ncpu MHz\t\t: 3500.5\n"),
            Some(3250)
        );
        assert_eq!(linux::processor_mhz("model name: x\n"), None);
        assert_eq!(linux::processor_mhz("cpu MHz: nan\n"), None);
        assert_eq!(
            linux::current_level("0: 500Mhz \n1: 2375Mhz *\n2: 2210Mhz \n"),
            Some(2375)
        );
        assert_eq!(linux::current_level("0: 500Mhz \n1: 2375Mhz \n"), None);
        let mounts = "22 1 0:21 / / rw - ext4 /dev/nvme0n1p2 rw\n\
            30 22 0:30 / /home rw - btrfs /dev/mapper/luks-abc rw\n\
            31 22 0:31 / /run rw - tmpfs tmpfs rw\n";
        let disk = |path: &str| linux::disk_of(mounts, Path::new(path));
        assert_eq!(disk("/var/data").as_deref(), Some("nvme0n1p2"));
        // The deepest mount that holds the folder, by its device's own name.
        assert_eq!(disk("/home/a/Downloads").as_deref(), Some("luks-abc"));
        // A memory file system is no disk.
        assert_eq!(disk("/run/user"), None);
    }

    #[test]
    fn each_part_is_read_alone_and_what_a_device_lacks_stays_unknown() {
        let root = std::env::temp_dir().join(format!("peersus-readings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "proc/meminfo",
            "MemTotal:       16000 kB\nMemFree: 1 kB\nMemAvailable:    4000 kB\n",
        );
        write("sys/class/hwmon/hwmon0/name", "nvme\n");
        write("sys/class/hwmon/hwmon1/name", "k10temp\n");
        write("sys/class/hwmon/hwmon1/temp1_input", "61250\n");
        write("sys/class/drm/card0-eDP-1/status", "connected\n");
        write("sys/class/drm/card1/device/vendor", "0x1002\n");
        write("sys/class/drm/card1/device/gpu_busy_percent", "37\n");
        write("sys/class/power_supply/AC/type", "Mains\n");
        write("sys/class/power_supply/BAT0/type", "Battery\n");
        write("sys/class/power_supply/BAT0/capacity", "81\n");
        write("sys/class/power_supply/BAT0/status", "Discharging\n");
        let memory = linux::memory(&root);
        assert_eq!(
            (memory.total, memory.available),
            (Some(16000 * 1024), Some(4000 * 1024))
        );
        let processor = linux::processor(&root);
        assert_eq!(processor.celsius, Some(61.25));
        // No stat: unknown, not zero.
        assert_eq!((processor.busy, processor.total), (None, None));
        let graphics = linux::graphics(&root);
        assert_eq!(graphics.name.as_deref(), Some("AMD"));
        assert_eq!((graphics.busy, graphics.celsius), (Some(3700), None));
        assert_eq!(
            linux::battery(&root).battery,
            Battery {
                charge: Some(8100),
                status: Some(BatteryStatus::Discharging),
                plugged: None,
                minutes: None,
            }
        );
        assert_eq!(linux::network(&root), Network::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn linux_reports_status_and_online_supply_independently() {
        use serde_json::json;

        let root = tempfile::tempdir().unwrap();
        let supplies = root.path().join("sys/class/power_supply");
        let write = |path: &str, text: &str| {
            let path = supplies.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("BAT0/type", "Battery\n");
        write("BAT0/capacity", "50\n");
        write("AC/type", "Mains\n");
        for (text, status) in [
            ("Charging\n", Some("charging")),
            ("Discharging\n", Some("discharging")),
            ("Not charging\n", Some("not_charging")),
            ("Full\n", Some("full")),
            ("Unknown\n", None),
            ("Other\n", None),
        ] {
            write("BAT0/status", text);
            for (online, plugged) in [("1\n", Some(true)), ("0\n", Some(false)), ("9\n", None)] {
                write("AC/online", online);
                assert_eq!(
                    serde_json::to_value(linux::battery(root.path()).battery).unwrap(),
                    json!({"charge":5000,"status":status,"plugged":plugged,"minutes":null})
                );
            }
        }
        write("AC/online", "0\n");
        write("WIRELESS/type", "Wireless\n");
        write("WIRELESS/online", "1\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(true));
        write("WIRELESS/online", "0\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(false));

        write("USB/type", "USB\n");
        write("USB/online", "1\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, None); // direction is not known
        write("USB/input_current_limit", "unknown\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, None);
        write("USB/input_current_limit", "900000\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(true));
        write("USB/online", "0\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(false));
        write("USB/type", "USB_PD\n");
        write("USB/online", "2\n");
        write("USB/usb_type", "[PD]\n"); // a writable usb_type is a source
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(false));
        let usb_type = supplies.join("USB/usb_type");
        let mut permissions = usb_type.metadata().unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&usb_type, permissions).unwrap();
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(true));
        write("USB/scope", "Device\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(false));
        write("USB/scope", "System\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, Some(true));
        write("USB/online", "9\n");
        assert_eq!(linux::battery(root.path()).battery.plugged, None);
    }

    #[test]
    fn linux_samples_the_energy_counter_instead_of_instant_power_and_tracks_replacement() {
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let battery = root.path().join("sys/class/power_supply/BAT0");
        std::fs::create_dir_all(&battery).unwrap();
        let write = |file: &str, value: &str| std::fs::write(battery.join(file), value).unwrap();
        write("type", "Battery\n");
        write("status", "Discharging\n");
        write("serial_number", "fixture-first\n");
        let mut history = BatteryHistory::default();
        let mut read = |seconds| {
            history
                .observe(
                    Some(Duration::from_secs(seconds)),
                    linux::battery(root.path()),
                )
                .minutes
        };
        for second in 0..=123 {
            let energy = 9_000_000 - second * 100;
            write("energy_now", &energy.to_string());
            write("power_now", if second % 2 == 0 { "1" } else { "20" });
            let expected = (second >= 122).then_some((energy / 6_000) as u32);
            assert_eq!(read(second), expected);
        }
        write("serial_number", "fixture-replacement\n");
        write("energy_now", "8987600");
        assert_eq!(read(124), None);
    }

    #[test]
    fn a_phone_says_its_processors_heat_through_its_cooling_devices_and_its_graphics_through_its_driver()
     {
        let root =
            std::env::temp_dir().join(format!("peersus-phone-readings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        // Listed first and called after the processor, with no temperature: it hid every zone before.
        write("sys/class/thermal/cooling_device0/type", "cpu-hotplug4\n");
        write("sys/class/thermal/thermal_zone3/type", "cpuss-0\n");
        write("sys/class/thermal/thermal_zone3/temp", "31900\n");
        write("sys/class/thermal/thermal_zone12/type", "cpu-1-0-0\n");
        write("sys/class/thermal/thermal_zone12/temp", "33500\n");
        write("sys/class/thermal/thermal_zone40/type", "gpuss-0\n");
        write("sys/class/thermal/thermal_zone40/temp", "30200\n");
        write("sys/class/thermal/thermal_zone41/type", "gpuss-1\n");
        write("sys/class/thermal/thermal_zone41/temp", "31000\n");
        // A zone whose temperature is not one (a limit level) says nothing.
        write("sys/class/thermal/thermal_zone50/type", "cpu-limit\n");
        write("sys/class/thermal/thermal_zone50/temp", "999999\n");
        write("sys/class/kgsl/kgsl-3d0/gpu_model", "Adreno750v2\n");
        write("sys/class/kgsl/kgsl-3d0/gpubusy", "  250000  1000000\n");
        // The hottest core.
        assert_eq!(linux::processor(&root).celsius, Some(33.5));
        let graphics = linux::graphics(&root);
        assert_eq!(graphics.name.as_deref(), Some("Adreno750v2"));
        assert_eq!((graphics.busy, graphics.celsius), (Some(2500), Some(31.0)));
        // What the driver does not let an application read stays unknown.
        assert_eq!((graphics.mhz, graphics.memory_total), (None, None));
        // Powered down: idle, not unknown.
        write("sys/class/kgsl/kgsl-3d0/gpubusy", "      0       0\n");
        assert_eq!(linux::graphics(&root).busy, Some(0));
        // A computer's package zone is its processor's temperature, before any core's.
        write("sys/class/thermal/thermal_zone60/type", "x86_pkg_temp\n");
        write("sys/class/thermal/thermal_zone60/temp", "45000\n");
        assert_eq!(linux::processor(&root).celsius, Some(45.0));
        // A card the system lists with a maker nobody here names and nothing else readable says nothing: the driver's
        // device is read instead.
        write("sys/class/drm/card0/device/vendor", "0x5143\n");
        assert_eq!(linux::graphics(&root).name.as_deref(), Some("Adreno750v2"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_part_is_a_method_of_its_own_with_a_permission_of_its_own() {
        let mut methods = std::collections::BTreeSet::new();
        let mut capabilities = std::collections::BTreeSet::new();
        let mut history = BatteryHistory::default();
        for part in PARTS {
            assert!(methods.insert(part.method) && capabilities.insert(part.capability));
            assert!(
                answer(part.method, None, None, &mut history).is_some(),
                "{} answers nothing",
                part.method
            );
        }
        assert_eq!(answer("readings", None, None, &mut history), None);
    }
}
