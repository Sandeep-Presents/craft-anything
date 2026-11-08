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

/// The original's pictures for a run's first message, at most `max`, relative to the project
/// folder: the person's pictures in reference/ first, then the captured screenshots (not the
/// noise masks, which only say what moves) of the start page and of each other page studied
/// (`capture/routes/<page>/`), every page's first picture before any page's second, so the other
/// pages are shown too and not only the start page at every width. Returns the pictures and how
/// many were left out.
pub(super) fn for_first_message(ws: &Workspace, max: usize) -> (Vec<PathBuf>, usize) {
    let references: Vec<Vec<PathBuf>> = ws.references().iter().filter_map(|p| p.strip_prefix(&ws.root).ok().map(|p| vec![p.to_path_buf()])).collect();
    let mut page_dirs = vec![ws.capture_dir()];
    let mut routes: Vec<PathBuf> =
        std::fs::read_dir(ws.capture_dir().join("routes")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    routes.sort();
    page_dirs.extend(routes);
    let pages: Vec<Vec<Vec<PathBuf>>> = page_dirs
        .iter()
        .map(|dir| {
            let mut shots: Vec<PathBuf> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_png(p) && !file_name(p).starts_with("noise-"))
                .filter_map(|p| p.strip_prefix(&ws.root).ok().map(Path::to_path_buf))
                .collect();
            shots.sort();
            shots.into_iter().map(|p| vec![p]).collect()
        })
        .collect();
    let mut picked = Vec::new();
    let mut left_out = take_in_turn(&[references], max, &mut picked);
    left_out += take_in_turn(&pages, max, &mut picked);
    (picked, left_out)
}

/// A round's pictures of the build, at most `max`: those of the checks that weren't good enough
/// first, and in each check the pages that look least alike first, every page's first picture
/// before any page's second (a site's start page at three widths would otherwise fill a message,
/// and its other pages would never be shown). Each build picture comes after the original's
/// picture at the same place under capture/ when there is one, and the two go together or not at
/// all. `exists` says whether a project-relative file is there. Returns the pictures and how many
/// were left out.
pub(super) fn for_check(checks: &[CheckResult], good_enough: f64, max: usize, exists: impl Fn(&Path) -> bool) -> (Vec<PathBuf>, usize) {
    let mut ordered: Vec<&CheckResult> = checks.iter().filter(|c| !c.good(good_enough)).collect();
    ordered.extend(checks.iter().filter(|c| c.good(good_enough)));
    let mut picked = Vec::new();
    let mut left_out = 0;
    for c in ordered {
        let round = PathBuf::from("checks").join(format!("round-{}", c.round));
        // The check's pictures by page (the folder under checks/round-N/), in the order it lists them.
        let mut pages: Vec<(PathBuf, Vec<Vec<PathBuf>>)> = Vec::new();
        for built in c.artifacts.iter().filter(|p| is_picture(p)) {
            let rel = built.strip_prefix(&round).ok();
            let mut pair = Vec::new();
            if let Some(original) = rel.map(|r| PathBuf::from("capture").join(r)).filter(|o| exists(o)) {
                pair.push(original);
            }
            pair.push(built.clone());
            let dir = rel.and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
            match pages.iter_mut().find(|(d, _)| *d == dir) {
                Some((_, pairs)) => pairs.push(pair),
                None => pages.push((dir, vec![pair])),
            }
        }
        // Least alike first; a page the check gave no score keeps its place after those it did.
        let alike = |dir: &Path| c.pages.iter().find(|p| Path::new(&p.dir) == dir).map_or(f64::INFINITY, |p| p.similarity);
        pages.sort_by(|a, b| alike(&a.0).total_cmp(&alike(&b.0)));
        let pages: Vec<Vec<Vec<PathBuf>>> = pages.into_iter().map(|(_, pairs)| pairs).collect();
        left_out += take_in_turn(&pages, max, &mut picked);
    }
    (picked, left_out)
}

/// Add groups of pictures (a picture, or an original and the build's picture of the same place)
/// to `picked` while they fit in `max`: each page's first group, then each page's second, and so
/// on. A group goes in whole or not at all; a picture already picked isn't added again. Returns
/// how many pictures didn't fit.
fn take_in_turn(pages: &[Vec<Vec<PathBuf>>], max: usize, picked: &mut Vec<PathBuf>) -> usize {
    let mut left_out = 0;
    let deepest = pages.iter().map(Vec::len).max().unwrap_or(0);
    for k in 0..deepest {
        for group in pages.iter().filter_map(|p| p.get(k)) {
            let new: Vec<PathBuf> = group.iter().filter(|p| !picked.contains(p)).cloned().collect();
            if picked.len() + new.len() <= max {
                picked.extend(new);
            } else {
                left_out += new.len();
            }
        }
    }
    left_out
}

