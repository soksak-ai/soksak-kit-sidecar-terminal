//! Compile the darwin canvas unit. Rust owns grids, damage and the ring state
//! machine; `src/render/render_darwin.m` owns every Metal, CoreText and
//! IOSurface call (NATIVE-LAYER N2: native source lives in its own files).
//! Off darwin there is nothing to compile — the Rust side refuses by name.

fn main() {
    let target = std::env::var("TARGET").expect("Cargo supplies TARGET");
    if !target.contains("apple-darwin") {
        return;
    }
    println!("cargo:rerun-if-changed=src/render/render_darwin.m");
    println!("cargo:rerun-if-changed=src/render/render_darwin.h");
    cc::Build::new()
        .file("src/render/render_darwin.m")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .compile("soksak_render_darwin");
    for framework in ["Metal", "CoreText", "CoreGraphics", "IOSurface", "QuartzCore", "Foundation"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
