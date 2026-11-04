fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        let source = std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("../../target/extensions/upload-android.tar.gz");
        println!("cargo::rerun-if-changed={}", source.display());
        let destination = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
            .join("upload-android.tar.gz");
        std::fs::copy(source, destination)
            .expect("Build the bundled WASM payload with cargo xtask android-dev-native first");
        // NDK r27 and older do not default to 16 KiB pages. Apply both
        // flags to the shipped cdylib, without changing desktop or dependency builds.
        println!("cargo::rustc-link-arg-cdylib=-Wl,-z,max-page-size=16384");
        println!("cargo::rustc-link-arg-cdylib=-Wl,-z,common-page-size=16384");
    }
}
