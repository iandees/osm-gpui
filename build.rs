// The message catalogs in `locales/` are embedded by the `rust_i18n::i18n!`
// macro at compile time; this makes cargo rebuild the crate when one changes
// (a proc macro's file reads are otherwise invisible to cargo).
//
// The preset translations in `assets/presets/translations/<locale>.json` are
// embedded by a generated list (`$OUT_DIR/preset_translations.rs`), so adding
// a locale is just dropping a file into that directory.
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=locales");

    // Embed the app icon (and the version info) in the Windows executable.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/AppIcon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/AppIcon.ico");
        res.compile().expect("failed to embed the Windows icon");
    }

    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("assets/presets/translations");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut locales: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
                .filter_map(|p| {
                    let tag = p.file_stem()?.to_str()?.to_string();
                    Some((tag, p))
                })
                .collect()
        })
        .unwrap_or_default();
    locales.sort();

    let mut generated = String::from("pub const BUNDLED: &[(&str, &str)] = &[\n");
    for (tag, path) in &locales {
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!(
            "    ({tag:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    generated.push_str("];\n");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("preset_translations.rs");
    std::fs::write(out, generated).unwrap();

    embed_preset_icons();
}

// The preset icons in `assets/presets/icons/` are embedded as a table sorted
// by icon name (`$OUT_DIR/preset_icons.rs`), looked up by binary search and
// served to GPUI through `assets::AppAssets`.
fn embed_preset_icons() {
    let dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/presets/icons");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut icons: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "svg"))
                .filter_map(|p| {
                    let name = p.file_stem()?.to_str()?.to_string();
                    Some((name, p))
                })
                .collect()
        })
        .unwrap_or_default();
    icons.sort();

    let mut generated = String::from("pub const PRESET_ICONS: &[(&str, &str)] = &[\n");
    for (name, path) in &icons {
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!(
            "    ({name:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    generated.push_str("];\n");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("preset_icons.rs");
    std::fs::write(out, generated).unwrap();
}
