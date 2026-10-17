//! Attachment handling: preparing an image, copying it under the data dir,
//! and the four operations the UI and the MCP both need.
//!
//! Lives here rather than beside the Tauri commands because none of it needs
//! a UI framework — it is `std::fs` under `paths::attachments_dir()`. The
//! commands are now thin wrappers, and `api::dispatch` calls the same
//! functions, so there is one implementation rather than two.

use rusqlite::Connection;

use crate::db::{self, attachments::AttachmentRecord};
use crate::error::AppError;
use crate::paths;
use serde::Serialize;
use std::path::PathBuf;

/// The import cap, and — because it is `pub` — the upload cap the server
/// enforces too.
///
/// ONE number for both ends on purpose. A file larger than this could never
/// have been imported, so a server that accepted a larger body would be
/// accepting bytes no row could ever have described; and a server that capped
/// LOWER would refuse an attachment the app had already stored, which presents
/// as one image that silently never syncs.
pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

/// Is this a name that may be joined onto the attachments directory?
///
/// **The one definition**, shared by the HTTP routes and the sync client, for
/// the reason `paths.rs` exists at all (invariant 8): a second, slightly
/// laxer copy at one end is how a traversal gets through. The rule is an
/// allowlist rather than a "does it contain `..`" test — a denylist has to
/// anticipate every encoding, and `%2e%2e`, a backslash, a NUL and a bare `/`
/// are four different ways to write the same attack.
///
/// Everything this app mints is `<uuid>.<ext>`, so the allowlist costs nothing
/// real: ASCII alphanumerics, `.`, `-` and `_`, not starting with a dot, and
/// bounded in length.
pub fn is_safe_stored_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// Where a managed file lives — validated, then joined.
///
/// Every join of a `stored_name` onto [`paths::attachments_dir`] goes through
/// here, so the check cannot be skipped by a caller who did not know it
/// existed. A name that fails is a `Validation` error naming the name, because
/// the only way to reach it is a peer sending something this app did not mint.
pub fn managed_path(stored_name: &str) -> Result<PathBuf, AppError> {
    managed_path_in(&paths::attachments_dir(), stored_name)
}

/// [`managed_path`], against a store the caller names.
///
/// The directory is a parameter for the reason `paths::root_from` takes the
/// environment value instead of reading it: `app_data_dir()` is memoized in a
/// process-wide `OnceLock`, so a test that moved the root would race every
/// other test in the binary — intermittently, which is the worst kind. Passing
/// the store makes every byte-moving function testable against a temp
/// directory with no mutation at all, and it is honest besides: a module whose
/// job is copying bytes between two stores should say which store it means.
pub fn managed_path_in(dir: &std::path::Path, stored_name: &str) -> Result<PathBuf, AppError> {
    if !is_safe_stored_name(stored_name) {
        return Err(AppError::Validation(format!(
            "{stored_name:?} is not a managed attachment filename"
        )));
    }
    Ok(dir.join(stored_name))
}

/// Lowercase hex SHA-256.
pub fn digest_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// The same, streamed, so a 25 MB image is not held twice.
pub fn digest_of_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub id: String,
    pub task_id: String,
    pub comment_id: Option<String>,
    pub name: String,
    pub mime_type: String,
    /// Absolute managed path. The frontend turns this into a scoped `asset:`
    /// URL; source paths are never rendered and may disappear after import.
    pub path: String,
    pub created_at: String,
    /// The content address. `None` on a row written before v22 — see the v22
    /// migration for why it is never filled in afterwards.
    pub sha256: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommentWithAttachments {
    pub comment: db::comments::Comment,
    pub attachments: Vec<Attachment>,
}

struct PreparedImage {
    source: PathBuf,
    name: String,
    mime_type: &'static str,
    stored_name: String,
}

fn image_type(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "heic" | "heif" => Some("image/heic"),
        "tif" | "tiff" => Some("image/tiff"),
        _ => None,
    }
}

