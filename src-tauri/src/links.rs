//! `cxtasks://` links — the way something outside the app says "show me this
//! task" (the Alfred `t` workflow, a link pasted in a note).
//!
//! The scheme is registered by `CFBundleURLTypes` in `Info.plist`, so it exists
//! only in a real bundle installed where LaunchServices can see it — never in
//! `tauri dev`. AppKit delivers the URL as `RunEvent::Opened`, both to an app
//! that is already running and to one the link just launched.
//!
//! A link opens a task through [`crate::notify_macos::open_task`], the same
//! latch a notification click uses, so a cold launch lands on the task too.
//! The ref is handed on UNRESOLVED: the frontend's `reveal` looks it up with
//! `get_task`, which is `resolve_ref` and accepts a T-number as well as an id.

/// The task a `cxtasks://task/<ref>` link names, where `<ref>` is a T-number
/// (`T42`) or a task id.
///
/// `None` for anything else. The ref reaches the frontend and then a DB lookup
/// by parameter, so the character check is not an injection guard — it keeps a
/// malformed link from being parked in the latch and revealing nothing later.
pub fn task_ref(url: &str) -> Option<String> {
    let rest = url.strip_prefix("cxtasks://task/")?;
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let r = rest[..end].trim_end_matches('/');
    let valid = !r.is_empty()
        && r.len() <= 64
        && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    valid.then(|| r.to_string())
}

#[cfg(test)]
mod tests {
    use super::task_ref;

    #[test]
    fn accepts_a_t_number_and_an_id() {
        assert_eq!(task_ref("cxtasks://task/T42").as_deref(), Some("T42"));
        assert_eq!(
            task_ref("cxtasks://task/3f2a9c1e-0b4d-4e7a-9c1f-2d3e4f5a6b7c").as_deref(),
            Some("3f2a9c1e-0b4d-4e7a-9c1f-2d3e4f5a6b7c")
        );
    }

    #[test]
    fn ignores_a_trailing_slash_query_and_fragment() {
        assert_eq!(task_ref("cxtasks://task/T42/").as_deref(), Some("T42"));
        assert_eq!(task_ref("cxtasks://task/T42?from=alfred").as_deref(), Some("T42"));
        assert_eq!(task_ref("cxtasks://task/T42#notes").as_deref(), Some("T42"));
    }

    #[test]
    fn refuses_anything_that_is_not_a_task_link() {
        assert_eq!(task_ref("cxtasks://task/"), None);
        assert_eq!(task_ref("cxtasks://run/abc"), None);
        assert_eq!(task_ref("https://task/T42"), None);
        assert_eq!(task_ref("cxtasks://task/T42/extra"), None);
        assert_eq!(task_ref("cxtasks://task/T 42"), None);
        assert_eq!(task_ref(&format!("cxtasks://task/{}", "a".repeat(65))), None);
    }
}
