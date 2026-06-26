fn main() {
    // Declare all custom cfgs so rustc's check-cfg lint doesn't fire,
    // regardless of which target is active in a given build.
    println!("cargo::rustc-check-cfg=cfg(armv6m)");
    println!("cargo::rustc-check-cfg=cfg(armv7m)");
    println!("cargo::rustc-check-cfg=cfg(armv7em)");
    println!("cargo::rustc-check-cfg=cfg(armv8m)");
    println!("cargo::rustc-check-cfg=cfg(has_fpu)");

    let target = std::env::var("TARGET").unwrap();

    // Emit cfg flags based on the target triple so the kernel can use
    // #[cfg(armv6m)], #[cfg(armv7m)], #[cfg(armv7em)], #[cfg(armv8m)],
    // and #[cfg(has_fpu)] — no manual --features needed, --target drives it.
    match target.as_str() {
        "thumbv6m-none-eabi" => {
            println!("cargo:rustc-cfg=armv6m");
        }
        "thumbv7m-none-eabi" => {
            println!("cargo:rustc-cfg=armv7m");
        }
        "thumbv7em-none-eabi" => {
            println!("cargo:rustc-cfg=armv7em");
        }
        "thumbv7em-none-eabihf" => {
            println!("cargo:rustc-cfg=armv7em");
            println!("cargo:rustc-cfg=has_fpu");
        }
        "thumbv8m.base-none-eabi" => {
            println!("cargo:rustc-cfg=armv8m");
        }
        "thumbv8m.main-none-eabi" => {
            println!("cargo:rustc-cfg=armv8m");
        }
        "thumbv8m.main-none-eabihf" => {
            println!("cargo:rustc-cfg=armv8m");
            println!("cargo:rustc-cfg=has_fpu");
        }
        _ => {}
    }

    // Only emit -Tdefmt.x when the defmt feature is active.
    // The defmt crate's own build script adds defmt.x to the linker search
    // path; we just need to tell the linker to actually use it.
    if std::env::var("CARGO_FEATURE_DEFMT").is_ok() {
        println!("cargo:rustc-link-arg=-Tdefmt.x");
    }

    println!("cargo:rerun-if-changed=build.rs");
}
