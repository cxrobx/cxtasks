//! App icons for the sidebar, found in a project's own repos.
//!
//! Most areas and projects ARE an app with an icon sitting in its repo — a
//! favicon, an `apple-touch-icon`, a Tauri `icon.png`, an Xcode `AppIcon`. This
//! finds those, turns each into what the sidebar can draw, and hands them to the
//! picker as `data:` URIs. Nothing is stored here: the picker sends the one the
//! person chose to `set_project_appearance`, which validates it again
//! (`db::appearance`) because that row syncs to every device.
//!
//! Host-local by nature — it reads files on THIS Mac — so it is a Tauri command
//! and not a verb in the shared table, like `fetch_vault_look`.
//!
//! # What "turn it into an icon" means
//!
//! 1. **Trim** transparent padding. A macOS app icon is a squircle inside a
//!    transparent margin, so drawn at 16px untrimmed it looks a size smaller than
//!    a favicon beside it.
//! 2. **Optionally cut out a solid background** — when all four corners are the
//!    same opaque colour, that colour becomes transparent (with a soft edge) and
//!    the result is trimmed again. Offered as a SECOND candidate, never applied
//!    silently: a white-on-black logo cut out is white on nothing, invisible on
//!    the light theme. PocketBuddy's logo is the case it exists for.
//! 3. Pad to a square and scale to 64px — sharp on a Retina sidebar, and a few KB.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::Engine;
use image::{imageops::FilterType, DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::Serialize;
use tauri::State;

use crate::db;
use crate::error::AppError;
use crate::{AppState, LockExt};

/// The output edge, in pixels.
const SIZE: u32 = 64;
/// How deep to walk a repo. The icons live near the top (`public/`,
/// `src-tauri/icons/`, `…/Assets.xcassets/AppIcon.appiconset/`), and a shallow
/// walk is what keeps this fast on a monorepo.
const MAX_DEPTH: usize = 6;
/// Directories that are never where an app keeps its icon, and are where a
/// walk goes to die.
const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", ".next", "build", "dist", "out", "DerivedData",
    "coverage", ".venv", "venv", "__pycache__", "Pods", ".build", ".turbo", "vendor",
];
/// Tiles offered, at most — two rows in the picker.
const MAX_CANDIDATES: usize = 10;
/// Source images taken from any one directory. An Xcode `AppIcon.appiconset`
/// holds three near-identical renders (light, dark, tinted), and without this
/// they fill every slot and push out the one logo in `branding/` that was the
/// right answer (PocketBuddy's, 2026-09-27).
const MAX_PER_DIR: usize = 2;
/// A file larger than this is a screenshot or artwork, not an icon.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IconCandidate {
    /// Where it came from, relative to the repo — what the tile's caption shows.
    pub label: String,
    pub data_url: String,
    /// The background was cut out of this one.
    pub cutout: bool,
}

/// Every icon in `project`'s repos, best first.
#[tauri::command]
pub async fn repo_icon_candidates(
    state: State<'_, AppState>,
    project: String,
) -> Result<Vec<IconCandidate>, AppError> {
    let repos: Vec<PathBuf> = {
        let conn = state.db.safe_lock();
        db::projects::list_repos(&conn, &project)?
            .into_iter()
            .map(|r| PathBuf::from(r.repo_path))
            .collect()
    };
    tauri::async_runtime::spawn_blocking(move || candidates_in(&repos))
        .await
        .map_err(|e| AppError::General(format!("icon search failed: {e}")))
}

/// One image the person picked with the file dialog: as-is, and cut out when
/// it has a solid background.
#[tauri::command]
pub async fn icon_from_file(path: String) -> Result<Vec<IconCandidate>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path);
        let img = load(&path).ok_or_else(|| {
            AppError::Validation(format!("{} is not an image this can read (PNG, JPEG or ICO)", path.display()))
        })?;
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(variants(&img, &label))
    })
    .await
    .map_err(|e| AppError::General(format!("icon conversion failed: {e}")))?
}

