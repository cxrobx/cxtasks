//! Tauri wrappers over `cxtasks_core::attachments`.
//!
//! The substance moved to core — none of it needed a UI framework, and both
//! this layer and `api::dispatch` have to run it. What is left is the IPC
//! boundary: take the lock, call the one implementation.

use cxtasks_core::attachments::{self, Attachment, CommentWithAttachments};
use tauri::State;

use crate::error::AppError;
use crate::{AppState, LockExt};

#[tauri::command]
pub async fn list_task_attachments(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<Attachment>, AppError> {
    let conn = state.db.safe_lock();
    attachments::list_task_attachments(&conn, task_id)
}

#[tauri::command]
pub async fn add_task_attachments(
    state: State<'_, AppState>,
    task_id: String,
    comment_id: Option<String>,
    source_paths: Vec<String>,
) -> Result<Vec<Attachment>, AppError> {
    let mut conn = state.db.safe_lock();
    attachments::add_task_attachments(&mut conn, task_id, comment_id, source_paths)
}

#[tauri::command]
pub async fn add_comment_with_attachments(
    state: State<'_, AppState>,
    task_id: String,
    body: String,
    mut source_paths: Vec<String>,
    clipboard_images: Option<Vec<Vec<u8>>>,
) -> Result<CommentWithAttachments, AppError> {
    let staged = TemporaryImages::new(clipboard_images.unwrap_or_default())?;
    source_paths.extend(staged.paths());
    let mut conn = state.db.safe_lock();
    attachments::add_comment_with_attachments(&mut conn, task_id, body, source_paths)
}

#[tauri::command]
pub async fn delete_task_attachment(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let conn = state.db.safe_lock();
    attachments::delete_task_attachment(&conn, id)
}

/// Native clipboard access also works when the WebView does not expose image files.
#[tauri::command]
pub async fn paste_task_image(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    quiet: Option<bool>,
) -> Result<Vec<Attachment>, AppError> {
    let Some(png) = clipboard_image(app, quiet.unwrap_or(false)).await? else {
        return Ok(Vec::new());
    };
    let staged = TemporaryImages::new(vec![png])?;
    let mut conn = state.db.safe_lock();
    attachments::add_task_attachments(&mut conn, task_id, None, staged.paths())
}

/// Read now, but do not create a comment until the person posts their draft.
#[tauri::command]
pub async fn read_clipboard_image(app: tauri::AppHandle) -> Result<Vec<u8>, AppError> {
    clipboard_image(app, false).await?.ok_or_else(|| {
        AppError::Validation("No image in the clipboard. Copy an image and try again.".into())
    })
}

#[tauri::command]
pub async fn try_read_clipboard_image(app: tauri::AppHandle) -> Result<Option<Vec<u8>>, AppError> {
    clipboard_image(app, true).await
}

async fn clipboard_image(app: tauri::AppHandle, quiet: bool) -> Result<Option<Vec<u8>>, AppError> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    // NSPasteboard is main-thread only. A ⌘V has WebKit reading it on the main
    // thread at the same moment, and a background read crashed the app with
    // SIGSEGV in -[NSPasteboard _updateTypeCacheIfNeeded] (mutated while
    // enumerated). So READ on the main thread; only the PNG encode runs off it.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let read = handle
            .clipboard()
            .read_image()
            .map(|image| (image.width(), image.height(), image.rgba().to_vec()))
            .map_err(|e| e.to_string());
        let _ = tx.send(read);
    })
    .map_err(|e| AppError::General(e.to_string()))?;
    let read = rx
        .await
        .map_err(|_| AppError::General("Clipboard read was dropped".into()))?;
    let (width, height, rgba) = match read {
        Ok(image) => image,
        Err(_) if quiet => return Ok(None),
        Err(_) => {
            return Err(AppError::Validation(
                "No image in the clipboard. Copy an image and try again.".into(),
            ))
        }
    };
    tauri::async_runtime::spawn_blocking(move || {
        let image = image::RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| AppError::Validation("Invalid clipboard image".into()))?;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .map_err(|e| AppError::General(e.to_string()))?;
        let png = bytes.into_inner();
        validate_clipboard_size(&png)?;
        Ok(Some(png))
    })
    .await
    .map_err(|e| AppError::General(e.to_string()))?
}

fn validate_clipboard_size(bytes: &[u8]) -> Result<(), AppError> {
    if bytes.len() as u64 > attachments::MAX_IMAGE_BYTES {
        return Err(AppError::Validation(
            "Clipboard image is larger than 25 MB".into(),
        ));
    }
    Ok(())
}

/// Scoped staging: all temporary files are removed on success or failure.
struct TemporaryImages {
    directory: std::path::PathBuf,
    sources: Vec<String>,
}

impl TemporaryImages {
    fn new(images: Vec<Vec<u8>>) -> Result<Self, AppError> {
        let directory =
            std::env::temp_dir().join(format!("cxtasks-paste-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let mut staged = Self {
            directory,
            sources: Vec::new(),
        };
        for (index, bytes) in images.into_iter().enumerate() {
            validate_clipboard_size(&bytes)?;
            let source = staged
                .directory
                .join(format!("Clipboard image {}.png", index + 1));
            std::fs::write(&source, bytes)?;
            staged.sources.push(source.to_string_lossy().into_owned());
        }
        Ok(staged)
    }

    fn paths(&self) -> Vec<String> {
        self.sources.clone()
    }
}

impl Drop for TemporaryImages {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_clipboard_images_are_removed_after_import_or_failure() {
        let staged = TemporaryImages::new(vec![vec![1, 2, 3], vec![4, 5]]).unwrap();
        let directory = staged.directory.clone();
        let paths = staged.paths();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), vec![1, 2, 3]);
        assert_eq!(std::fs::read(&paths[1]).unwrap(), vec![4, 5]);
        drop(staged);
        assert!(!directory.exists());
    }

    #[test]
    fn clipboard_images_obey_the_importers_size_limit() {
        let oversized = vec![0; attachments::MAX_IMAGE_BYTES as usize + 1];
        assert!(validate_clipboard_size(&oversized)
            .unwrap_err()
            .to_string()
            .contains("25 MB"));
        assert!(TemporaryImages::new(vec![oversized]).is_err());
    }
}
