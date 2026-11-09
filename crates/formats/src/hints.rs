//! Toolchain, runtime and packer hints: "this was written in Go", "this is an Electron app",
//! "this is packed with UPX". Each hint carries the evidence it rests on.

use crate::binary::{Binary, Format};
use crate::find_bytes;
use serde::Serialize;

/// How far into the file byte searches look.
const SCAN_LIMIT: usize = 48 << 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hint {
    /// Stable id: go, rust, dotnet, electron, flutter, delphi, upx, …
    pub id: &'static str,
    /// language, runtime, framework, compiler, packer, installer, protector
    pub category: &'static str,
    pub label: String,
    /// strong (a format structure or unique marker) or weak (a string that could be borrowed)
    pub strength: &'static str,
    pub evidence: String,
}

fn hint(id: &'static str, category: &'static str, label: impl Into<String>, strength: &'static str, evidence: impl Into<String>) -> Hint {
    Hint { id, category, label: label.into(), strength, evidence: evidence.into() }
}

/// Look for toolchain, runtime and packer fingerprints.
pub fn detect(b: &Binary, data: &[u8]) -> Vec<Hint> {
    let mut out: Vec<Hint> = Vec::new();
    let sec = |n: &str| b.sections.iter().any(|s| s.name == n);
    let sec_prefix = |p: &str| b.sections.iter().find(|s| s.name.starts_with(p)).map(|s| s.name.clone());
    let sym = |n: &str| b.symbols.iter().any(|s| s.name == n || s.name.strip_prefix('_') == Some(n));
    let sym_contains = |n: &str| b.symbols.iter().take(200_000).find(|s| s.name.contains(n)).map(|s| s.name.clone());
    let lib = |n: &str| b.libraries.iter().find(|l| l.to_ascii_lowercase().contains(&n.to_ascii_lowercase())).cloned();
    let imp = |n: &str| b.imports.iter().any(|i| i.name == n || i.name.strip_prefix('_') == Some(n));
    let bytes = |n: &[u8]| find_bytes(data, n, SCAN_LIMIT).is_some();
    let push = |out: &mut Vec<Hint>, h: Hint| {
        if !out.iter().any(|x| x.id == h.id) {
            out.push(h);
        }
    };

    // Go
    if sec(".go.buildinfo") || sec("__go_buildinfo") {
        push(&mut out, hint("go", "language", "Go", "strong", "Go build-info section"));
    } else if let Some(at) = find_bytes(data, b"\xff Go buildinf:", SCAN_LIMIT) {
        push(&mut out, hint("go", "language", "Go", "strong", format!("Go build-info marker at file offset {at:#x}")));
    } else if sec(".gopclntab") || sec("__gopclntab") || sym("runtime.main") {
        push(&mut out, hint("go", "language", "Go", "strong", "Go runtime symbols or pclntab section"));
    }
    if let Some(h) = out.iter_mut().find(|h| h.id == "go") {
        let version = crate::go::inline_version(data, SCAN_LIMIT)
            .map(|v| (v, "the build information"))
            .or_else(|| go_release_in(data).map(|v| (v, "a Go release name in the data")));
        if let Some((v, from)) = version {
            h.label = format!("Go ({v})");
            h.evidence = format!("{}; version from {from}", h.evidence);
        }
    }
    // Rust
    if let Some(s) = sym_contains("rust_begin_unwind").or_else(|| sym_contains("rust_panic")).or_else(|| sym_contains("4core9panicking")) {
        push(&mut out, hint("rust", "language", "Rust", "strong", format!("symbol {s}")));
    } else if bytes(b"/rustc/") || bytes(b"RUST_BACKTRACE") {
        push(&mut out, hint("rust", "language", "Rust", "weak", "Rust standard-library paths or panic text in the data"));
    }
    // .NET
    if b.info.contains_key("dotnet") {
        push(&mut out, hint("dotnet", "runtime", ".NET (C#, F# or VB)", "strong", "CLR runtime header"));
    } else if lib("mscoree.dll").is_some() {
        push(&mut out, hint("dotnet", "runtime", ".NET (C#, F# or VB)", "weak", "imports mscoree.dll"));
    }
    if sym("DotNetRuntimeInfo") || bytes(b"RhpNewFast") {
        push(&mut out, hint("dotnet_aot", "runtime", ".NET Native AOT", "weak", "NativeAOT runtime helpers"));
    }
    // Electron / Chromium / Node
    if bytes(b"electron.asar") || bytes(b"ELECTRON_RUN_AS_NODE") {
        push(
            &mut out,
            hint(
                "electron",
                "framework",
                "Electron (a web app inside a desktop app)",
                "strong",
                "Electron runtime strings (electron.asar / ELECTRON_RUN_AS_NODE)",
            ),
        );
    } else if lib("libnode").is_some() || sym_contains("node::Start").is_some() {
        push(&mut out, hint("node", "runtime", "Node.js runtime", "weak", "Node.js symbols or library"));
    }
    // Flutter / Dart
    if lib("libflutter").is_some() || lib("Flutter.framework").is_some() {
        push(&mut out, hint("flutter", "framework", "Flutter (Dart)", "strong", "links the Flutter engine"));
    } else if sym("kDartVmSnapshotData") || sym("_kDartVmSnapshotData") || sym("kDartIsolateSnapshotInstructions") {
        push(&mut out, hint("flutter", "framework", "Flutter (Dart AOT snapshot)", "strong", "Dart snapshot symbols"));
    }
    // Swift / Objective-C
    if let Some(s) = sec_prefix("__swift5") {
        push(&mut out, hint("swift", "language", "Swift", "strong", format!("section {s}")));
    }
    if sec("__objc_classlist") || sec("__objc_imageinfo") {
        push(&mut out, hint("objc", "language", "Objective-C", "strong", "Objective-C metadata sections"));
    }
    // Delphi / C++Builder
    if sec(".itext") || (sec("CODE") && sec("DATA")) || bytes(b"Embarcadero") || bytes(b"Borland\0") || bytes(b"FastMM") {
        let ev = if sec(".itext") { "an .itext section" } else { "Borland/Embarcadero markers" };
        push(&mut out, hint("delphi", "language", "Delphi or C++Builder", if sec(".itext") { "strong" } else { "weak" }, ev));
    }
    // Python bundlers
    if bytes(b"MEI\x0c\x0b\x0a\x0b\x0e") || bytes(b"_MEIPASS") {
        push(&mut out, hint("pyinstaller", "packer", "PyInstaller (a bundled Python program)", "strong", "PyInstaller archive cookie"));
    } else if imp("Py_Initialize") || lib("python3").is_some() {
        push(&mut out, hint("python", "runtime", "Embedded Python", "weak", "Python runtime import"));
    }
    // Other languages and frameworks
    if sym_contains("NimMain").is_some() || bytes(b"@nimMain") {
        push(&mut out, hint("nim", "language", "Nim", "weak", "Nim runtime symbol"));
    }
    if lib("Qt5Core").or_else(|| lib("Qt6Core")).or_else(|| lib("QtCore")).is_some() {
        push(&mut out, hint("qt", "framework", "Qt", "strong", "links Qt"));
    }
    if lib("UnityPlayer").is_some() || lib("libil2cpp").is_some() || bytes(b"il2cpp") {
        push(&mut out, hint("unity", "framework", "Unity (IL2CPP)", "weak", "Unity/IL2CPP markers"));
    }
    if b.exports.iter().any(|e| e.name == "JNI_OnLoad" || e.name.starts_with("Java_")) {
        push(&mut out, hint("jni", "framework", "Java native library (JNI)", "strong", "exports JNI entry points"));
    }
    if lib("libSystem").is_none()
        && lib("msvcrt").is_none()
        && lib("libc.so").is_none()
        && b.format == Format::Elf
        && b.libraries.is_empty()
        && b.kind == "executable"
    {
        push(&mut out, hint("static", "compiler", "Statically linked", "weak", "no shared libraries needed"));
    }
    // Compilers
    if let Some(c) = b.info.get("compiler") {
        let label = if c.contains("clang") {
            "Clang/LLVM"
        } else if c.contains("GCC") {
            "GCC"
        } else {
            "a C/C++ compiler"
        };
        push(&mut out, hint("compiler", "compiler", label, "strong", format!(".comment: {c}")));
    }
    if b.info.contains_key("rich_header") {
        push(&mut out, hint("msvc", "compiler", "Microsoft Visual C++ toolchain", "strong", "Rich header"));
    }
    if sym_contains("__mingw").is_some() || bytes(b"Mingw-w64 runtime failure") {
        push(&mut out, hint("mingw", "compiler", "MinGW (GCC for Windows)", "weak", "MinGW runtime markers"));
    }
    // Packers and protectors (section names are the classic tell).
    let packers: [(&str, &'static str, &str, &'static str); 12] = [
        ("UPX", "upx", "UPX", "packer"),
        (".aspack", "aspack", "ASPack", "packer"),
        (".adata", "aspack", "ASPack", "packer"),
        (".MPRESS", "mpress", "MPRESS", "packer"),
        (".themida", "themida", "Themida/WinLicense", "protector"),
        (".winlice", "themida", "Themida/WinLicense", "protector"),
        (".vmp", "vmprotect", "VMProtect", "protector"),
        (".enigma", "enigma", "Enigma Protector", "protector"),
        (".petite", "petite", "Petite", "packer"),
        (".nsp", "nspack", "NsPack", "packer"),
        ("pec", "pecompact", "PECompact", "packer"),
        (".packed", "packed", "an unknown packer", "packer"),
    ];
    for (prefix, id, label, cat) in packers {
        if let Some(s) = b.sections.iter().find(|s| s.name.starts_with(prefix)) {
            push(&mut out, hint(id, cat, label, "strong", format!("section {}", s.name)));
        }
    }
    if !out.iter().any(|h| h.id == "upx") && bytes(b"UPX!") && b.format != Format::Raw {
        push(&mut out, hint("upx", "packer", "UPX", "weak", "UPX! marker in the data"));
    }
    // Installers
    if bytes(b"Nullsoft") {
        push(&mut out, hint("nsis", "installer", "NSIS installer", "weak", "Nullsoft marker"));
    }
    if bytes(b"Inno Setup") {
        push(&mut out, hint("inno", "installer", "Inno Setup installer", "weak", "Inno Setup marker"));
    }
    if bytes(b"AU3!") {
        push(&mut out, hint("autoit", "language", "AutoIt script", "weak", "AU3! marker"));
    }
    // High-entropy code means compressed or encrypted code.
    if let Some(s) = b.sections.iter().filter(|s| s.exec && s.file_size >= 4096).find(|s| s.entropy.is_some_and(|e| e > 7.2)) {
        push(
            &mut out,
            hint(
                "high_entropy",
                "packer",
                "Compressed or encrypted code",
                "weak",
                format!("section {} has entropy {:.2} bits/byte", s.name, s.entropy.unwrap_or(0.0)),
            ),
        );
    }
    out
}