fn prepare(path: &str) -> Result<PreparedImage, AppError> {
    let source = PathBuf::from(path);
    let metadata = std::fs::metadata(&source)?;
    if !metadata.is_file() {
        return Err(AppError::Validation(format!(
            "{} is not a file",
            source.display()
        )));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(AppError::Validation(format!(
            "{} is larger than 25 MB",
            source.display()
        )));
    }
    let extension = source
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| AppError::Validation("image needs a file extension".into()))?;
    let mime_type = image_type(&extension).ok_or_else(|| {
        AppError::Validation(format!(".{extension} is not a supported image format"))
    })?;
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string();
    let stored_name = format!("{}.{}", uuid::Uuid::new_v4(), extension);
    Ok(PreparedImage {
        source,
        name,
        mime_type,
        stored_name,
    })
}

fn to_attachment(record: AttachmentRecord) -> Attachment {
    let path = paths::attachments_dir()
        .join(&record.stored_name)
        .to_string_lossy()
        .into_owned();
    Attachment {
        id: record.id,
        task_id: record.task_id,
        comment_id: record.comment_id,
        name: record.name,
        mime_type: record.mime_type,
        path,
        created_at: record.created_at,
        sha256: record.sha256,
    }
}

/// One copied file: where it landed, and what it hashes to.
struct Stored {
    path: PathBuf,
    sha256: String,
}

/// Copy each prepared image under the data dir and hash **what was written**.
///
/// The digest is taken from the DESTINATION rather than the source, and that is
/// not fussiness: the source is a file in Downloads that another process may be
/// rewriting, so hashing it and then copying it can record a digest for bytes
/// that are not the ones stored. Hashing the copy makes the recorded address
/// describe the file this row will forever point at.
fn copy_prepared(images: &[PreparedImage]) -> Result<Vec<Stored>, AppError> {
    let dir = paths::attachments_dir();
    std::fs::create_dir_all(&dir)?;
    let mut copied: Vec<Stored> = Vec::with_capacity(images.len());
    for image in images {
        let destination = dir.join(&image.stored_name);
        let outcome = std::fs::copy(&image.source, &destination)
            .map_err(AppError::from)
            .and_then(|_| digest_of_file(&destination).map_err(AppError::from));
        match outcome {
            Ok(sha256) => copied.push(Stored { path: destination, sha256 }),
            Err(error) => {
                let _ = std::fs::remove_file(&destination);
                remove_files(&copied.iter().map(|c| c.path.clone()).collect::<Vec<_>>());
                return Err(error);
            }
        }
    }
    Ok(copied)
}

fn remove_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("failed to remove attachment {}: {error}", path.display());
            }
        }
    }
}

/// Delete the managed copies behind these rows.
///
/// Names are vetted first even though every one of them was minted here: this
/// is a DELETE primitive taking a string that now arrives over the network (a
/// peer's post-image lands in `task_attachments` before anything here reads
/// it), and a traversal in a delete is the one that cannot be undone. A name
/// that fails is logged and skipped rather than joined.
pub fn remove_managed_records(records: &[AttachmentRecord]) {
    let paths = records
        .iter()
        .filter_map(|record| match managed_path(&record.stored_name) {
            Ok(path) => Some(path),
            Err(error) => {
                log::warn!("refusing to delete {}: {error}", record.stored_name);
                None
            }
        })
        .collect::<Vec<_>>();
    remove_files(&paths);
}

/// How long an untracked file is left alone on a node that syncs.
///
/// Long enough that no reconcile in flight can be outlived by it, short enough
/// that genuine garbage does not accumulate for a week. It is a *grace*, not a
/// retention: on a standalone host the wait is zero.
const PRUNE_GRACE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Heal files left by a crashed import or a cascade performed by another
/// binary that knows the v10 table but not the filesystem side.
///
/// # Why this is host-local in EFFECT, not just in where it runs
///
/// The rule used to be "a file no row names is garbage". That is true of a
/// standalone database and false the moment one syncs, because a row and its
/// bytes stop arriving together:
///
/// * On a **canonical** node, an upload lands as its own request. A row deleted
///   between the upload and the row's next post-image leaves bytes nobody has
///   claimed *yet*.
/// * On a **local** node, a pull applies the peer's whole database, and
///   `apply::delete_absent` removes rows the peer does not have. An attachment
///   created here in the window between another process's push and its pull is
///   deleted by that pull and re-created by the next sweep — during which its
///   file is untracked and, under the old rule, destroyed. The row comes back;
///   the image does not, and nothing reports it.
///
/// So on a synced host an untracked file is not evidence of anything until it
/// has been untracked for a while. [`PRUNE_GRACE`] is that while. With sync off
/// the behaviour is byte-identical to what it always was — which is the point:
/// the standalone Mac must not change because a feature it does not use exists.
///
/// Partial downloads are handled structurally rather than by the grace, since
/// they live in [`paths::attachments_incoming_dir`] and the walk below skips
/// anything that is not a file. They are swept on the same clock.
pub fn prune_orphaned_files(conn: &rusqlite::Connection) -> Result<usize, AppError> {
    prune_orphaned_files_in(&paths::attachments_dir(), conn)
}