/// The largest picture file read to be shrunk; past it a picture isn't shown.
const MAX_SOURCE_BYTES: u64 = 64_000_000;

/// Read pictures (project-relative paths) as image blocks named by their path. One larger than
/// [`MAX_IMAGE_BYTES`], or longer than [`MAX_SIDE`] on a side, is shrunk to fit, and its name
/// says at what scale it's shown, so sizes
/// read off it can be scaled back. Those that can't be shown (missing, not a picture, too large
/// to read) come back with why, to be said.
pub(crate) fn load(root: &Path, paths: &[PathBuf]) -> (Vec<Block>, Vec<String>) {
    load_within(root, paths, MAX_IMAGE_BYTES)
}

fn load_within(root: &Path, paths: &[PathBuf], max: usize) -> (Vec<Block>, Vec<String>) {
    use std::io::Read as _;
    let (mut shown, mut skipped) = (Vec::new(), Vec::new());
    for rel in paths {
        let mut name = rel.display().to_string().replace('\\', "/");
        let mut bytes = Vec::new();
        let read = std::fs::File::open(root.join(rel)).and_then(|f| f.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes));
        let fitted = read.map_err(|e| e.to_string()).and_then(|_| {
            if bytes.len() as u64 > MAX_SOURCE_BYTES {
                return Err(format!("larger than the {} MB read to be shrunk", MAX_SOURCE_BYTES / 1_000_000));
            }
            fit(bytes, max)
        });
        match fitted.and_then(|f| {
            if let Some(s) = f.shrunk {
                name = format!("{name} (shown at {}%: {}×{} of {}×{} px)", s.percent(), s.width, s.height, s.from_width, s.from_height);
            }
            Block::image(name.clone(), &f.bytes).map_err(|e| e.message)
        }) {
            Ok(b) => shown.push(b),
            Err(why) => skipped.push(format!("{name} ({why})")),
        }
    }
    (shown, skipped)
}

/// A picture's bytes as they'll be shown, and how it was shrunk to fit, if it was.
pub(super) struct Fitted {
    pub bytes: Vec<u8>,
    pub shrunk: Option<Shrunk>,
}

/// The size a picture is shown at and the size it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Shrunk {
    pub width: u32,
    pub height: u32,
    pub from_width: u32,
    pub from_height: u32,
}

impl Shrunk {
    /// The scale it's shown at, in whole percent (at least 1).
    pub fn percent(&self) -> u32 {
        (u64::from(self.width) * 100 / u64::from(self.from_width.max(1))).clamp(1, 100) as u32
    }
}

/// The longest side a picture is shown with: Anthropic refuses a picture over 8000 px on either
/// side, however few bytes it is (a full-page screenshot can be that tall).
pub(super) const MAX_SIDE: u32 = 8000;

