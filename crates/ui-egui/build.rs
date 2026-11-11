//! Optional craft-fonts (fallbacks for CJK, Arabic, Thai, Hebrew, Devanagari…), following
//! craft-fonts' `docs/integration.md`. Releases and CI set it.
//!
//! Unset `CRAFT_FONTS_DIR` checks for a sibling `craft-fonts` checkout in the parent directory
//! (`../craft-fonts`) or in the repository root (`craft-fonts`). If none is found, the app embeds
//! nothing and uses its own DM Sans/JetBrains Mono (CJK and other non-Latin scripts will show as
//! boxes). Set `CRAFT_FONTS_DIR` to an absolute path of a craft-fonts checkout to embed every font
//! its manifest lists. A bad path is a warning unless `CRAFT_FONTS_REQUIRED=1`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_REQUIRED");
    let mut src = String::from("pub static CRAFT_FONTS: &[CraftFont] = &[\n");
    let required = std::env::var_os("CRAFT_FONTS_REQUIRED").is_some();
    if let Some(dir) = find_craft_fonts_dir() {
        match craft_fonts(&dir) {
            Ok(entries) => src.push_str(&entries),
            Err(e) if required => {
                println!("cargo::error=craft-fonts {}: {e}", dir.display());
            }
            Err(e) => println!("cargo::warning=building without craft-fonts: {}: {e}", dir.display()),
        }
    } else if required {
        println!("cargo::error=CRAFT_FONTS_REQUIRED is set but no craft-fonts directory was found");
    } else {
        println!(
            "cargo::warning=building without craft-fonts: CJK and non-Latin scripts will show as boxes. Clone https://github.com/storytold/craft-fonts next to this repository or set CRAFT_FONTS_DIR."
        );
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_fonts.rs");
    if let Err(e) = std::fs::write(&out, src) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}

fn find_craft_fonts_dir() -> Option<PathBuf> {
    if let Some(dir_os) = std::env::var_os("CRAFT_FONTS_DIR") {
        let dir = PathBuf::from(dir_os);
        if dir.as_os_str().is_empty() || dir.as_os_str() == "0" || dir.as_os_str() == "none" {
            return None;
        }
        return Some(dir);
    }
    // Auto-detect sibling checkout in workspace parent (e.g. `../craft-fonts`) or repo root (`craft-fonts`).
    if let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) {
        let candidates = [manifest_dir.join("../../../craft-fonts"), manifest_dir.join("../../craft-fonts")];
        for candidate in candidates {
            if let Some(canon) = candidate.canonicalize().ok().filter(|c| c.join("fonts/manifest.txt").is_file()) {
                return Some(canon);
            }
        }
    }
    None
}

/// One `CraftFont { .. }` initialiser per manifest line.
fn craft_fonts(dir: &Path) -> Result<String, String> {
    let manifest = dir.join("fonts/manifest.txt");
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    const WEB_FONTS: &[(&str, &str)] = &[("BIZ UDPGothic", "Regular")];
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32");
    let mut out = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        if wasm && !WEB_FONTS.contains(&(*family, *style)) {
            continue;
        }
        let path = dir.join(file).canonicalize().map_err(|e| format!("{file}: {e}"))?;
        println!("cargo::rerun-if-changed={}", path.display());
        let scripts: Vec<String> = scripts.split(',').map(|s| format!("{:?}", s.trim())).collect();
        let _ = writeln!(
            out,
            "    CraftFont {{ family: {family:?}, style: {style:?}, scripts: &[{}], bytes: include_bytes!({:?}) }},",
            scripts.join(", "),
            path.display().to_string(),
        );
    }
    Ok(out)
}
