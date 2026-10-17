//! Whether a piece of agent prose will READ well in the app — what the pane
//! will drop, and what will arrive as a wall.
//!
//! Notes, comments and run results render in the app through one markdown
//! surface that mounts inline HTML and SVG through a sanitize allowlist
//! (`src/lib/markdownSchema.ts`, gotcha #28). Whatever is not on that list
//! vanishes from the pane without a trace, and the author never finds out: an
//! agent writes `Vec<String>` outside backticks, the parser reads `<String>` as
//! a tag, and the note says "Vec". The only moment anyone can fix that is at
//! write time, and nearly every write arrives through the MCP — so the MCP's
//! write tools run [`check`] and say, in their reply, what will not show.
//!
//! It REPORTS and never rewrites. The body is stored exactly as sent: a check
//! that "fixed" text would be a second renderer with opinions. What the reply
//! tells the agent to do about it depends on the path ([`Channel`]), because
//! only notes can be rewritten — a comment or a completion result cannot.
//!
//! **The allowlist is shared, not copied.** `markdown_allowlist.json` holds
//! every tag and every attribute, read here and imported by the frontend
//! schema, so the warning and the pane cannot disagree about what renders.
//! The one thing the JSON cannot say is a value constraint (`input` may only be
//! a checkbox); the TypeScript keeps those, and this check does not model them.
//!
//! The scan is deliberately a scan, not a parser. Code is excluded the way the
//! renderer excludes it (fenced blocks and inline code spans are literal
//! text), and anything that is not unambiguously a tag — an autolink, an
//! email, `a < b` — is left alone. A false negative costs one silent drop, the
//! status quo; a false positive teaches agents to ignore the check, so the
//! bias is towards saying nothing.
//!
//! [`audit`] runs the same check over a week of writes, for the weekly
//! `script:prose-audit` tripwire (`cxtasks-runner --prose-audit`): the check
//! only helps if agents act on it, and that is measured rather than assumed.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// A paragraph longer than this with no structure is a wall. Two or three
/// sentences run 250–400 characters; 600 leaves room for one long one and
/// still catches the 900-character single block that prompted this.
pub const WALL_CHARS: usize = 600;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Allowlist {
    html: Vec<String>,
    svg: Vec<String>,
    attributes: HashMap<String, Vec<String>>,
    svg_attributes: Vec<String>,
}

struct Allowed {
    tags: HashSet<String>,
    svg_tags: HashSet<String>,
    /// Per tag, normalized attribute names; `*` applies to every tag.
    attributes: HashMap<String, HashSet<String>>,
    svg_attributes: HashSet<String>,
}

impl Allowed {
    fn attribute(&self, tag: &str, attr: &str) -> bool {
        let a = normalize_attr(attr);
        let on = |key: &str| self.attributes.get(key).is_some_and(|s| s.contains(&a));
        on("*") || on(tag) || (self.svg_tags.contains(tag) && self.svg_attributes.contains(&a))
    }
}

/// An HTML attribute as written (`stroke-width`, `xlink:href`, `class`) and
/// the hast property name the JSON uses (`strokeWidth`, `xLinkHref`,
/// `className`) compare equal once lower-cased with `-` and `:` removed. The
/// two exceptions `property-information` makes are the only ones that matter
/// in prose: `class` and `for`.
fn normalize_attr(name: &str) -> String {
    let lower: String = name
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match lower.as_str() {
        "class" => "classname".into(),
        "for" => "htmlfor".into(),
        _ => lower,
    }
}

/// HTML parsing lower-cases tag names, so `<Text>` and `<text>` are the same
/// element and compare the same here.
fn allowed() -> &'static Allowed {
    static SET: OnceLock<Allowed> = OnceLock::new();
    SET.get_or_init(|| {
        let list: Allowlist = serde_json::from_str(include_str!("markdown_allowlist.json"))
            .expect("markdown_allowlist.json matches the Allowlist shape");
        let lower = |v: Vec<String>| v.into_iter().map(|t| t.to_ascii_lowercase()).collect();
        let svg_tags: HashSet<String> = lower(list.svg.clone());
        Allowed {
            tags: list.html.into_iter().chain(list.svg).map(|t| t.to_ascii_lowercase()).collect(),
            svg_tags,
            attributes: list
                .attributes
                .into_iter()
                .map(|(tag, names)| {
                    (tag.to_ascii_lowercase(), names.iter().map(|n| normalize_attr(n)).collect())
                })
                .collect(),
            svg_attributes: list.svg_attributes.iter().map(|n| normalize_attr(n)).collect(),
        }
    })
}