/// `bytes` as they are when they fit in `max` and no side is over [`MAX_SIDE`]; otherwise the
/// picture decoded and scaled down, keeping its shape, until it fits as a PNG. Each try scales by
/// the square root of how far the last was over, a little smaller, so a few tries do it.
pub(super) fn fit(bytes: Vec<u8>, max: usize) -> Result<Fitted, String> {
    use craft_anything_engine::tools::web_common::{decode_picture, encode_png_within, picture_dimensions, resize};
    let too_long = picture_dimensions(&bytes).is_ok_and(|(w, h)| w.max(h) > MAX_SIDE);
    if bytes.len() <= max && !too_long {
        return Ok(Fitted { bytes, shrunk: None });
    }
    let img = decode_picture(&bytes).map_err(|e| e.message)?;
    let (from_width, from_height) = (img.width.max(1), img.height.max(1));
    let by_bytes = if bytes.len() > max { (max as f64 / bytes.len() as f64).sqrt() * 0.9 } else { 1.0 };
    let mut scale = by_bytes.min(f64::from(MAX_SIDE) / f64::from(from_width.max(from_height)));
    for _ in 0..8 {
        let width = ((f64::from(from_width) * scale) as u32).clamp(1, MAX_SIDE);
        let height = ((f64::from(from_height) * scale) as u32).clamp(1, MAX_SIDE);
        let small = resize(&img, width, height);
        if let Some(png) = encode_png_within(&small, max).map_err(|e| e.message)? {
            return Ok(Fitted { bytes: png, shrunk: Some(Shrunk { width, height, from_width, from_height }) });
        }
        if width == 1 && height == 1 {
            break;
        }
        scale *= 0.7;
    }
    Err(format!("couldn't be made smaller than {} MB", max as f64 / 1_000_000.0))
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
        let (picked, left_out) = for_check(&[program, web], 80.0, 6, |p| originals.contains(p));
        let picked: Vec<String> = picked.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            picked,
            [
                // The check that wasn't good enough first, each build picture after the original's
                // (the about page beside the about page, not the start page); each page's first
                // picture before any page's second; one with no original stands alone, and a text
                // file isn't a picture.
                "capture/screenshot-1280.png",
                "checks/round-3/screenshot-1280.png",
                "capture/routes/about/screenshot-1280.png",
                "checks/round-3/routes/about/screenshot-1280.png",
                "checks/round-3/screenshot-390.png",
                "checks/round-3/program/screenshot.png",
            ]
        );
        assert_eq!(left_out, 0);
    }

    /// A site checked at three widths: its start page alone would fill a message, so its other
    /// pages were never shown. Within the limit, the least alike pages come first at their first
    /// width, a pair is never split, and what didn't fit is counted.
    #[test]
    fn a_sites_other_pages_are_shown_within_the_limit() {
        use craft_anything_engine::workspace::PageLikeness;
        let mut artifacts = Vec::new();
        let mut pages = Vec::new();
        for (dir, similarity) in [("", 85.0), ("routes/about", 40.0), ("routes/shop", 60.0), ("routes/blog", 90.0)] {
            for w in [1440, 768, 390] {
                artifacts.push(format!("checks/round-2/{dir}{}screenshot-{w}.png", if dir.is_empty() { "" } else { "/" }));
            }
            let name = if dir.is_empty() { "start page".to_string() } else { format!("/{}", &dir[7..]) };
            pages.push(PageLikeness { name, dir: dir.into(), similarity });
        }
        let artifacts: Vec<&str> = artifacts.iter().map(String::as_str).collect();
        let web = CheckResult { pages, ..check(CheckResult::WEB_VISUAL, 2, Some(69.0), &artifacts) };
        let every_original = |p: &Path| p.starts_with("capture");

        let (picked, left_out) = for_check(std::slice::from_ref(&web), 80.0, 6, every_original);
        let picked: Vec<String> = picked.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            picked,
            [
                "capture/routes/about/screenshot-1440.png",
                "checks/round-2/routes/about/screenshot-1440.png",
                "capture/routes/shop/screenshot-1440.png",
                "checks/round-2/routes/shop/screenshot-1440.png",
                "capture/screenshot-1440.png",
                "checks/round-2/screenshot-1440.png",
            ]
        );
        assert_eq!(left_out, 24 - 6, "four pages at three widths, each with its original");

        // An odd limit leaves the next pair out whole rather than its original alone.
        let (picked, left_out) = for_check(std::slice::from_ref(&web), 80.0, 5, every_original);
        assert_eq!(picked.len(), 4);
        assert_eq!(left_out, 20);
        // With room for everything, every picture goes, each page's widths in turn.
        let (picked, left_out) = for_check(std::slice::from_ref(&web), 80.0, 100, every_original);
        assert_eq!((picked.len(), left_out), (24, 0));
        assert_eq!(picked[6].display().to_string(), "capture/routes/blog/screenshot-1440.png", "the most alike page last in each turn");
        assert_eq!(picked[8].display().to_string(), "capture/routes/about/screenshot-768.png");
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
        let picked: Vec<String> = for_first_message(&ws, 6).0.iter().map(|p| p.display().to_string().replace('\\', "/")).collect();
        assert_eq!(picked, ["reference/picture-1.png", "capture/screenshot-1280.png", "capture/screenshot-390.png"]);

        // The other pages studied are shown too, each page's first picture before any second,
        // within the limit, with what didn't fit counted.
        for page in ["shop", "about"] {
            let dir = ws.capture_dir().join("routes").join(page);
            std::fs::create_dir_all(&dir).unwrap();
            for f in ["screenshot-390.png", "screenshot-1280.png", "noise-1280.png"] {
                std::fs::write(dir.join(f), png).unwrap();
            }
        }
        let (picked, left_out) = for_first_message(&ws, 5);
        let picked: Vec<String> = picked.iter().map(|p| p.display().to_string().replace('\\', "/")).collect();
        assert_eq!(
            picked,
            [
                "reference/picture-1.png",
                "capture/screenshot-1280.png",
                "capture/routes/about/screenshot-1280.png",
                "capture/routes/shop/screenshot-1280.png",
                "capture/screenshot-390.png",
            ]
        );
        assert_eq!(left_out, 2);
        std::fs::remove_dir_all(ws.capture_dir().join("routes")).unwrap();

        // Each is read and named by its path; one that can't be shown is said, not dropped silently.
        std::fs::write(ws.capture_dir().join("screenshot-390.png"), b"not a picture").unwrap();
        let mut asked: Vec<PathBuf> = for_first_message(&ws, 6).0;
        asked.push(PathBuf::from("capture/gone.png"));
        let (shown, skipped) = load(&ws.root, &asked);
        let names: Vec<&str> = shown.iter().filter_map(|b| if let Block::Image { name, .. } = b { Some(name.as_str()) } else { None }).collect();
        assert_eq!(names, ["reference/picture-1.png", "capture/screenshot-1280.png"]);
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(skipped[0].starts_with("capture/screenshot-390.png (") && skipped[1].starts_with("capture/gone.png ("), "{skipped:?}");
    }

    /// A PNG of noise (which hardly compresses), `w`×`h`.
    fn noise_png(w: u32, h: u32) -> Vec<u8> {
        use craft_anything_engine::tools::web_common::{Image, encode_png};
        let mut x = 0x2545_f491_u32;
        let rgba = (0..w * h * 4)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                if i % 4 == 3 { 255 } else { x as u8 }
            })
            .collect();
        encode_png(&Image { width: w, height: h, rgba }).unwrap()
    }

    #[test]
    fn a_picture_over_the_limit_is_shrunk_to_fit_keeping_its_shape() {
        use craft_anything_engine::tools::web_common::png_dimensions;
        let big = noise_png(300, 200);
        let max = big.len() / 5;
        let f = fit(big.clone(), max).unwrap();
        let s = f.shrunk.unwrap();
        assert!(f.bytes.len() <= max, "{} > {max}", f.bytes.len());
        assert_eq!(png_dimensions(&f.bytes).unwrap(), (s.width, s.height));
        assert_eq!((s.from_width, s.from_height), (300, 200));
        let shape = f64::from(s.width) / f64::from(s.height);
        assert!((shape - 1.5).abs() < 0.05, "{s:?}");
        assert!(s.percent() < 100, "{s:?}");
        // One that fits is left exactly as it is.
        let same = fit(big.clone(), big.len()).unwrap();
        assert!(same.shrunk.is_none() && same.bytes == big);
        // Something over the limit that isn't a picture can't be shrunk, and says so.
        assert!(fit(vec![7; 100], 10).is_err());
    }

    #[test]
    fn a_picture_longer_than_the_limit_on_a_side_is_shrunk_though_its_bytes_fit() {
        use craft_anything_engine::tools::web_common::{Image, encode_png, png_dimensions};
        let tall = |h: u32| encode_png(&Image { width: 100, height: h, rgba: vec![200; 100 * h as usize * 4] }).unwrap();
        // A full-page screenshot: few bytes (one colour), but taller than a model is shown.
        let long = tall(MAX_SIDE + 1000);
        assert!(long.len() < MAX_IMAGE_BYTES);
        let f = fit(long, MAX_IMAGE_BYTES).unwrap();
        let s = f.shrunk.unwrap();
        assert_eq!((s.from_width, s.from_height), (100, MAX_SIDE + 1000));
        assert_eq!(png_dimensions(&f.bytes).unwrap(), (s.width, s.height));
        assert_eq!((s.width, s.height), (88, MAX_SIDE), "its shape kept, its long side at the limit");
        // One at the limit is left exactly as it is.
        let edge = tall(MAX_SIDE);
        let same = fit(edge.clone(), MAX_IMAGE_BYTES).unwrap();
        assert!(same.shrunk.is_none() && same.bytes == edge);
    }

    #[test]
    fn a_shrunk_picture_is_named_with_the_scale_its_shown_at() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("capture")).unwrap();
        let big = noise_png(300, 200);
        std::fs::write(d.path().join("capture/screenshot-3840.png"), &big).unwrap();
        let (shown, skipped) = load_within(d.path(), &[PathBuf::from("capture/screenshot-3840.png")], big.len() / 5);
        assert!(skipped.is_empty(), "{skipped:?}");
        let Some(Block::Image { name, .. }) = shown.first() else { panic!("{shown:?}") };
        assert!(name.starts_with("capture/screenshot-3840.png (shown at ") && name.ends_with(" of 300×200 px)"), "{name}");
    }
}