fn candidates_in(repos: &[PathBuf]) -> Vec<IconCandidate> {
    let mut found: Vec<(i64, PathBuf, PathBuf)> = Vec::new();
    for repo in repos {
        walk(repo, 0, &mut |path| {
            if let Some(score) = name_score(path) {
                found.push((score, repo.clone(), path.to_path_buf()));
            }
        });
    }

    // Rank on the header alone — dimensions without decoding — and decode only
    // what is actually offered. A repo holds dozens of 1024px renders, and
    // decoding every one was seconds on finance-app.
    let mut scored: Vec<(i64, String, PathBuf, PathBuf)> = found
        .into_iter()
        .filter_map(|(name_score, repo, path)| {
            let (w, h) = dimensions(&path)?;
            let aspect = w as f64 / h as f64;
            // An icon is square. A wide `logo.png` is a wordmark, which at 16px
            // is a smear.
            if w.min(h) < 32 || !(0.85..=1.18).contains(&aspect) {
                return None;
            }
            let label = format!(
                "{}/{}",
                repo.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                path.strip_prefix(&repo).unwrap_or(&path).display()
            );
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            Some((name_score + i64::from(w.min(h).min(512)), label, dir, path))
        })
        .collect();
    // Shorter path first on a tie: the same favicon is often copied into a
    // marketing folder, and `web/src/app/favicon.ico` is the better caption.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(&b.1)));

    let mut out: Vec<IconCandidate> = Vec::new();
    let mut per_dir: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    for (_, label, dir, path) in scored {
        let taken = per_dir.entry(dir).or_default();
        if *taken >= MAX_PER_DIR {
            continue;
        }
        let Some(img) = load(&path) else { continue };
        *taken += 1;
        for c in variants(&img, &label) {
            // The same icon ships at several sizes; after scaling to 64px they
            // are often byte-identical, and one tile per image is enough.
            if !out.iter().any(|o| o.data_url == c.data_url) {
                out.push(c);
            }
        }
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    out.truncate(MAX_CANDIDATES);
    out
}

/// How likely a file NAME is to be an app icon; `None` = not one.
fn name_score(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !matches!(ext.as_str(), "png" | "ico" | "jpg" | "jpeg") {
        return None;
    }
    let in_app_icon_set = path.components().any(|c| c.as_os_str().to_string_lossy().starts_with("AppIcon"));
    let score = if in_app_icon_set {
        300
    } else if name.starts_with("apple-touch-icon") {
        260
    } else if name.contains("logo") && name.contains("icon") {
        240
    } else if name == "icon.png" || name == "app-icon.png" || name == "icon.ico" {
        200
    } else if name.contains("128x128@2x") {
        180
    } else if name.starts_with("favicon") {
        120
    } else {
        return None;
    };
    Some(score)
}

fn walk(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue; // a symlinked tree is somebody else's repo, or a loop
        }
        if kind.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                walk(&path, depth + 1, visit);
            }
        } else if kind.is_file() && entry.metadata().map(|m| m.len() <= MAX_FILE_BYTES).unwrap_or(false) {
            visit(&path);
        }
    }
}

fn dimensions(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()
}

fn load(path: &Path) -> Option<DynamicImage> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    image::load_from_memory(&bytes).ok()
}

/// The as-is icon, and — when it sits on a solid colour — the cut-out one.
fn variants(img: &DynamicImage, label: &str) -> Vec<IconCandidate> {
    let rgba = img.to_rgba8();
    let mut out = Vec::new();
    if let Some(url) = finish(rgba.clone()) {
        out.push(IconCandidate { label: label.to_string(), data_url: url, cutout: false });
    }
    if let Some(cut) = cut_out_background(&rgba) {
        if let Some(url) = finish(cut) {
            out.push(IconCandidate { label: format!("{label} · no background"), data_url: url, cutout: true });
        }
    }
    out
}

