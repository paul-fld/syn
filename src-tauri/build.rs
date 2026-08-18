fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("apple-darwin") {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let object = out.join("native_macos.o");
        let library = out.join("libsyn_native_macos.a");
        let clang = std::process::Command::new("xcrun")
            .args([
                "clang",
                "-fobjc-arc",
                "-fblocks",
                "-c",
                "src/connectors/native_macos.m",
                "-o",
            ])
            .arg(&object)
            .status()
            .expect("clang macOS indisponible");
        assert!(clang.success(), "échec de compilation du pont natif macOS");
        let libtool = std::process::Command::new("xcrun")
            .args(["libtool", "-static", "-o"])
            .arg(&library)
            .arg(&object)
            .status()
            .expect("libtool macOS indisponible");
        assert!(libtool.success(), "échec d’édition du pont natif macOS");
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=syn_native_macos");
        for framework in [
            "Contacts",
            "AppKit",
            "EventKit",
            "Photos",
            "ApplicationServices",
            "CoreGraphics",
            "CoreServices",
            "Foundation",
            "Vision",
            "Speech",
            "AVFoundation",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rerun-if-changed=src/connectors/native_macos.m");
    }
    tauri_build::build()
}
