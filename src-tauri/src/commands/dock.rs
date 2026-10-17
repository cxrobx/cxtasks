use crate::error::AppError;

#[cfg(target_os = "macos")]
use crate::dock_macos::DockMenuSettings;

#[cfg(not(target_os = "macos"))]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockMenuSettings {
    show_today: bool,
    show_inbox: bool,
    show_quick_add: bool,
    max_tasks: u8,
}

#[tauri::command]
pub async fn get_dock_menu_settings() -> Result<DockMenuSettings, AppError> {
    #[cfg(target_os = "macos")]
    {
        Ok(crate::dock_macos::settings())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(DockMenuSettings {
            show_today: true,
            show_inbox: false,
            show_quick_add: true,
            max_tasks: 6,
        })
    }
}

#[tauri::command]
pub async fn save_dock_menu_settings(
    settings: DockMenuSettings,
) -> Result<DockMenuSettings, AppError> {
    #[cfg(target_os = "macos")]
    {
        crate::dock_macos::save_settings(settings)
            .map_err(|error| AppError::General(error.to_string()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(settings)
    }
}
