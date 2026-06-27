use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::StoreExt;

const READ_STATE_STORE: &str = "mr_read_state.json";

fn read_state(
    store: &tauri_plugin_store::Store<tauri::Wry>,
    key: &str,
) -> (bool, String, Option<i64>) {
    store
        .get(key)
        .map(|v| {
            if let Some(obj) = v.as_object() {
                let unread = obj.get("unread").and_then(|u| u.as_bool()).unwrap_or(true);
                let at = obj
                    .get("updatedAt")
                    .and_then(|u| u.as_str())
                    .map(String::from)
                    .unwrap_or_default();
                let todo_id = obj.get("reviewRequestTodoId").and_then(|u| u.as_i64());
                (unread, at, todo_id)
            } else {
                (v.as_bool().unwrap_or(true), String::new(), None)
            }
        })
        .unwrap_or((true, String::new(), None))
}

pub(crate) fn read_review_request_todo_id(
    store: &tauri_plugin_store::Store<tauri::Wry>,
    key: &str,
) -> Option<i64> {
    read_state(store, key).2
}

pub(crate) fn write_state(
    store: &tauri_plugin_store::Store<tauri::Wry>,
    key: &str,
    unread: bool,
    updated_at: &str,
    source: &str,
    review_request_todo_id: Option<i64>,
) {
    store.set(
        key,
        serde_json::json!({
            "unread": unread,
            "updatedAt": updated_at,
            "source": source,
            "reviewRequestTodoId": review_request_todo_id,
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
    let (current_unread, mut updated_at, todo_id) = read_state(&store, &key);

    // legacy bool entries have no updatedAt — fall back to the latest known value
    // so the user-pin can be respected on the next fetch
    if updated_at.is_empty() {
        updated_at = crate::polling::previous_mr_updated_at_raw(mr_id).unwrap_or_default();
    }

    let new_value = !current_unread;
    // keep the stored todo id so the pin stays anchored to the current re-request:
    // a later re-request gets a new todo id and correctly breaks the pin
    write_state(&store, &key, new_value, &updated_at, "user", todo_id);
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
    crate::notifications::send_notification(
        &app,
        "Meerkat",
        "Test notification — everything works!",
    );
}

#[tauri::command]
pub fn get_app_version(app: AppHandle) -> String {
    let version = app
        .config()
        .version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let hash = env!("COMMIT_HASH");
    if hash.is_empty() {
        version
    } else {
        format!("{version} ({hash})")
    }
}
