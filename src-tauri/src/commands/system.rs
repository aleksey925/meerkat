use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::StoreExt;

pub(crate) const READ_STATE_PREFIX: &str = "mr_read_state";

// each account's read-state and reminders live in their own store file, keyed by
// host + user_id, so switching accounts reads different files and no per-account
// state has to be wiped. returns None when no account is configured yet.
pub(crate) fn account_store_name(app: &AppHandle, prefix: &str) -> Option<String> {
    let store = app.store("settings.json").ok()?;
    let url = store
        .get("gitlab_url")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|u| !u.is_empty())?;
    let user_id = store.get("user_id").and_then(|v| v.as_i64())?;
    Some(format!("{prefix}_{}_{user_id}.json", sanitize_host(&url)))
}

fn sanitize_host(url: &str) -> String {
    url.trim_end_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

// before namespacing, read-state and reminders lived in single shared files.
// move that data into the current account's files once so an existing user keeps
// their reminders and read pins after the upgrade. only runs when an account is
// configured and that account has no data of its own yet.
pub(crate) fn migrate_legacy_stores(app: &AppHandle) {
    migrate_legacy_store(app, "mr_read_state.json", READ_STATE_PREFIX);
    migrate_legacy_store(
        app,
        "reminders.json",
        crate::commands::reminders::REMINDERS_PREFIX,
    );
}

fn migrate_legacy_store(app: &AppHandle, legacy_name: &str, prefix: &str) {
    let Some(target_name) = account_store_name(app, prefix) else {
        return;
    };
    let Ok(legacy) = app.store(legacy_name) else {
        return;
    };
    if legacy.keys().is_empty() {
        return;
    }
    let Ok(target) = app.store(&target_name) else {
        return;
    };
    // do not clobber an account that already has its own data
    if !target.keys().is_empty() {
        return;
    }
    for key in legacy.keys() {
        if let Some(value) = legacy.get(&key) {
            target.set(key, value);
        }
    }
    let _ = target.save();
    legacy.clear();
    let _ = legacy.save();
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ReadStateSource {
    User,
    Auto,
}

impl ReadStateSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReadStateSource::User => "user",
            ReadStateSource::Auto => "auto",
        }
    }

    fn from_stored(s: &str) -> Self {
        match s {
            "user" => ReadStateSource::User,
            _ => ReadStateSource::Auto,
        }
    }
}

// a legacy bare bool carries no updated_at/source, so it is kept distinct from
// the full object: the unread decision treats a missing anchor (legacy) as "do
// not compare", which is different from comparing against an empty string.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StoredReadState {
    Full {
        unread: bool,
        updated_at: String,
        source: ReadStateSource,
        review_request_todo_id: Option<i64>,
    },
    LegacyBool(bool),
}

