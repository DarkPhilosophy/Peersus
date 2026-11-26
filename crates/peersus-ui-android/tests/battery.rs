#[path = "../src/battery.rs"]
mod battery;

use peersus_app::system_readings::BatteryStatus;

#[test]
fn android_battery_status_and_plugged_codes_are_independent() {
    for (status_code, status) in [
        (Some(2), Some(BatteryStatus::Charging)),
        (Some(3), Some(BatteryStatus::Discharging)),
        (Some(4), Some(BatteryStatus::NotCharging)),
        (Some(5), Some(BatteryStatus::Full)),
        (Some(1), None),
        (Some(99), None),
        (None, None),
    ] {
        for (plugged_code, plugged) in [
            (Some(0), Some(false)),
            (Some(1), Some(true)),
            (Some(2), Some(true)),
            (Some(4), Some(true)),
            (Some(8), Some(true)),
            (Some(99), None),
            (Some(-1), None),
            (None, None),
        ] {
            assert_eq!(
                battery::decode(status_code, plugged_code),
                (status, plugged)
            );
        }
    }
}