/// Make a solid background transparent, or `None` when there isn't one.
///
/// "Solid" = all four corners opaque and within a few levels of each other.
/// Pixels within `TOL` of that colour go fully transparent; the next `SOFT`
/// levels fade, so an anti-aliased edge stays smooth instead of haloed.
pub fn cut_out_background(img: &RgbaImage) -> Option<RgbaImage> {
    const TOL: i32 = 48;
    const SOFT: i32 = 40;
    let (w, h) = img.dimensions();
    if w < 2 || h < 2 {
        return None;
    }
    let corners = [img.get_pixel(0, 0), img.get_pixel(w - 1, 0), img.get_pixel(0, h - 1), img.get_pixel(w - 1, h - 1)];
    let bg = *corners[0];
    let dist = |p: &Rgba<u8>, q: &Rgba<u8>| (0..3).map(|i| (p[i] as i32 - q[i] as i32).abs()).max().unwrap_or(0);
    if bg[3] != 255 || corners.iter().any(|c| c[3] != 255 || dist(c, &bg) >= 12) {
        return None;
    }
    let mut out = img.clone();
    for p in out.pixels_mut() {
        let d = dist(p, &bg);
        if d < TOL {
            p[3] = 0;
        } else if d < TOL + SOFT {
            p[3] = (p[3] as i32 * (d - TOL) / SOFT) as u8;
        }
    }
    Some(out)
}

/// Trim, square, scale, encode. `None` for an image with nothing visible left.
fn finish(img: RgbaImage) -> Option<String> {
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 16 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 || y1 < y0 {
        return None;
    }
    let cropped = image::imageops::crop_imm(&img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
    let side = cropped.width().max(cropped.height());
    let mut square = RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut square,
        &cropped,
        i64::from((side - cropped.width()) / 2),
        i64::from((side - cropped.height()) / 2),
    );
    let scaled = image::imageops::resize(&square, SIZE, SIZE, FilterType::Lanczos3);
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(scaled).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_with_dot(bg: [u8; 4], dot: [u8; 4]) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(100, 100, Rgba(bg));
        for x in 40..60 {
            for y in 30..70 {
                img.put_pixel(x, y, Rgba(dot));
            }
        }
        img
    }

    #[test]
    fn a_solid_background_is_cut_out_and_the_logo_fills_the_icon() {
        let img = solid_with_dot([22, 28, 54, 255], [120, 220, 160, 255]);
        let cut = cut_out_background(&img).expect("four matching opaque corners");
        assert_eq!(cut.get_pixel(0, 0)[3], 0, "the navy is gone");
        assert_eq!(cut.get_pixel(50, 50)[3], 255, "the logo is untouched");
    }

    #[test]
    fn a_transparent_or_busy_background_is_left_alone() {
        let transparent = solid_with_dot([0, 0, 0, 0], [255, 0, 0, 255]);
        assert!(cut_out_background(&transparent).is_none());
        let mut busy = solid_with_dot([255, 255, 255, 255], [0, 0, 0, 255]);
        busy.put_pixel(99, 99, Rgba([0, 0, 0, 255]));
        assert!(cut_out_background(&busy).is_none(), "corners disagree — not a background");
    }

    #[test]
    fn the_output_is_a_small_square_png_the_row_will_accept() {
        let img = DynamicImage::ImageRgba8(solid_with_dot([0, 0, 0, 0], [255, 0, 0, 255]));
        let v = variants(&img, "x.png");
        assert_eq!(v.len(), 1, "no background to cut out");
        let b64 = v[0].data_url.strip_prefix("data:image/png;base64,").unwrap();
        let png = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let out = image::load_from_memory(&png).unwrap();
        assert_eq!((out.width(), out.height()), (SIZE, SIZE));
        assert!(png.len() <= cxtasks_core::db::appearance::MAX_ICON_BYTES);
        // Trimmed: the 20×40 red bar fills the height instead of sitting in a
        // 100px canvas.
        assert!(out.to_rgba8().get_pixel(SIZE / 2, 1)[3] > 0);
    }

    #[test]
    fn names_that_are_icons_score_and_the_rest_do_not() {
        for yes in [
            "public/apple-touch-icon.png",
            "src-tauri/icons/icon.png",
            "web/src/app/favicon.ico",
            "public/branding/logo-icon-dark.png",
            "App/Assets.xcassets/AppIcon.appiconset/1024.png",
        ] {
            assert!(name_score(Path::new(yes)).is_some(), "{yes}");
        }
        for no in ["logo-context.png", "public/hero.png", "icon.svg", "favicon.txt"] {
            assert!(name_score(Path::new(no)).is_none(), "{no}");
        }
    }
}
