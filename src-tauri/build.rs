fn main() {
    println!("cargo:rerun-if-changed=native/macos_wifi.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/macos_wifi.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=11.0")
            .compile("sdrwispy_wifi");
        println!("cargo:rustc-link-lib=objc");
        for framework in ["Foundation", "CoreWLAN", "CoreLocation"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
    tauri_build::build();
}
