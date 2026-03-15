use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

#[tauri::command]
pub async fn set_reminder(app: AppHandle, mr_id: i64, at: String, label: String) -> Result<(), String> {
    let store = app
        .store("reminders.json")
        .map_err(|e| format!("Store error: {e}"))?;

    store.set(&mr_id.to_string(), serde_json::json!({"at": at, "label": label}));
    store.save().map_err(|e| format!("Save error: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn clear_reminder(app: AppHandle, mr_id: i64) -> Result<(), String> {
    let store = app
        .store("reminders.json")
        .map_err(|e| format!("Store error: {e}"))?;

    store.delete(&mr_id.to_string());
    store.save().map_err(|e| format!("Save error: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn get_reminder(app: AppHandle, mr_id: i64) -> Result<Option<String>, String> {
    let store = app
        .store("reminders.json")
        .map_err(|e| format!("Store error: {e}"))?;

    Ok(store.get(&mr_id.to_string()).and_then(|v| {
        if let Some(obj) = v.as_object() {
            obj.get("label").and_then(|l| l.as_str()).map(String::from)
        } else {
            v.as_str().map(String::from)
        }
    }))
}