/// [`prune_orphaned_files`], against a store the caller names. See
/// [`managed_path_in`] for why the directory is a parameter.
pub fn prune_orphaned_files_in(
    dir: &std::path::Path,
    conn: &rusqlite::Connection,
) -> Result<usize, AppError> {
    if !dir.exists() {
        return Ok(0);
    }
    // The rule follows THIS host's state, read from the database it is about.
    let grace = if crate::sync::state::enabled(conn) {
        Some(PRUNE_GRACE)
    } else {
        None
    };
    let tracked = db::attachments::stored_names(conn)?;
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // Skips `.incoming/`, and therefore every download in flight.
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if tracked.contains(&name) {
            continue;
        }
        if let Some(grace) = grace {
            if !older_than(&entry, grace) {
                continue;
            }
        }
        std::fs::remove_file(entry.path())?;
        removed += 1;
    }
    removed += sweep_incoming(&dir.join(paths::ATTACHMENTS_INCOMING), grace.unwrap_or(PRUNE_GRACE));
    Ok(removed)
}

/// Collect abandoned partial downloads. Always on a clock, whatever the host's
/// sync state: an in-flight download is in-flight on a standalone host too
/// (a node can be paused mid-transfer), and the file is worthless either way.
fn sweep_incoming(dir: &std::path::Path, grace: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if older_than(&entry, grace) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Has this file been sitting here longer than `grace`?
///
/// A clock this cannot read is treated as "too young to collect". Failing
/// toward keeping a file is the only safe direction: the cost of keeping
/// garbage is bytes, and the cost of deleting an image is the image.
fn older_than(entry: &std::fs::DirEntry, grace: std::time::Duration) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age >= grace)
}

