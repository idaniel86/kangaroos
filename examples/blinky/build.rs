use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::copy("memory.x", out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");

    if env::var("CARGO_FEATURE_DEFMT").is_ok() {
        println!("cargo:rustc-link-arg=-Tdefmt.x");
    }
}