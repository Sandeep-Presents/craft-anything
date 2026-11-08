//! The pictures a builder whose model sees is shown with its messages (R5.4): the person's
//! pictures and the original's screenshots with a run's first message, and after each check its
//! pictures of the build, each after the original's picture of the same page and width.
//!
//! Agent apps open pictures with their own tools; this is for the API loop, whose model gets them
//! attached ([`crate::Conversation::attach`]). What is picked is decided here from the project's
//! folders and the check's typed `artifacts`; the reading is separate so the picking can be tested
//! without files.

use crate::model::{Block, MAX_IMAGE_BYTES};
use craft_anything_engine::workspace::{CheckResult, Workspace};
use std::path::{Path, PathBuf};

/// The original's pictures for a run's first message, relative to the project folder: the
/// person's pictures in reference/ first, then the captured screenshots (not the noise masks,
/// which only say what moves).
pub(super) fn for_first_message(ws: &Workspace) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ws.references().iter().filter_map(|p| p.strip_prefix(&ws.root).ok().map(Path::to_path_buf)).collect();
    let mut shots: Vec<PathBuf> = std::fs::read_dir(ws.capture_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_png(p) && !file_name(p).starts_with("noise-"))
        .filter_map(|p| p.strip_prefix(&ws.root).ok().map(Path::to_path_buf))
        .collect();
    shots.sort();
    v.extend(shots);
    v
}

/// A round's pictures of the build, those of the checks that weren't good enough first, each
/// after the original's picture at the same place under capture/ when there is one.
/// `exists` says whether a project-relative file is there.
pub(super) fn for_check(checks: &[CheckResult], good_enough: f64, exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut ordered: Vec<&CheckResult> = checks.iter().filter(|c| !c.good(good_enough)).collect();
    ordered.extend(checks.iter().filter(|c| c.good(good_enough)));
    let mut v = Vec::new();
    for c in ordered {
        let round = PathBuf::from("checks").join(format!("round-{}", c.round));
        for built in c.artifacts.iter().filter(|p| is_picture(p)) {
            if let Ok(rel) = built.strip_prefix(&round) {
                let original = PathBuf::from("capture").join(rel);
                if exists(&original) && !v.contains(&original) {
                    v.push(original);
                }
            }
            if !v.contains(built) {
                v.push(built.clone());
            }
        }
    }
    v
}

/// Read pictures (project-relative paths) as image blocks named by their path. Those that can't
/// be shown (missing, not a picture, over [`MAX_IMAGE_BYTES`]) come back with why, to be said.
pub(super) fn load(root: &Path, paths: &[PathBuf]) -> (Vec<Block>, Vec<String>) {
    use std::io::Read as _;
    let (mut shown, mut skipped) = (Vec::new(), Vec::new());
    for rel in paths {
        let name = rel.display().to_string().replace('\\', "/");
        let mut bytes = Vec::new();
        let read = std::fs::File::open(root.join(rel)).and_then(|f| f.take(MAX_IMAGE_BYTES as u64 + 1).read_to_end(&mut bytes));
        match read.map_err(|e| e.to_string()).and_then(|_| Block::image(name.clone(), &bytes).map_err(|e| e.message)) {
            Ok(b) => shown.push(b),
            Err(why) => skipped.push(format!("{name} ({why})")),
        }
    }
    (shown, skipped)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn is_png(p: &Path) -> bool {
    file_name(p).ends_with(".png")
}

fn is_picture(p: &Path) -> bool {
    let n = file_name(p);
    [".png", ".jpg", ".jpeg", ".gif", ".webp"].iter().any(|e| n.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn check(id: &str, round: u32, similarity: Option<f64>, artifacts: &[&str]) -> CheckResult {
        CheckResult { similarity, artifacts: artifacts.iter().map(PathBuf::from).collect(), ..CheckResult::new(id, round, true, String::new()) }
    }

    #[test]
    fn a_checks_pictures_come_after_the_originals_of_the_same_page() {
        let originals: BTreeSet<PathBuf> = ["capture/screenshot-1280.png", "capture/routes/about/screenshot-1280.png"].iter().map(PathBuf::from).collect();
        let web = check(
            CheckResult::WEB_VISUAL,
            3,
            Some(62.0),
            &[
                "checks/round-3/screenshot-1280.png",
                "checks/round-3/screenshot-390.png",
                "checks/round-3/routes/about/screenshot-1280.png",
                "checks/round-3/log.txt",
            ],
        );
        let program = check("screenshot", 3, Some(95.0), &["checks/round-3/program/screenshot.png"]);
        let picked = for_check(&[program, web], 80.0, |p| originals.contains(p));
        let picked: Vec<String> = picked.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            picked,
            [
                // The check that wasn't good enough first, each build picture after the original's
                // (the about page beside the about page, not the start page); one with no original
                // stands alone, and a text file isn't a picture.
                "capture/screenshot-1280.png",
                "checks/round-3/screenshot-1280.png",
                "checks/round-3/screenshot-390.png",
                "capture/routes/about/screenshot-1280.png",
                "checks/round-3/routes/about/screenshot-1280.png",
                "checks/round-3/program/screenshot.png",
            ]
        );
    }

    #[test]
    fn the_first_message_shows_the_persons_pictures_then_the_captured_ones() {
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Site", "a site like this", craft_anything_engine::workspace::TargetRef::None).unwrap();
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        let shot = d.path().join("shot.png");
        std::fs::write(&shot, png).unwrap();
        ws.add_references(&[shot]).unwrap();
        std::fs::create_dir_all(ws.capture_dir()).unwrap();
        for f in ["screenshot-390.png", "screenshot-1280.png", "noise-1280.png", "README.md"] {
            std::fs::write(ws.capture_dir().join(f), png).unwrap();
        }
        let picked: Vec<String> = for_first_message(&ws).iter().map(|p| p.display().to_string().replace('\\', "/")).collect();
        assert_eq!(picked, ["reference/picture-1.png", "capture/screenshot-1280.png", "capture/screenshot-390.png"]);

        // Each is read and named by its path; one that can't be shown is said, not dropped silently.
        std::fs::write(ws.capture_dir().join("screenshot-390.png"), b"not a picture").unwrap();
        let mut asked: Vec<PathBuf> = for_first_message(&ws);
        asked.push(PathBuf::from("capture/gone.png"));
        let (shown, skipped) = load(&ws.root, &asked);
        let names: Vec<&str> = shown.iter().filter_map(|b| if let Block::Image { name, .. } = b { Some(name.as_str()) } else { None }).collect();
        assert_eq!(names, ["reference/picture-1.png", "capture/screenshot-1280.png"]);
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(skipped[0].starts_with("capture/screenshot-390.png (") && skipped[1].starts_with("capture/gone.png ("), "{skipped:?}");
    }
}