/// Where a body was written, which decides what the agent can do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// A task's notes — the one channel that can be rewritten in place.
    Notes,
    /// A comment. Nothing edits a comment after it is posted.
    Comment,
    /// `complete_task`'s result, stored on a run row. Not editable either.
    Result,
    /// `file_followup` from a background run, which holds no `update_task`.
    Followup,
}

impl Channel {
    fn remedy(self) -> &'static str {
        match self {
            Channel::Notes => "Rewrite the notes with update_task(body) if it matters.",
            Channel::Comment => {
                "A comment can't be edited; post a corrected one with comment_task if it matters."
            }
            Channel::Result => {
                "A completion result can't be edited; add the corrected text with comment_task \
                 if it matters."
            }
            Channel::Followup => {
                "This run can't edit the follow-up; name the correction in your final message \
                 so the reviewer can fix it."
            }
        }
    }
}

/// What a body will lose, or how it will read, on its way to the pane.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    /// Tag names outside the allowlist, as written, first occurrence only.
    pub dropped_tags: Vec<String>,
    /// `(attribute, tag)` for attributes the allowlist strips from an allowed
    /// tag, first occurrence of each attribute only. Handlers are separate.
    pub dropped_attrs: Vec<(String, String)>,
    /// `on*` attribute names, first occurrence only.
    pub handlers: Vec<String>,
    /// `href` / `src` values using `javascript:`, `vbscript:` or `data:`.
    pub unsafe_urls: usize,
    /// Character counts of unstructured paragraphs over [`WALL_CHARS`].
    pub walls: Vec<usize>,
}

impl RenderReport {
    pub fn is_clean(&self) -> bool {
        self.dropped_tags.is_empty()
            && self.dropped_attrs.is_empty()
            && self.handlers.is_empty()
            && self.unsafe_urls == 0
            && self.walls.is_empty()
    }

