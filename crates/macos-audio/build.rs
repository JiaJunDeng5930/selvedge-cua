fn main() {
    println!("cargo:rerun-if-changed=src/capture.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/capture.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .flag("-mmacosx-version-min=14.0")
            .compile("selvedge_audio_capture");
        for framework in ["Foundation", "CoreAudio", "AVFoundation"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
