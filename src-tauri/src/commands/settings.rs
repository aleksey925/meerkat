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

    let stored_token = credentials::get_token()?;
    let has_token = stored_token.is_some();
    let connected = has_token && store.get("user_id").is_some() && !url.is_empty();

    let token_display = stored_token.filter(|t| !t.is_empty());

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

// persists non-identity settings only. the identity (url + token) is owned by
// `connect`, so a preferences save never touches it and needs no validation or
// poll restart.
#[tauri::command]
pub async fn save_preferences(app: AppHandle, settings: Settings) -> Result<(), String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    store.set("poll_interval", serde_json::json!(settings.poll_interval));
    store.set("show_drafts", serde_json::json!(settings.show_drafts));
    store.set("desktop_notif", serde_json::json!(settings.desktop_notif));
    store.set("sound_notif", serde_json::json!(settings.sound_notif));
    store.save().map_err(|e| format!("Save error: {e}"))?;

    Ok(())
}

// stops polling and drops the identity so the app returns to a disconnected
// state. per-account stores are left in place, so reconnecting the same account
// restores its read-state and reminders.
#[tauri::command]
pub async fn disconnect(app: AppHandle) -> Result<(), String> {
    crate::polling::stop_polling();

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;
    store.delete("user_id");
    store.delete("username");
    store.save().map_err(|e| format!("Save error: {e}"))?;

    credentials::delete_token()?;
    crate::polling::reset_previous_mrs();
    Ok(())
}

// whether a usable identity is configured: a url, a validated user_id, and a
// stored token. used at startup to decide whether to begin polling.
pub(crate) fn is_connected(app: &AppHandle) -> bool {
    let Ok(store) = app.store("settings.json") else {
        return false;
    };
    let has_url = store
        .get("gitlab_url")
        .and_then(|v| v.as_str().map(String::from))
        .is_some_and(|u| !u.is_empty());
    let has_user = store.get("user_id").and_then(|v| v.as_i64()).is_some();
    let has_token = credentials::get_token().ok().flatten().is_some();
    has_url && has_user && has_token
}