/// Write bytes for a managed file, atomically, verifying them first.
///
/// Two properties, and the file layout is what buys both. The digest is checked
/// **before** anything lands, so a corrupted transfer never becomes a file at
/// all; and the write goes to [`paths::attachments_incoming_dir`] and is
/// RENAMED into place, so "the file exists" means "the file is complete".
/// Streaming straight to the final path would leave a truncated image that
/// every later check reads as present — permanently, because nothing would ever
/// look at it again.
///
/// `expected` is the row's `sha256`. `None` — a row written before v22 — is
/// accepted as served: an unverified image is a weaker guarantee, never a wrong
/// one, and refusing would make legacy attachments permanently unfetchable.
pub fn store_bytes(
    stored_name: &str,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<(), AppError> {
    store_bytes_in(&paths::attachments_dir(), stored_name, bytes, expected)
}

/// [`store_bytes`], against a store the caller names. See [`managed_path_in`].
pub fn store_bytes_in(
    dir: &std::path::Path,
    stored_name: &str,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<(), AppError> {
    let destination = managed_path_in(dir, stored_name)?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(AppError::Validation(format!(
            "{stored_name} is {} bytes, over the {MAX_IMAGE_BYTES}-byte limit",
            bytes.len()
        )));
    }
    if let Some(expected) = expected {
        let actual = digest_of(bytes);
        if actual != expected {
            return Err(AppError::Validation(format!(
                "{stored_name} hashes to {actual} but the row says {expected} —                  refusing to store bytes that are not the ones this row describes"
            )));
        }
    }
    let incoming = dir.join(paths::ATTACHMENTS_INCOMING);
    std::fs::create_dir_all(&incoming)?;
    let partial = incoming.join(format!("{}.part", uuid::Uuid::new_v4()));
    std::fs::write(&partial, bytes)?;
    if let Err(error) = std::fs::rename(&partial, &destination) {
        let _ = std::fs::remove_file(&partial);
        return Err(error.into());
    }
    Ok(())
}

/// Read a managed file's bytes.
pub fn read_bytes(stored_name: &str) -> Result<Vec<u8>, AppError> {
    read_bytes_in(&paths::attachments_dir(), stored_name)
}

/// [`read_bytes`], against a store the caller names. See [`managed_path_in`].
pub fn read_bytes_in(dir: &std::path::Path, stored_name: &str) -> Result<Vec<u8>, AppError> {
    Ok(std::fs::read(managed_path_in(dir, stored_name)?)?)
}

/// Do we hold correct bytes for this row?
///
/// **Correct**, not merely present. A file whose digest disagrees with its row
/// is not that attachment — it is a truncated download from a build that
/// predates [`store_bytes`], or a file somebody dropped in by hand — and
/// treating it as present is how a broken thumbnail becomes permanent. A row
/// with no digest (pre-v22) can only be judged on presence.
pub fn have_bytes_for(dir: &std::path::Path, record: &AttachmentRecord) -> bool {
    let Ok(path) = managed_path_in(dir, &record.stored_name) else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    match record.sha256.as_deref() {
        None => true,
        Some(expected) => digest_of_file(&path).map(|got| got == expected).unwrap_or(false),
    }
}

pub fn list_task_attachments(
    conn: &Connection,
    task_id: String,
) -> Result<Vec<Attachment>, AppError> {
    Ok(db::attachments::for_task(&conn, &task_id)?
        .into_iter()
        .map(to_attachment)
        .collect())
}

pub fn add_task_attachments(
    conn: &mut Connection,
    task_id: String,
    comment_id: Option<String>,
    source_paths: Vec<String>,
) -> Result<Vec<Attachment>, AppError> {
    if source_paths.is_empty() {
        return Err(AppError::Validation("choose at least one image".into()));
    }
    let images = source_paths
        .iter()
        .map(|path| prepare(path))
        .collect::<Result<Vec<_>, _>>()?;
    let copied = copy_prepared(&images)?;

    let result = (|| {
        let tx = conn.savepoint()?;
        let mut records = Vec::with_capacity(images.len());
        for (image, stored) in images.iter().zip(&copied) {
            records.push(db::attachments::add(
                &tx,
                &task_id,
                comment_id.as_deref(),
                &image.name,
                image.mime_type,
                &image.stored_name,
                Some(&stored.sha256),
            )?);
        }
        tx.commit()?;
        Ok::<_, AppError>(records.into_iter().map(to_attachment).collect())
    })();

    if result.is_err() {
        remove_files(&copied.iter().map(|c| c.path.clone()).collect::<Vec<_>>());
    }
    result
}

pub fn add_comment_with_attachments(
    conn: &mut Connection,
    task_id: String,
    body: String,
    source_paths: Vec<String>,
) -> Result<CommentWithAttachments, AppError> {
    if source_paths.is_empty() {
        return Err(AppError::Validation("choose at least one image".into()));
    }
    let images = source_paths
        .iter()
        .map(|path| prepare(path))
        .collect::<Result<Vec<_>, _>>()?;
    let copied = copy_prepared(&images)?;

    let result = (|| {
        let tx = conn.savepoint()?;
        let comment = db::comments::add_with_attachment(
            &tx,
            &task_id,
            db::comments::AUTHOR_ME,
            &body,
        )?;
        let mut records = Vec::with_capacity(images.len());
        for (image, stored) in images.iter().zip(&copied) {
            records.push(db::attachments::add(
                &tx,
                &task_id,
                Some(&comment.id),
                &image.name,
                image.mime_type,
                &image.stored_name,
                Some(&stored.sha256),
            )?);
        }
        tx.commit()?;
        Ok::<_, AppError>(CommentWithAttachments {
            comment,
            attachments: records.into_iter().map(to_attachment).collect(),
        })
    })();

    if result.is_err() {
        remove_files(&copied.iter().map(|c| c.path.clone()).collect::<Vec<_>>());
    }
    result
}

pub fn delete_task_attachment(
    conn: &Connection,
    id: String,
) -> Result<(), AppError> {
    let record = db::attachments::delete(conn, &id)?;
    let path = managed_path(&record.stored_name)?;
    if let Err(error) = std::fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            // The metadata is already gone, so report success to let the UI
            // remove the thumbnail. Startup pruning will retry this orphan.
            log::warn!("failed to remove attachment {}: {error}", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    /// A throwaway store. Never `paths::attachments_dir()` — that is the live
    /// Mac's, and a test must not write into it.
    fn temp_store() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cxtasks-att-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Backdate a file so a grace period can be tested without waiting an hour.
    ///
    /// The alternative is a `sleep`, which would make the suite take longer
    /// than the grace it is proving — the same reason the sweep tests move the
    /// watermark instead of waiting for the clock.
    fn backdate(path: &Path, by: Duration) {
        let when = SystemTime::now() - by;
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(when)).unwrap();
    }

    fn tracked(conn: &rusqlite::Connection, dir: &Path, content: &[u8]) -> String {
        let task = tasks::create(conn, &NewTask { title: "t".into(), ..Default::default() }).unwrap();
        let stored_name = format!("{}.png", uuid::Uuid::new_v4());
        db::attachments::add(
            conn,
            &task.id,
            None,
            "shot.png",
            "image/png",
            &stored_name,
            Some(&digest_of(content)),
        )
        .unwrap();
        std::fs::write(dir.join(&stored_name), content).unwrap();
        stored_name
    }

    // ── the name is the boundary ────────────────────────────────────────────

    /// Nothing that is not a plain filename may be joined onto the store.
    ///
    /// The list is the interesting part: each entry is a different way to write
    /// the same attack, which is why the check is an allowlist. A `contains("..")`
    /// test passes four of these.
    #[test]
    fn a_name_that_is_not_a_plain_filename_never_becomes_a_path() {
        for bad in [
            "../../../etc/passwd",
            "..",
            ".",
            "a/b.png",
            "a\\b.png",
            "/etc/passwd",
            ".hidden",
            "",
            "with space.png",
            "naïve.png",
        ] {
            assert!(!is_safe_stored_name(bad), "{bad:?} must be refused");
            assert!(managed_path(bad).is_err(), "{bad:?} must not resolve to a path");
        }
        // ...and the shape this app actually mints still works, or the check
        // passes by refusing everything.
        let real = "2f3a4b5c-0000-1111-2222-333344445555.png";
        assert!(is_safe_stored_name(real));
        assert!(managed_path(real).unwrap().ends_with(real));
    }

    // ── writing bytes ───────────────────────────────────────────────────────

    /// **Verification happens before anything lands.** A body that is not what
    /// the row describes must leave no file at all — not a bad one, and not a
    /// leftover partial either, because prune would keep the first and the
    /// second is litter.
    #[test]
    fn bytes_that_fail_verification_leave_nothing_behind() {
        let dir = temp_store();
        let name = "aaaaaaaa-1111-2222-3333-444444444444.png";

        let err = store_bytes_in(&dir, name, b"the wrong bytes", Some(&digest_of(b"the right bytes")))
            .unwrap_err()
            .to_string();

        assert!(err.contains("hashes to"), "the refusal must name both digests: {err}");
        assert!(!dir.join(name).exists(), "a rejected body must not become a file");
        let incoming = dir.join(paths::ATTACHMENTS_INCOMING);
        let leftovers = std::fs::read_dir(&incoming).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "and must not leave a partial behind");
    }

    /// A row with no content address (pre-v22) is still fetchable. An
    /// unverified image is a weaker guarantee, never a wrong one; refusing
    /// would make every legacy attachment permanently unfetchable.
    #[test]
    fn a_row_with_no_digest_accepts_what_it_is_served() {
        let dir = temp_store();
        let name = "bbbbbbbb-1111-2222-3333-444444444444.png";
        store_bytes_in(&dir, name, b"legacy", None).unwrap();
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), b"legacy");
    }

    /// `have_bytes_for` answers CORRECT, not merely present — a file that
    /// disagrees with its row is not that attachment.
    #[test]
    fn a_file_that_disagrees_with_its_row_does_not_count_as_held() {
        let dir = temp_store();
        let conn = test_conn();
        let name = tracked(&conn, &dir, b"the real image");
        let record = db::attachments::by_stored_name(&conn, &name).unwrap().unwrap();
        assert!(have_bytes_for(&dir, &record));

        std::fs::write(dir.join(&name), b"something else").unwrap();
        assert!(
            !have_bytes_for(&dir, &record),
            "a truncated or swapped file must not read as present, or the \
             broken thumbnail is permanent"
        );
    }

    // ── pruning ─────────────────────────────────────────────────────────────

    /// A tracked file is never touched, whatever the host's sync state.
    #[test]
    fn a_tracked_file_is_never_collected() {
        let dir = temp_store();
        let conn = test_conn();
        let name = tracked(&conn, &dir, b"keep me");
        backdate(&dir.join(&name), Duration::from_secs(60 * 60 * 24));

        assert_eq!(prune_orphaned_files_in(&dir, &conn).unwrap(), 0);
        assert!(dir.join(&name).exists());
    }

    /// **Standalone behaviour is unchanged**, and that is the point of the
    /// grace being conditional: a Mac that does not sync must not start
    /// keeping garbage because a feature it never uses exists.
    #[test]
    fn a_standalone_host_collects_an_orphan_immediately() {
        let dir = temp_store();
        let conn = test_conn();
        std::fs::write(dir.join("cccccccc-1111-2222-3333-444444444444.png"), b"orphan").unwrap();

        assert_eq!(prune_orphaned_files_in(&dir, &conn).unwrap(), 1);
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    }

    /// **The host-local rule.** On a synced host an untracked file may be
    /// bytes whose row this node has not pulled yet — a row created here
    /// between another process's push and its pull is deleted by that pull and
    /// re-created by the next sweep, and under the old rule its image was
    /// destroyed in between. So freshness buys it a stay.
    #[test]
    fn a_synced_host_leaves_a_fresh_orphan_alone() {
        let dir = temp_store();
        let conn = test_conn();
        crate::sync::state::set_enabled(&conn, true).unwrap();
        let name = "dddddddd-1111-2222-3333-444444444444.png";
        std::fs::write(dir.join(name), b"a row is on its way").unwrap();

        assert_eq!(prune_orphaned_files_in(&dir, &conn).unwrap(), 0);
        assert!(
            dir.join(name).exists(),
            "a file whose row may merely be in flight must survive"
        );
    }

    /// ...but a stay is not a reprieve. Once it has been untracked for longer
    /// than any reconcile could take, it is garbage on a synced host too.
    #[test]
    fn a_synced_host_still_collects_an_orphan_that_has_been_one_for_hours() {
        let dir = temp_store();
        let conn = test_conn();
        crate::sync::state::set_enabled(&conn, true).unwrap();
        let name = "eeeeeeee-1111-2222-3333-444444444444.png";
        std::fs::write(dir.join(name), b"nothing is coming for this").unwrap();
        backdate(&dir.join(name), PRUNE_GRACE + Duration::from_secs(60));

        assert_eq!(prune_orphaned_files_in(&dir, &conn).unwrap(), 1);
        assert!(!dir.join(name).exists());
    }

    /// **A download in flight is structurally invisible.** It lives in a
    /// subdirectory, and the walk skips anything that is not a file — so a
    /// second process starting up cannot delete a partial out from under the
    /// process writing it. Not a grace-period question: this must hold on a
    /// standalone host too, where the grace is zero.
    #[test]
    fn a_download_in_flight_is_not_collected_even_on_a_standalone_host() {
        let dir = temp_store();
        let conn = test_conn();
        let incoming = dir.join(paths::ATTACHMENTS_INCOMING);
        std::fs::create_dir_all(&incoming).unwrap();
        let partial = incoming.join("ffffffff-1111-2222-3333-444444444444.part");
        std::fs::write(&partial, b"half an image").unwrap();

        assert_eq!(prune_orphaned_files_in(&dir, &conn).unwrap(), 0);
        assert!(partial.exists(), "prune must not race a transfer");

        // And an abandoned one is still collected, on its own clock.
        backdate(&partial, PRUNE_GRACE + Duration::from_secs(60));
        assert_eq!(prune_orphaned_files_in(&dir, &conn).unwrap(), 1);
        assert!(!partial.exists());
    }
}
