fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        // NDK r27 and older do not default to 16 KiB pages. Apply both
        // flags to the shipped cdylib, without changing desktop or dependency builds.
        println!("cargo::rustc-link-arg-cdylib=-Wl,-z,max-page-size=16384");
        println!("cargo::rustc-link-arg-cdylib=-Wl,-z,common-page-size=16384");
    }
}
