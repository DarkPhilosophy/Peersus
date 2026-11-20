fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        // Match the application's NDK r27 convention for every packaged cdylib.
        println!("cargo::rustc-link-arg-cdylib=-Wl,-z,max-page-size=16384");
        println!("cargo::rustc-link-arg-cdylib=-Wl,-z,common-page-size=16384");
    }
}
