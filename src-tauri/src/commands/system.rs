use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::StoreExt;

const READ_STATE_STORE: &str = "mr_read_state.json";

fn read_state(store: &tauri_plugin_store::Store<tauri::Wry>, key: &str) -> (bool, String) {
    store
        .get(key)
        .map(|v| {
            if let Some(obj) = v.as_object() {
                let unread = obj
                    .get("unread")
                    .and_then(|u| u.as_bool())
                    .unwrap_or(true);
                let at = obj
                    .get("updatedAt")
                    .and_then(|u| u.as_str())
                    .map(String::from)
                    .unwrap_or_default();
                (unread, at)
            } else {
                (v.as_bool().unwrap_or(true), String::new())
            }
        })
        .unwrap_or((true, String::new()))
}

fn write_state(
    store: &tauri_plugin_store::Store<tauri::Wry>,
    key: &str,
    unread: bool,
    updated_at: &str,
) {
    store.set(
        key,
        serde_json::json!({
            "unread": unread,
            "updatedAt": updated_at,
        }),
    );
}

#[tauri::command]
pub async fn open_in_browser(app: AppHandle, url: String) -> Result<(), String> {
    app.opener()
        .open_url(&url, None::<&str>)
        .map_err(|e| format!("Failed to open URL: {e}"))
}

#[tauri::command]
pub async fn toggle_unread(app: AppHandle, mr_id: i64) -> Result<bool, String> {
    let store = app
        .store(READ_STATE_STORE)
        .map_err(|e| format!("Store error: {e}"))?;

    let key = mr_id.to_string();
    let (current_unread, updated_at) = read_state(&store, &key);

    let new_value = !current_unread;
    write_state(&store, &key, new_value, &updated_at);
    store.save().map_err(|e| format!("Save error: {e}"))?;

    Ok(new_value)
}

#[tauri::command]
pub fn update_tray_badge(app: AppHandle, count: usize) {
    crate::update_tray(&app, count);
}

#[tauri::command]
pub fn check_notification_permission() -> bool {
    crate::notifications::check_permission()
}

#[tauri::command]
pub fn prompt_notification_permission(app: AppHandle) {
    crate::notifications::prompt_permission(&app);
}

#[tauri::command]
pub fn send_test_notification(app: AppHandle) {
    crate::notifications::send_notification(&app, "Meerkat", "Test notification — everything works!");
}
