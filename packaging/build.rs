use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../VERSION");
    assert_eq!(
        env!("CARGO_PKG_VERSION"),
        include_str!("../VERSION").trim(),
        "refresh Cargo metadata with scripts/sync-version.ps1"
    );
    println!("cargo:rerun-if-env-changed=RUSTILE_RELEASE_CAB");
    println!("cargo:rerun-if-env-changed=RUSTILE_RELEASE_SHA256");
    let cabinet =
        env::var_os("RUSTILE_RELEASE_CAB").expect("build with scripts/package-release.ps1");
    println!(
        "cargo:rerun-if-changed={}",
        PathBuf::from(cabinet).display()
    );
    let hash = env::var("RUSTILE_RELEASE_SHA256").expect("missing payload SHA-256");
    assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
    let digest: Vec<_> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hash[i..i + 2], 16).unwrap())
        .collect();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("payload.sha256"), digest).unwrap();

    println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
    for path in [
        "package.rc",
        "../assets/rustile.manifest",
        "../assets/rustile.ico",
        "../LICENSE",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    embed_resource::compile_for_everything("package.rc", embed_resource::NONE)
        .manifest_required()
        .expect("could not embed release package resources");
}