impl StoredReadState {
    pub(crate) fn parse(val: &serde_json::Value) -> Option<Self> {
        if let Some(obj) = val.as_object() {
            let unread = obj.get("unread").and_then(|v| v.as_bool()).unwrap_or(true);
            let updated_at = obj
                .get("updatedAt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source = obj
                .get("source")
                .and_then(|v| v.as_str())
                .map(ReadStateSource::from_stored)
                .unwrap_or(ReadStateSource::Auto);
            let review_request_todo_id = obj.get("reviewRequestTodoId").and_then(|v| v.as_i64());
            return Some(StoredReadState::Full {
                unread,
                updated_at,
                source,
                review_request_todo_id,
            });
        }
        val.as_bool().map(StoredReadState::LegacyBool)
    }

    pub(crate) fn unread(&self) -> bool {
        match self {
            StoredReadState::Full { unread, .. } => *unread,
            StoredReadState::LegacyBool(b) => *b,
        }
    }

    pub(crate) fn updated_at(&self) -> Option<&str> {
        match self {
            StoredReadState::Full { updated_at, .. } => Some(updated_at),
            StoredReadState::LegacyBool(_) => None,
        }
    }

    pub(crate) fn review_request_todo_id(&self) -> Option<i64> {
        match self {
            StoredReadState::Full {
                review_request_todo_id,
                ..
            } => *review_request_todo_id,
            StoredReadState::LegacyBool(_) => None,
        }
    }
}

pub(crate) fn encode_read_state(
    unread: bool,
    updated_at: &str,
    source: ReadStateSource,
    review_request_todo_id: Option<i64>,
) -> serde_json::Value {
    serde_json::json!({
        "unread": unread,
        "updatedAt": updated_at,
        "source": source.as_str(),
        "reviewRequestTodoId": review_request_todo_id,
    })
}

fn read_state(
    store: &tauri_plugin_store::Store<tauri::Wry>,
    key: &str,
) -> (bool, String, Option<i64>) {
    match store.get(key).as_ref().and_then(StoredReadState::parse) {
        Some(state) => (
            state.unread(),
            state.updated_at().unwrap_or_default().to_string(),
            state.review_request_todo_id(),
        ),
        None => (true, String::new(), None),
    }
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
    source: ReadStateSource,
    review_request_todo_id: Option<i64>,
) {
    store.set(
        key,
        encode_read_state(unread, updated_at, source, review_request_todo_id),
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
    let name = account_store_name(&app, READ_STATE_PREFIX).ok_or("Not connected")?;
    let store = app.store(name).map_err(|e| format!("Store error: {e}"))?;

    let key = mr_id.to_string();
    let (current_unread, mut updated_at, todo_id) = read_state(&store, &key);

    // legacy bool entries have no updatedAt - fall back to the latest known value
    // so the user-pin can be respected on the next fetch
    if updated_at.is_empty() {
        updated_at = crate::polling::previous_mr_updated_at_raw(mr_id).unwrap_or_default();
    }

    let new_value = !current_unread;
    // keep the stored todo id so the pin stays anchored to the current re-request:
    // a later re-request gets a new todo id and correctly breaks the pin
    write_state(
        &store,
        &key,
        new_value,
        &updated_at,
        ReadStateSource::User,
        todo_id,
    );
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
        "Test notification - everything works!",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_host_replaces_non_alphanumerics_and_ignores_trailing_slash() {
        // assert
        assert_eq!(
            sanitize_host("https://gl.example.com/"),
            "https___gl_example_com"
        );
        assert_eq!(
            sanitize_host("https://gl.example.com"),
            "https___gl_example_com"
        );
    }

    #[test]
    fn read_state_source_roundtrip() {
        for src in [ReadStateSource::User, ReadStateSource::Auto] {
            // act
            let restored = ReadStateSource::from_stored(src.as_str());

            // assert
            assert_eq!(restored, src);
        }
    }

    #[test]
    fn encode_read_state_roundtrips_through_parse() {
        // arrange
        let encoded = encode_read_state(
            true,
            "2026-02-02T09:00:00Z",
            ReadStateSource::Auto,
            Some(42),
        );

        // act
        let parsed = StoredReadState::parse(&encoded);

        // assert
        assert_eq!(
            parsed,
            Some(StoredReadState::Full {
                unread: true,
                updated_at: "2026-02-02T09:00:00Z".to_string(),
                source: ReadStateSource::Auto,
                review_request_todo_id: Some(42),
            })
        );
    }

    #[test]
    fn encode_read_state_preserves_source() {
        // act
        let encoded = encode_read_state(false, "2026-01-01T10:00:00Z", ReadStateSource::User, None);

        // assert
        assert_eq!(encoded["source"], serde_json::json!("user"));
    }

    #[test]
    fn stored_read_state_accessors_for_full() {
        // arrange
        let state = StoredReadState::Full {
            unread: false,
            updated_at: "2026-01-01T10:00:00Z".to_string(),
            source: ReadStateSource::User,
            review_request_todo_id: Some(7),
        };

        // assert
        assert!(!state.unread());
        assert_eq!(state.updated_at(), Some("2026-01-01T10:00:00Z"));
        assert_eq!(state.review_request_todo_id(), Some(7));
    }

    #[test]
    fn stored_read_state_accessors_for_legacy_bool() {
        // arrange
        let state = StoredReadState::LegacyBool(false);

        // assert
        assert!(!state.unread());
        assert_eq!(state.updated_at(), None);
        assert_eq!(state.review_request_todo_id(), None);
    }

    const T1: &str = "2026-01-01T10:00:00Z";

    fn full(unread: bool, updated_at: &str, source: ReadStateSource) -> StoredReadState {
        StoredReadState::Full {
            unread,
            updated_at: updated_at.to_string(),
            source,
            review_request_todo_id: None,
        }
    }

    #[test]
    fn parse_full_object_extracts_all_fields() {
        // arrange
        let val = serde_json::json!({
            "unread": false,
            "updatedAt": T1,
            "source": "user",
        });

        // act
        let parsed = StoredReadState::parse(&val);

        // assert
        assert_eq!(parsed, Some(full(false, T1, ReadStateSource::User)));
    }

    #[test]
    fn parse_full_object_extracts_review_request_todo_id() {
        // arrange
        let val = serde_json::json!({
            "unread": false,
            "updatedAt": T1,
            "source": "user",
            "reviewRequestTodoId": 42,
        });

        // act
        let parsed = StoredReadState::parse(&val);

        // assert
        assert_eq!(
            parsed,
            Some(StoredReadState::Full {
                unread: false,
                updated_at: T1.to_string(),
                source: ReadStateSource::User,
                review_request_todo_id: Some(42),
            })
        );
    }

    #[test]
    fn parse_object_missing_source_defaults_to_auto() {
        // arrange
        let val = serde_json::json!({ "unread": true, "updatedAt": T1 });

        // act
        let parsed = StoredReadState::parse(&val);

        // assert
        assert_eq!(parsed, Some(full(true, T1, ReadStateSource::Auto)));
    }

    #[test]
    fn parse_object_unknown_source_defaults_to_auto() {
        // arrange
        let val = serde_json::json!({ "unread": true, "updatedAt": T1, "source": "weird" });

        // act
        let parsed = StoredReadState::parse(&val);

        // assert
        assert_eq!(parsed, Some(full(true, T1, ReadStateSource::Auto)));
    }

    #[test]
    fn parse_object_missing_updated_at_defaults_to_empty() {
        // arrange
        let val = serde_json::json!({ "unread": true });

        // act
        let parsed = StoredReadState::parse(&val);

        // assert
        assert_eq!(parsed, Some(full(true, "", ReadStateSource::Auto)));
    }

    #[test]
    fn parse_legacy_bool_returns_legacy_variant() {
        // act
        let parsed_false = StoredReadState::parse(&serde_json::Value::Bool(false));
        let parsed_true = StoredReadState::parse(&serde_json::Value::Bool(true));

        // assert
        assert_eq!(parsed_false, Some(StoredReadState::LegacyBool(false)));
        assert_eq!(parsed_true, Some(StoredReadState::LegacyBool(true)));
    }

    #[test]
    fn parse_unknown_value_returns_none() {
        // assert
        assert_eq!(StoredReadState::parse(&serde_json::Value::Null), None);
        assert_eq!(StoredReadState::parse(&serde_json::json!("string")), None);
        assert_eq!(StoredReadState::parse(&serde_json::json!(42)), None);
    }
}
