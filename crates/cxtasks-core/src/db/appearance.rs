//! How a project, area or tag is DRAWN in the sidebar: its icon and colour.
//!
//! Two columns on each table (schema v29 added `icon`; `color` has been there
//! since v3/v4 and nothing wrote it until now). Both are whole-row synced, so
//! whatever lands here reaches the NAS and the phone with no extra code — which
//! is exactly why every value is validated HERE, on write: a row that every
//! device renders is not a place to let arbitrary text in.
//!
//! * `color` is a hue NAME (`blue`, `sand`, …), never a hex, so one stored
//!   choice means the right thing in both themes — `src/lib/hues.ts` maps it.
//! * `icon` is `lucide:<name>` for a built-in glyph, or a PNG as a
//!   `data:image/png;base64,` URI — an app's own icon from its repo, already
//!   trimmed and scaled to 64px by the app (a few KB). Anything else is refused:
//!   no SVG (a document, not an image), no other `data:` type, no URL (a fetch
//!   from every device that renders the row), and nothing past [`MAX_ICON_BYTES`].
//!
//! `None` means "the default": the kind's own glyph, and the automatic colour.

use crate::error::AppError;
use base64::Engine;
use rusqlite::{params, Connection};

/// The hue names `src/lib/hues.ts` knows. Kept in step by hand; a name missing
/// here is refused on write, and one missing there falls back to the automatic
/// colour, so a drift fails safe in both directions.
pub const HUES: &[&str] = &[
    "blue", "red", "green", "orange", "amber", "purple", "teal", "pink", "sand", "grey",
];

/// Decoded PNG size cap. A 64px icon is 2–8 KB; 48 KB leaves room for a busy
/// one while keeping the row — which rides every sidebar load and every sync —
/// small.
pub const MAX_ICON_BYTES: usize = 48 * 1024;

const PNG_PREFIX: &str = "data:image/png;base64,";
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

fn validate_color(color: Option<&str>) -> Result<Option<String>, AppError> {
    match color.map(str::trim).filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) if HUES.contains(&c) => Ok(Some(c.to_string())),
        Some(c) => Err(AppError::Validation(format!(
            "'{c}' is not a colour — use one of {}",
            HUES.join(", ")
        ))),
    }
}

fn validate_icon(icon: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(icon) = icon.map(str::trim).filter(|i| !i.is_empty()) else {
        return Ok(None);
    };
    if let Some(name) = icon.strip_prefix("lucide:") {
        let ok = !name.is_empty()
            && name.len() <= 48
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        return if ok {
            Ok(Some(icon.to_string()))
        } else {
            Err(AppError::Validation(format!("'{name}' is not an icon name")))
        };
    }
    if let Some(b64) = icon.strip_prefix(PNG_PREFIX) {
        // Checked BEFORE decoding, so a huge string costs nothing to refuse.
        if b64.len() > MAX_ICON_BYTES.div_ceil(3) * 4 {
            return Err(AppError::Validation(format!(
                "icon image is over {} KB — it should be a small, scaled icon",
                MAX_ICON_BYTES / 1024
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| AppError::Validation("icon image is not valid base64".into()))?;
        if !bytes.starts_with(PNG_MAGIC) {
            return Err(AppError::Validation("icon image must be a PNG".into()));
        }
        return Ok(Some(icon.to_string()));
    }
    Err(AppError::Validation(
        "an icon is `lucide:<name>` or a `data:image/png;base64,` image".into(),
    ))
}

/// Set a project's or area's icon and colour. Both are written together — the
/// picker always sends the pair it shows — and `None` clears either back to the
/// default.
pub fn set_project(
    conn: &Connection,
    name: &str,
    color: Option<&str>,
    icon: Option<&str>,
) -> Result<(), AppError> {
    let (color, icon) = (validate_color(color)?, validate_icon(icon)?);
    let n = conn.execute(
        "UPDATE projects SET color = ?1, icon = ?2 WHERE name = ?3 COLLATE NOCASE",
        params![color, icon, name.trim()],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("project '{}' not found", name.trim())));
    }
    Ok(())
}

/// Set a tag's icon and colour. Same contract as [`set_project`].
pub fn set_tag(
    conn: &Connection,
    name: &str,
    color: Option<&str>,
    icon: Option<&str>,
) -> Result<(), AppError> {
    let (color, icon) = (validate_color(color)?, validate_icon(icon)?);
    let n = conn.execute(
        "UPDATE tags SET color = ?1, icon = ?2 WHERE name = ?3 COLLATE NOCASE",
        params![color, icon, name.trim()],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("tag '{}' not found", name.trim())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 1×1 PNG, the smallest thing that passes.
    const TINY_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn accepts_what_the_picker_sends() {
        assert_eq!(validate_color(Some("blue")).unwrap().as_deref(), Some("blue"));
        assert_eq!(validate_color(None).unwrap(), None);
        assert_eq!(validate_color(Some("")).unwrap(), None, "empty clears, like None");
        assert_eq!(validate_icon(Some("lucide:building-2")).unwrap().as_deref(), Some("lucide:building-2"));
        assert!(validate_icon(Some(TINY_PNG)).unwrap().is_some());
        assert_eq!(validate_icon(Some("  ")).unwrap(), None);
    }

    #[test]
    fn refuses_anything_a_renderer_should_not_be_handed() {
        for bad in [
            "#ff0000",
            "rgb(0 0 0)",
            "blue; background:url(x)",
        ] {
            assert!(validate_color(Some(bad)).is_err(), "{bad}");
        }
        let svg = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode("<svg onload=alert(1)/>")
        );
        let not_png = format!(
            "{PNG_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(b"GIF89a....")
        );
        let huge = format!("{PNG_PREFIX}{}", "A".repeat(MAX_ICON_BYTES * 2));
        for bad in [
            "lucide:",
            "lucide:Rocket",
            "lucide:../../x",
            "https://example.com/icon.png",
            "rocket",
            svg.as_str(),
            not_png.as_str(),
            huge.as_str(),
            "data:image/png;base64,***",
        ] {
            assert!(validate_icon(Some(bad)).is_err(), "{}", &bad[..bad.len().min(60)]);
        }
    }
}