    /// The issues as short clauses, without a remedy — shared by the tool
    /// reply and the audit's evidence lines.
    pub fn issues(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.dropped_tags.is_empty() {
            let list = self
                .dropped_tags
                .iter()
                .map(|t| format!("<{t}>"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "the app won't show {list} (code, paths, placeholders and generics belong in \
                 backticks; outside them `Vec<String>` renders as \"Vec\")"
            ));
        }
        if !self.dropped_attrs.is_empty() {
            let list = self
                .dropped_attrs
                .iter()
                .map(|(a, t)| format!("{a} on <{t}>"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("attributes it strips: {list}"));
        }
        if !self.handlers.is_empty() {
            parts.push(format!("event handlers stripped: {}", self.handlers.join(", ")));
        }
        if self.unsafe_urls > 0 {
            parts.push(format!("{} javascript:/data: link(s) removed", self.unsafe_urls));
        }
        if !self.walls.is_empty() {
            let lens = self.walls.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
            parts.push(format!(
                "{} paragraph(s) run {lens} characters unbroken (split into two-to-three-sentence \
                 paragraphs, bullets or a table)",
                self.walls.len()
            ));
        }
        parts
    }

    /// One paragraph for a tool reply, or `None` when everything reads well.
    pub fn message(&self, channel: Channel) -> Option<String> {
        if self.is_clean() {
            return None;
        }
        Some(format!(
            "RENDER CHECK (saved exactly as sent): {}. {}",
            self.issues().join("; "),
            channel.remedy()
        ))
    }
}

/// Convenience for the tool replies: `"\n\n" + message`, or nothing.
pub fn render_note(md: &str, channel: Channel) -> String {
    check(md)
        .message(channel)
        .map(|m| format!("\n\n{m}\n"))
        .unwrap_or_default()
}

/// Scan `md` for what the renderer will drop and what will read as a wall.
pub fn check(md: &str) -> RenderReport {
    let text = outside_code(md);
    let b = text.as_bytes();
    let list = allowed();
    let mut report = RenderReport {
        walls: walls(&text),
        ..Default::default()
    };
    let mut seen_tags = HashSet::new();
    let mut seen_attrs = HashSet::new();
    let mut seen_handlers = HashSet::new();

    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        // A comment is dropped whole and silently, which is what its author
        // meant — nothing to report.
        if text[j..].starts_with("!--") {
            i = text[j..].find("-->").map_or(b.len(), |k| j + k + 3);
            continue;
        }
        let closing = b.get(j) == Some(&b'/');
        if closing {
            j += 1;
        }
        if !b.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        let name_start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        // A tag name ends at whitespace, `>` or `/`. Anything else — the `:`
        // of `<https://…>`, the `@` of `<me@x.com>` — is not a tag.
        if !matches!(b.get(j), Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')) {
            i += 1;
            continue;
        }
        let Some(end) = tag_end(b, j) else {
            i += 1;
            continue;
        };
        let name = &text[name_start..j];
        let lower = name.to_ascii_lowercase();
        let tag_ok = list.tags.contains(&lower);
        if !tag_ok && seen_tags.insert(lower.clone()) {
            report.dropped_tags.push(name.to_string());
        }
        if !closing {
            for (attr, value) in attributes(&text[j..end]) {
                let attr_lower = attr.to_ascii_lowercase();
                let handler = attr_lower.len() > 2 && attr_lower.starts_with("on");
                if handler {
                    if seen_handlers.insert(attr_lower.clone()) {
                        report.handlers.push(attr_lower.clone());
                    }
                } else if tag_ok
                    && !list.attribute(&lower, attr)
                    && seen_attrs.insert(attr_lower.clone())
                {
                    // A dropped TAG already says everything on it goes, so
                    // attributes are only reported on tags that survive.
                    report.dropped_attrs.push((attr.to_string(), name.to_string()));
                }
                if matches!(attr_lower.as_str(), "href" | "src" | "xlink:href" | "action") {
                    let v = value.trim().to_ascii_lowercase();
                    if v.starts_with("javascript:")
                        || v.starts_with("vbscript:")
                        || v.starts_with("data:")
                    {
                        report.unsafe_urls += 1;
                    }
                }
            }
        }
        i = end + 1;
    }
    report
}

/// Unstructured paragraphs over [`WALL_CHARS`], by character count. Works on
/// text with code already blanked, so a long fenced block is never a wall.
fn walls(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut flush = |para: &mut Vec<&str>| {
        if let Some(first) = para.first() {
            if !structured(first) {
                let len = para.iter().map(|l| l.trim().chars().count()).sum::<usize>()
                    + para.len().saturating_sub(1);
                if len > WALL_CHARS {
                    out.push(len);
                }
            }
        }
        para.clear();
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut para);
        } else {
            para.push(line);
        }
    }
    flush(&mut para);
    out
}

/// Does this line open something other than a plain paragraph — a heading,
/// list, quote, table, HTML block, rule or indented code? Those are the
/// author's structure, and a long bullet is not a wall.
fn structured(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() >= 4 {
        return true;
    }
    if t.starts_with(['#', '>', '|', '<']) || t.starts_with("---") || t.starts_with("***") {
        return true;
    }
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// Index of the `>` closing a tag whose name ended at `from`, skipping any `>`
/// inside a quoted attribute value. `None` when the tag never closes, or a
/// blank line intervenes — the renderer would not read that as a tag either.
fn tag_end(b: &[u8], from: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let mut k = from;
    while k < b.len() {
        let c = b[k];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => return Some(k),
            None if c == b'\n' && b.get(k + 1) == Some(&b'\n') => return None,
            None => {}
        }
        k += 1;
    }
    None
}

