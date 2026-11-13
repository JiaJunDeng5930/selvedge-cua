fn main() {
    println!("cargo:rerun-if-changed=src/capture.m");
    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("src/capture.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .compile("selvedge_screen_capture");
        for framework in ["Foundation", "CoreGraphics", "ScreenCaptureKit"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
