//! The two independent extras from Android's `ACTION_BATTERY_CHANGED`.
use peersus_app::system_readings::BatteryStatus;

pub(super) fn decode(
    status: Option<i32>,
    plugged: Option<i32>,
) -> (Option<BatteryStatus>, Option<bool>) {
    let status = match status {
        // BatteryManager.BATTERY_STATUS_*. UNKNOWN (1) and other codes say nothing.
        Some(2) => Some(BatteryStatus::Charging),
        Some(3) => Some(BatteryStatus::Discharging),
        Some(4) => Some(BatteryStatus::NotCharging),
        Some(5) => Some(BatteryStatus::Full),
        _ => None,
    };
    let plugged = match plugged {
        Some(0) => Some(false),
        // BatteryManager.BATTERY_PLUGGED_{AC,USB,WIRELESS,DOCK}.
        Some(1 | 2 | 4 | 8) => Some(true),
        _ => None,
    };
    (status, plugged)
}