/// `(name, value)` pairs from the inside of a start tag. A bare attribute has
/// an empty value.
fn attributes(inner: &str) -> Vec<(&str, &str)> {
    let b = inner.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        while k < b.len() && (b[k].is_ascii_whitespace() || b[k] == b'/') {
            k += 1;
        }
        let start = k;
        while k < b.len() && !b[k].is_ascii_whitespace() && !matches!(b[k], b'=' | b'/' | b'>') {
            k += 1;
        }
        if start == k {
            break;
        }
        let name = &inner[start..k];
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        let mut value = "";
        if b.get(k) == Some(&b'=') {
            k += 1;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            if let Some(&q) = b.get(k).filter(|c| **c == b'"' || **c == b'\'') {
                let vstart = k + 1;
                let vend = inner[vstart..].find(q as char).map_or(b.len(), |e| vstart + e);
                value = &inner[vstart..vend];
                k = (vend + 1).min(b.len());
            } else {
                let vstart = k;
                while k < b.len() && !b[k].is_ascii_whitespace() && b[k] != b'>' {
                    k += 1;
                }
                value = &inner[vstart..k];
            }
        }
        out.push((name, value));
    }
    out
}

/// `md` with every fenced block and inline code span blanked out, so markup
/// that renders as literal code is never mistaken for markup that renders.
/// Line structure is kept, so a blank line stays a blank line for `tag_end`
/// and a fence still separates the paragraphs around it for `walls`.
fn outside_code(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut fence: Option<(u8, usize)> = None;
    for line in md.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() <= 3 {
            if let Some((ch, n, rest)) = fence_marker(trimmed) {
                match fence {
                    None => {
                        fence = Some((ch, n));
                        out.push('\n');
                        continue;
                    }
                    Some((open_ch, open_n))
                        if ch == open_ch && n >= open_n && rest.trim().is_empty() =>
                    {
                        fence = None;
                        out.push('\n');
                        continue;
                    }
                    Some(_) => {}
                }
            }
        }
        if fence.is_some() {
            out.push('\n');
            continue;
        }
        out.push_str(&strip_code_spans(line));
    }
    out
}

/// `(char, run length, rest of line)` when `line` opens or closes a fence.
fn fence_marker(line: &str) -> Option<(u8, usize, &str)> {
    let b = line.as_bytes();
    let ch = *b.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let n = b.iter().take_while(|c| **c == ch).count();
    (n >= 3).then(|| (ch, n, &line[n..]))
}

