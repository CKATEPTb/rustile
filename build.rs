//! Builds the Windows resources and translation tables embedded in rustile.exe.
//! Each message has the same named parameters in all twelve languages, though
//! translations may reorder them. The runtime indexes the generated UTF-8
//! data directly, so the installed application needs no catalog files.

use std::collections::BTreeMap;
use std::{env, fs, path::PathBuf};

fn placeholders(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some((before, tail)) = rest.split_once('{') {
        assert!(!before.contains('}'), "unmatched brace in {text}");
        let (name, after) = tail.split_once('}').expect("unclosed placeholder");
        assert!(
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid placeholder in {text}"
        );
        names.push(name);
        rest = after;
    }
    assert!(!rest.contains('}'), "unmatched brace in {text}");
    names.sort_unstable();
    names
}

fn compile_messages() {
    println!("cargo:rerun-if-changed=locales");
    let mut catalog = BTreeMap::new();
    for entry in fs::read_dir("locales").expect("read language catalogs") {
        let file = entry.expect("catalog entry").path();
        if file.extension().and_then(|s| s.to_str()) != Some("tsv") {
            continue;
        }
        for (index, row) in fs::read_to_string(&file)
            .expect("UTF-8 catalog")
            .lines()
            .enumerate()
        {
            if row.is_empty() || row.starts_with('#') {
                continue;
            }
            let mut cells = row.split('\t');
            let key = cells.next().unwrap();
            let values: Vec<_> = cells.map(|s| s.replace("\\n", "\n")).collect();
            assert_eq!(
                values.len(),
                12,
                "{}:{}: {key} requires 12 translations",
                file.display(),
                index + 1
            );
            let arguments = placeholders(&values[0]);
            for value in &values {
                assert!(
                    !value.trim().is_empty() && !value.contains('\0'),
                    "{key}: invalid translation"
                );
                assert_eq!(
                    placeholders(value),
                    arguments,
                    "{key}: placeholders in {value}"
                );
            }
            assert!(
                catalog.insert(key.to_owned(), values).is_none(),
                "duplicate key {key}"
            );
        }
    }
    assert!(!catalog.is_empty(), "missing translations");
    let mut code =
        String::from("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub(crate) enum Msg {\n");
    for key in catalog.keys() {
        code.push_str(&format!("    {key},\n"));
    }
    // Group each language's bytes together. Byte offsets keep the catalog
    // read-only when Windows relocates the executable at load time.
    code.push_str(&format!(
        "}}\nconst MESSAGE_COUNT: usize = {};\n",
        catalog.len()
    ));
    code.push_str("const CATALOGS: [(&str, &[u32; MESSAGE_COUNT + 1]); 12] = [\n");
    for language in 0..12 {
        let mut text = String::new();
        let mut offsets = vec![0u32];
        for values in catalog.values() {
            text.push_str(&values[language]);
            offsets.push(u32::try_from(text.len()).expect("language catalog exceeds 4 GiB"));
        }
        code.push_str(&format!("    ({text:?}, &{offsets:?}),\n"));
    }
    code.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("messages.rs"),
        code,
    )
    .unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=VERSION");
    assert_eq!(
        env!("CARGO_PKG_VERSION"),
        include_str!("VERSION").trim(),
        "refresh Cargo metadata with scripts/sync-version.ps1"
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        compile_messages();
        // Portable and installed copies only import system DLLs, including
        // during loader initialization before the consent helper reaches main.
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
        }
        println!("cargo:rerun-if-changed=LICENSE");
        println!("cargo:rerun-if-changed=assets/rustile.rc");
        println!("cargo:rerun-if-changed=assets/rustile.manifest");
        println!("cargo:rerun-if-changed=assets/rustile.ico");
        embed_resource::compile_for_everything("assets/rustile.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Failed to embed Rustile's Windows resources");
    }
}
