fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/rustile.rc");
        println!("cargo:rerun-if-changed=assets/rustile.ico");
        embed_resource::compile_for_everything("assets/rustile.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Failed to embed the Rustile application icon");
    }
}