/// One line with each `` `…` `` span replaced by a space. An opening run with
/// no closing run of the same length is literal backticks, as in CommonMark.
fn strip_code_spans(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let (mut i, mut last) = (0, 0);
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i] == b'`' {
            i += 1;
        }
        let n = i - start;
        let mut j = i;
        let mut closed = None;
        while j < b.len() {
            if b[j] == b'`' {
                let s = j;
                while j < b.len() && b[j] == b'`' {
                    j += 1;
                }
                if j - s == n {
                    closed = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        if let Some(end) = closed {
            out.push_str(&line[last..start]);
            out.push(' ');
            last = end;
            i = end;
        }
    }
    out.push_str(&line[last..]);
    out
}

// ─── The weekly audit ─────────────────────────────────────────────────────

/// A write that failed the check, for the audit's evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct AuditItem {
    /// `T113 notes`, `T113 comment`, `T113 result`.
    pub label: String,
    pub issues: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Audit {
    pub days: i64,
    pub total: usize,
    pub flagged: Vec<AuditItem>,
}

/// The audit trips when at least this many writes in the window fail…
pub const AUDIT_MIN_FLAGGED: usize = 3;
/// …and they are at least this share of the window's writes, in percent. Both,
/// so one bad note in a quiet week and a steady trickle in a busy one are
/// both left alone.
pub const AUDIT_TRIP_PERCENT: usize = 15;

impl Audit {
    pub fn tripped(&self) -> bool {
        self.flagged.len() >= AUDIT_MIN_FLAGGED
            && self.flagged.len() * 100 >= self.total * AUDIT_TRIP_PERCENT
    }

    /// The evidence a trip carries onto the task, most useful line first.
    pub fn evidence(&self) -> String {
        let pct = if self.total == 0 { 0 } else { self.flagged.len() * 100 / self.total };
        let mut s = format!(
            "{} of {} agent writes in the last {} days would lose text or read as a wall ({pct}%).",
            self.flagged.len(),
            self.total,
            self.days
        );
        for item in self.flagged.iter().take(12) {
            s.push_str(&format!("\n- {}: {}", item.label, item.issues.join("; ")));
        }
        if self.flagged.len() > 12 {
            s.push_str(&format!("\n- … and {} more", self.flagged.len() - 12));
        }
        s
    }
}

/// Every note, agent comment and run result written in the last `days` days,
/// scored with [`check`]. Notes count from the task's creation: a body edited
/// later is not dated on its own, and dating it by `updated_at` would re-score
/// an old note every time its status moved.
pub fn audit(conn: &rusqlite::Connection, days: i64) -> rusqlite::Result<Audit> {
    let window = format!("-{days} days");
    let label = "COALESCE('T' || t.short_id, t.id)";
    let queries = [
        (
            "notes",
            format!(
                "SELECT {label}, t.body FROM tasks t \
                 WHERE t.deleted_at IS NULL AND TRIM(COALESCE(t.body, '')) != '' \
                 AND datetime(t.created_at) >= datetime('now', ?1)"
            ),
        ),
        (
            "comment",
            format!(
                "SELECT {label}, c.body FROM task_comments c JOIN tasks t ON t.id = c.task_id \
                 WHERE c.author IN ({}) AND datetime(c.created_at) >= datetime('now', ?1)",
                crate::db::comments::AGENT_AUTHORS
                    .iter()
                    .map(|a| format!("'{a}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        (
            "result",
            format!(
                "SELECT {label}, r.result_text FROM task_runs r JOIN tasks t ON t.id = r.task_id \
                 WHERE TRIM(COALESCE(r.result_text, '')) != '' AND r.finished_at IS NOT NULL \
                 AND COALESCE(r.intent, '') != 'check' \
                 AND datetime(r.finished_at) >= datetime('now', ?1)"
            ),
        ),
    ];
    let mut audit = Audit { days, ..Default::default() };
    for (kind, sql) in queries {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([&window], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (reference, body) = row?;
            audit.total += 1;
            let report = check(&body);
            if !report.is_clean() {
                audit.flagged.push(AuditItem {
                    label: format!("{reference} {kind}"),
                    issues: report.issues(),
                });
            }
        }
    }
    Ok(audit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dropped(md: &str) -> Vec<String> {
        check(md).dropped_tags
    }

    #[test]
    fn plain_markdown_is_clean() {
        let md = "## Heading\n\n- a list\n- [ ] a task\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote";
        assert!(check(md).is_clean());
        assert_eq!(check(md).message(Channel::Notes), None);
        assert_eq!(render_note(md, Channel::Notes), "");
    }

    /// The failure this exists for: a generic outside backticks is a tag to
    /// the parser, and the pane shows only "Vec".
    #[test]
    fn a_generic_outside_backticks_is_reported() {
        assert_eq!(dropped("returns Vec<String> now"), vec!["String"]);
        let msg = check("returns Vec<String> now").message(Channel::Notes).unwrap();
        assert!(msg.contains("<String>"), "{msg}");
        assert!(msg.contains("backticks"), "{msg}");
    }

    #[test]
    fn code_is_literal_and_never_reported() {
        let md = "use `Vec<String>` and `<img onerror=x>`\n\n```html\n<script>alert(1)</script>\n<foo>\n```\n\n~~~\n<bar>\n~~~\n\n``a `<baz>` b``";
        assert!(check(md).is_clean(), "{:?}", check(md));
    }

    #[test]
    fn autolinks_emails_comparisons_and_comments_are_not_tags() {
        let md = "see <https://example.com/x> or <me@example.com>; a < b > c; 3<4\n<!-- <script> -->";
        assert!(check(md).is_clean(), "{:?}", check(md));
    }

    /// The reason HTML renders at all: an Onyx-style diagram must pass clean,
    /// attributes included.
    #[test]
    fn an_onyx_style_diagram_is_clean() {
        let md = r#"<figure>
<svg viewBox="0 0 440 90" width="440" height="90" role="img" aria-label="flow" xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
<defs><marker id="a" markerWidth="9" markerHeight="9" refX="6" refY="3" orient="auto"><path d="M0,0 L6,3 L0,6 Z" fill="var(--clay)"/></marker></defs>
<rect x="6" y="28" width="96" height="34" rx="8" fill="var(--white)" stroke="var(--gray-300)" stroke-width="1.5" class="node" style="opacity:.9"/>
<text x="54" y="49" text-anchor="middle" dominant-baseline="middle" font-size="12">email</text>
<line x1="104" y1="45" x2="168" y2="45" marker-end="url(#a)" stroke-dasharray="4 2" stroke-miterlimit="4"/>
<clipPath id="c"><rect width="1" height="1"/></clipPath>
</svg>
<figcaption>flow</figcaption>
</figure>

<details open><summary>More</summary>

hidden <mark>bit</mark><br/> <img src="https://x.io/a.png" alt="a" width="20"> <time datetime="2026-09-22">today</time>

</details>"#;
        assert!(check(md).is_clean(), "{:?}", check(md));
    }

    #[test]
    fn executable_markup_is_reported_once_each() {
        let r = check(
            "<script>alert(1)</script>\n<style>p{}</style>\n<iframe src=\"https://x\"></iframe>\n<svg><foreignObject></foreignObject><use href=\"#a\"/></svg>",
        );
        assert_eq!(r.dropped_tags, vec!["script", "style", "iframe", "foreignObject", "use"]);
        assert!(r.dropped_attrs.is_empty(), "attrs on a dropped tag are not re-reported: {r:?}");
    }

    #[test]
    fn handlers_and_script_urls_are_reported_on_allowed_tags() {
        let r = check(
            "<div onclick=\"x()\" style=\"color:red\">a</div> <img src=\"data:image/png;base64,AA\" onerror='y'> <a href=' JavaScript:alert(1)'>go</a> <a href=\"https://ok\">ok</a>",
        );
        assert!(r.dropped_tags.is_empty(), "{r:?}");
        assert!(r.dropped_attrs.is_empty(), "{r:?}");
        assert_eq!(r.handlers, vec!["onclick", "onerror"]);
        assert_eq!(r.unsafe_urls, 2);
        let msg = r.message(Channel::Comment).unwrap();
        assert!(msg.contains("onclick, onerror") && msg.contains("2 javascript:/data:"), "{msg}");
    }

    /// Attributes come from the same JSON as tags, so a diagram losing one is
    /// named — and one the list allows is not.
    #[test]
    fn attributes_outside_the_shared_list_are_reported_per_tag() {
        let r = check(
            "<div data-x=\"1\" aria-hidden=\"true\" title=\"t\" class=\"k\">a</div>\n<svg><line stroke-width=\"2\" data-x=\"2\" filter=\"url(#f)\"/></svg>",
        );
        assert_eq!(
            r.dropped_attrs,
            vec![
                ("data-x".to_string(), "div".to_string()),
                ("aria-hidden".to_string(), "div".to_string()),
                ("filter".to_string(), "line".to_string()),
            ]
        );
        let msg = r.message(Channel::Notes).unwrap();
        assert!(msg.contains("data-x on <div>") && msg.contains("filter on <line>"), "{msg}");
    }

    #[test]
    fn a_quoted_angle_bracket_does_not_end_the_tag() {
        let r = check("<div title=\"a > b\" onclick=\"z\">x</div>");
        assert_eq!(r.handlers, vec!["onclick"]);
    }

    #[test]
    fn tag_names_compare_case_insensitively_and_report_as_written() {
        assert!(check("<DIV><Text>t</Text></DIV>").is_clean());
        assert_eq!(dropped("<Widget/> and <widget>"), vec!["Widget"]);
    }

    /// The original complaint: one 900-character paragraph.
    #[test]
    fn a_long_unstructured_paragraph_is_a_wall_and_structure_is_not() {
        let sentence = "This sentence is here to make the paragraph long enough. ";
        let wall = sentence.repeat(12);
        let r = check(&format!("## State\n\n{wall}"));
        assert_eq!(r.walls.len(), 1, "{r:?}");
        assert!(r.walls[0] > WALL_CHARS);
        assert!(r.message(Channel::Notes).unwrap().contains("unbroken"));

        // The same words as bullets, a long table row, a quote, an indented
        // block or a fence are the author's structure, not a wall.
        for md in [
            format!("- {wall}"),
            format!("1. {wall}"),
            format!("> {wall}"),
            format!("| {wall} |"),
            format!("    {wall}"),
            format!("```\n{wall}\n```"),
            format!("<div>{wall}</div>"),
        ] {
            assert!(check(&md).walls.is_empty(), "{md:.40}");
        }
        // Short paragraphs are fine however many there are.
        let paras = vec![sentence.repeat(3); 6].join("\n\n");
        assert!(check(&paras).is_clean(), "{:?}", check(&paras));
    }

    /// Only notes can be rewritten, so the remedy depends on where it landed.
    #[test]
    fn the_remedy_names_a_fix_that_exists_on_that_path() {
        let r = check("x <Widget> y");
        let say = |c| r.message(c).unwrap();
        assert!(say(Channel::Notes).contains("update_task"));
        assert!(!say(Channel::Comment).contains("update_task"));
        assert!(say(Channel::Comment).contains("can't be edited"));
        assert!(!say(Channel::Result).contains("update_task"));
        assert!(say(Channel::Followup).contains("final message"));
    }

    #[test]
    fn attribute_names_normalize_across_html_and_hast_spellings() {
        for (html, hast) in [
            ("stroke-width", "strokeWidth"),
            ("xlink:href", "xLinkHref"),
            ("xmlns:xlink", "xmlnsXLink"),
            ("aria-label", "ariaLabel"),
            ("viewBox", "viewBox"),
            ("class", "className"),
        ] {
            assert_eq!(normalize_attr(html), normalize_attr(hast), "{html} vs {hast}");
        }
    }

    #[test]
    fn the_shared_list_carries_what_diagrams_and_markdown_need() {
        let a = allowed();
        for t in ["svg", "path", "marker", "clippath", "details", "summary", "figure", "input", "table", "mark"] {
            assert!(a.tags.contains(t), "{t} missing from markdown_allowlist.json");
        }
        for t in ["script", "style", "iframe", "foreignobject", "use", "object", "form"] {
            assert!(!a.tags.contains(t), "{t} must never be on the allowlist");
        }
        assert!(a.attribute("line", "marker-end"));
        assert!(a.attribute("div", "style") && a.attribute("div", "class"));
        assert!(!a.attribute("div", "onclick"), "a handler must never be allowlisted");
    }

    #[test]
    fn the_audit_scores_a_window_of_writes_and_trips_on_a_share() {
        let dir = std::env::temp_dir().join(format!("cx-prose-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("t.db")).unwrap();
        crate::db::schema::initialize(&conn).unwrap();

        let file = |title: &str, body: &str| {
            crate::db::tasks::create(
                &conn,
                &crate::db::tasks::NewTask {
                    title: title.into(),
                    body: Some(body.into()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let good = file("good", "## Fine\n\n- a\n- b");
        file("bad one", "returns Vec<String>");
        file("bad two", &"A long sentence that keeps going on. ".repeat(20));
        crate::db::comments::add(&conn, &good.id, crate::db::comments::AUTHOR_CLAUDE, "<script>x</script>")
            .unwrap();
        // Chris's own comments and machine evidence are not agent prose.
        crate::db::comments::add(&conn, &good.id, crate::db::comments::AUTHOR_ME, "Vec<String>").unwrap();

        let a = audit(&conn, 7).unwrap();
        assert_eq!(a.total, 4, "{a:?}");
        assert_eq!(a.flagged.len(), 3, "{a:?}");
        assert!(a.tripped());
        let ev = a.evidence();
        assert!(ev.starts_with("3 of 4 agent writes"), "{ev}");
        assert!(ev.contains("notes: the app won't show <String>"), "{ev}");
        assert!(ev.contains(" comment: "), "{ev}");

        std::fs::remove_dir_all(&dir).ok();

        // Both conditions must hold: three bad writes in a busy week (7.5%)
        // and one bad write in a quiet one (50%) are both left alone.
        let item = || AuditItem { label: "T1 notes".into(), issues: vec![] };
        let busy = Audit { days: 7, total: 40, flagged: (0..3).map(|_| item()).collect() };
        assert!(!busy.tripped());
        let quiet = Audit { days: 7, total: 2, flagged: vec![item()] };
        assert!(!quiet.tripped());
    }
}