/// The first Go release name (`go1.N`, `go1.N.P`, `go1.NrcK`, `go1.NbetaK`) standing on its own in
/// the data. Other text after a `go1.` (long digit runs, identifiers) isn't taken as a version.
fn go_release_in(data: &[u8]) -> Option<String> {
    let mut from = 0usize;
    for _ in 0..4096 {
        let at = from.checked_add(find_bytes(data.get(from..)?, b"go1.", SCAN_LIMIT.saturating_sub(from))?)?;
        let starts_word = at.checked_sub(1).and_then(|p| data.get(p)).is_none_or(|c| !c.is_ascii_alphanumeric());
        if starts_word && let Some(len) = data.get(at..).and_then(go_release_len) {
            return data.get(at..at.checked_add(len)?).map(|v| String::from_utf8_lossy(v).into_owned());
        }
        from = at.checked_add(1)?;
    }
    None
}

/// The length of the Go release name at the start of `s`, when one ends cleanly there.
fn go_release_len(s: &[u8]) -> Option<usize> {
    let digits = |at: usize, most: usize| {
        let n = s.get(at..).unwrap_or_default().iter().take(most + 1).take_while(|c| c.is_ascii_digit()).count();
        (1..=most).contains(&n).then_some(at + n)
    };
    let mut end = digits(4, 3)?;
    if s.get(end) == Some(&b'.') {
        end = digits(end + 1, 3)?;
    }
    if let Some(tag) = [&b"rc"[..], b"beta"].into_iter().find(|tag| s.get(end..).is_some_and(|rest| rest.starts_with(tag))) {
        end = digits(end + tag.len(), 2)?;
    }
    let continues = s.get(end).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_'));
    (!continues).then_some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An inline (Go 1.18+) build information block holding `version` and an empty module.
    fn inline_buildinfo(version: &str) -> Vec<u8> {
        let mut block = b"\xff Go buildinf:\x08\x02".to_vec();
        block.resize(32, 0);
        block.push(version.len() as u8);
        block.extend_from_slice(version.as_bytes());
        block.push(0);
        block
    }

    #[test]
    fn go_version_comes_from_the_build_information_before_stray_text() {
        let (elf, _) = craft_anything_testkit::elf64(craft_anything_testkit::ElfArch::X86_64, true);
        let b = crate::parse_binary(&elf).unwrap();
        let mut go = elf;
        go.extend_from_slice(b"\0go1.190119941996\0go1.21.0\0");
        go.extend_from_slice(&inline_buildinfo("go1.25.0"));
        let hints = detect(&b, &go);
        let g = hints.iter().find(|h| h.id == "go").unwrap();
        assert_eq!(g.label, "Go (go1.25.0)");
        assert!(g.evidence.ends_with("version from the build information"), "{}", g.evidence);
    }

    #[test]
    fn go_version_from_the_data_is_a_whole_release_name() {
        let found = |s: &[u8]| go_release_in(s);
        assert_eq!(found(b"go1.190119941996"), None);
        assert_eq!(found(b"\0go1.190119941996\0go1.25.0\0"), Some("go1.25.0".into()));
        assert_eq!(found(b"cargo1.2 go1.2.3.4 go1.x go1. go1.22_x"), None);
        assert_eq!(found(b"built with go1.21rc2 today"), Some("go1.21rc2".into()));
        assert_eq!(found(b"go1.20beta1"), Some("go1.20beta1".into()));
        assert_eq!(found(b"go1.9"), Some("go1.9".into()));
        assert_eq!(found(b"go1.21.0 X:boringcrypto"), Some("go1.21.0".into()));
        assert_eq!(found(b"go1.21rc"), None);
    }

    #[test]
    fn go_inline_version_skips_stray_magic_and_odd_strings() {
        let mut data = b"\xff Go buildinf:\x08\x00".to_vec(); // pointer form: not read here
        data.extend_from_slice(&inline_buildinfo("not a version"));
        assert_eq!(crate::go::inline_version(&data, usize::MAX), None);
        data.extend_from_slice(&inline_buildinfo("devel go1.26-abc"));
        assert_eq!(crate::go::inline_version(&data, usize::MAX).as_deref(), Some("devel go1.26-abc"));
        assert_eq!(crate::go::inline_version(&data[..data.len() - 4], usize::MAX), None);
        assert_eq!(crate::go::inline_version(&data, 10), None);
    }

    #[test]
    fn finds_markers() {
        let (elf, _) = craft_anything_testkit::elf64(craft_anything_testkit::ElfArch::X86_64, true);
        let b = crate::parse_binary(&elf).unwrap();
        assert!(detect(&b, &elf).iter().all(|h| h.id != "go"));
        let mut go = elf.clone();
        go.extend_from_slice(b"\xff Go buildinf:\x08\x02go1.22.3\0");
        let hints = detect(&b, &go);
        let g = hints.iter().find(|h| h.id == "go").unwrap();
        assert_eq!(g.label, "Go (go1.22.3)");
        let pe = craft_anything_testkit::pe64();
        let b = crate::parse_binary(&pe).unwrap();
        let _ = detect(&b, &pe);
    }
}
