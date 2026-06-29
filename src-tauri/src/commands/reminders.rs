use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub(crate) const REMINDERS_PREFIX: &str = "reminders";

// a reminder value is either {"at", "label"} or a legacy bare string that stands
// in for both fields; pull the requested key from whichever shape is stored.
pub(crate) fn reminder_field(val: &serde_json::Value, key: &str) -> Option<String> {
    if let Some(obj) = val.as_object() {
        return obj.get(key).and_then(|v| v.as_str()).map(String::from);
    }
    val.as_str().map(String::from)
}

#[tauri::command]
pub async fn set_reminder(
    app: AppHandle,
    mr_id: i64,
    at: String,
    label: String,
) -> Result<(), String> {
    let name = crate::commands::system::account_store_name(&app, REMINDERS_PREFIX)
        .ok_or("Not connected")?;
    let store = app.store(name).map_err(|e| format!("Store error: {e}"))?;

    store.set(
        mr_id.to_string(),
        serde_json::json!({"at": at, "label": label}),
    );
    store.save().map_err(|e| format!("Save error: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn clear_reminder(app: AppHandle, mr_id: i64) -> Result<(), String> {
    let name = crate::commands::system::account_store_name(&app, REMINDERS_PREFIX)
        .ok_or("Not connected")?;
    let store = app.store(name).map_err(|e| format!("Store error: {e}"))?;

    store.delete(mr_id.to_string());
    store.save().map_err(|e| format!("Save error: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: &str = "2026-01-01T10:00:00Z";

    #[test]
    fn reminder_field_reads_object_and_legacy_string() {
        // assert
        assert_eq!(
            reminder_field(&serde_json::json!({"at": T1, "label": "Later"}), "at"),
            Some(T1.to_string())
        );
        assert_eq!(
            reminder_field(&serde_json::json!({"at": T1, "label": "Later"}), "label"),
            Some("Later".to_string())
        );
        assert_eq!(
            reminder_field(&serde_json::json!(T1), "at"),
            Some(T1.to_string())
        );
        assert_eq!(
            reminder_field(&serde_json::json!(T1), "label"),
            Some(T1.to_string())
        );
        assert_eq!(reminder_field(&serde_json::json!({}), "at"), None);
        assert_eq!(reminder_field(&serde_json::json!(42), "at"), None);
    }
}
