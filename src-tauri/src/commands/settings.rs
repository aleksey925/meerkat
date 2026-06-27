use crate::credentials;
use crate::models::Settings;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    let url = store
        .get("gitlab_url")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    let poll_interval = store
        .get("poll_interval")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "30".to_string());

    let show_drafts = store
        .get("show_drafts")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let desktop_notif = store
        .get("desktop_notif")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let sound_notif = store
        .get("sound_notif")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let has_token = credentials::get_token()?.is_some();
    let connected = has_token && store.get("user_id").is_some() && !url.is_empty();

    let token_display = if has_token {
        credentials::get_token()?.and_then(|t| if t.is_empty() { None } else { Some(t) })
    } else {
        None
    };

    Ok(Settings {
        url,
        token: token_display,
        poll_interval,
        show_drafts,
        desktop_notif,
        sound_notif,
        connected,
    })
}

#[tauri::command]
pub async fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    store.set("gitlab_url", serde_json::json!(settings.url));
    store.set("poll_interval", serde_json::json!(settings.poll_interval));
    store.set("show_drafts", serde_json::json!(settings.show_drafts));
    store.set("desktop_notif", serde_json::json!(settings.desktop_notif));
    store.set("sound_notif", serde_json::json!(settings.sound_notif));
    store.save().map_err(|e| format!("Save error: {e}"))?;

    // Token stored via OS keychain, not in the store file
    if let Some(ref token) = settings.token {
        if !token.is_empty() {
            credentials::store_token(token)?;
        }
    }

    Ok(())
}
