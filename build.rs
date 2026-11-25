fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Portable and installed copies only import system DLLs, including
        // during loader initialization before the consent helper reaches main.
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
        }
        println!("cargo:rerun-if-changed=assets/rustile.rc");
        println!("cargo:rerun-if-changed=assets/rustile.ico");
        embed_resource::compile_for_everything("assets/rustile.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Failed to embed the Rustile application icon");
    }
}
