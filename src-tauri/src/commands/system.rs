use tauri::{AppHandle, Wry};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::{Store, StoreExt};

pub(crate) const READ_STATE_PREFIX: &str = "mr_read_state";

// the validated identity lives in a single store object so a reader sees either
// the whole old identity or the whole new one, never a torn mix of a new url with
// a stale user_id. only `connect`/`disconnect` write it.
const IDENTITY_KEY: &str = "identity";

// write-ahead marker that the keychain token matches the stored identity. the
// keychain and settings.json cannot be written atomically, so commit_identity
// sets this false before the keychain write and true after. a crash in that
// window leaves it false, and startup refuses to poll (fail closed) instead of
// silently polling a stale-token/identity mix - e.g. a same-host account switch
// where the old token would authenticate as the old user under the new user_id
// filter and never 401.
const TOKEN_COMMITTED_KEY: &str = "token_committed";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoredIdentity {
    pub url: String,
    pub user_id: Option<i64>,
    pub username: String,
}

fn parse_identity(val: &serde_json::Value) -> Option<StoredIdentity> {
    let obj = val.as_object()?;
    Some(StoredIdentity {
        url: obj
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        user_id: obj.get("user_id").and_then(|v| v.as_i64()),
        username: obj
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

pub(crate) fn encode_identity(
    url: &str,
    user_id: Option<i64>,
    username: &str,
) -> serde_json::Value {
    serde_json::json!({ "url": url, "user_id": user_id, "username": username })
}

pub(crate) fn read_identity(store: &Store<Wry>) -> StoredIdentity {
    if let Some(identity) = store.get(IDENTITY_KEY).as_ref().and_then(parse_identity) {
        return identity;
    }
    // fall back to the pre-object layout for an install that has not reconnected
    // since the upgrade; migrate_legacy_identity rewrites these into the object on
    // startup, so this branch is only hit before that runs.
    StoredIdentity {
        url: store
            .get("gitlab_url")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
        user_id: store.get("user_id").and_then(|v| v.as_i64()),
        username: store
            .get("username")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
    }
}

pub(crate) fn write_identity(store: &Store<Wry>, url: &str, user_id: Option<i64>, username: &str) {
    store.set(IDENTITY_KEY, encode_identity(url, user_id, username));
}

pub(crate) fn identity_connected(url: &str, user_id: Option<i64>, has_token: bool) -> bool {
    has_token && user_id.is_some() && !url.is_empty()
}

// absent defaults to true so installs that connected before this marker existed
// keep their connection; only a value explicitly set false (mid-commit crash)
// fails closed.
fn token_committed_value(val: Option<&serde_json::Value>) -> bool {
    val.and_then(|v| v.as_bool()).unwrap_or(true)
}

pub(crate) fn token_committed(store: &Store<Wry>) -> bool {
    token_committed_value(store.get(TOKEN_COMMITTED_KEY).as_ref())
}

pub(crate) fn set_token_committed(store: &Store<Wry>, committed: bool) {
    store.set(TOKEN_COMMITTED_KEY, serde_json::json!(committed));
}

// each account's read-state and reminders live in their own store file, keyed by
// host + user_id, so switching accounts reads different files and no per-account
// state has to be wiped. returns None when no account is configured yet.
pub(crate) fn account_store_name(app: &AppHandle, prefix: &str) -> Option<String> {
    let store = app.store("settings.json").ok()?;
    let identity = read_identity(&store);
    store_file_name(prefix, &identity.url, identity.user_id)
}

fn store_file_name(prefix: &str, url: &str, user_id: Option<i64>) -> Option<String> {
    let user_id = user_id?;
    if url.trim_end_matches('/').is_empty() {
        return None;
    }
    Some(format!("{prefix}_{}_{user_id}.json", sanitize_host(url)))
}

fn sanitize_host(url: &str) -> String {
    url.trim_end_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

// converts the pre-object identity layout (separate gitlab_url/user_id/username
// keys) into the single identity object once, so existing installs keep their
// connection and every later read is one atomic get.
pub(crate) fn migrate_legacy_identity(app: &AppHandle) {
    let Ok(store) = app.store("settings.json") else {
        return;
    };
    if store.get(IDENTITY_KEY).is_some() {
        return;
    }
    let url = store
        .get("gitlab_url")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let user_id = store.get("user_id").and_then(|v| v.as_i64());
    let username = store
        .get("username")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    if url.is_empty() && user_id.is_none() {
        return;
    }
    write_identity(&store, &url, user_id, &username);
    store.delete("gitlab_url");
    store.delete("user_id");
    store.delete("username");
    let _ = store.save();
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

// copy only when the source has data and the target has none, so an account that
// already owns data is never clobbered.
fn should_copy_legacy(legacy_empty: bool, target_empty: bool) -> bool {
    !legacy_empty && target_empty
}

fn migrate_legacy_store(app: &AppHandle, legacy_name: &str, prefix: &str) {
    let Some(target_name) = account_store_name(app, prefix) else {
        return;
    };
    let Ok(legacy) = app.store(legacy_name) else {
        return;
    };
    let Ok(target) = app.store(&target_name) else {
        return;
    };
    if !should_copy_legacy(legacy.keys().is_empty(), target.keys().is_empty()) {
        return;
    }
    for key in legacy.keys() {
        if let Some(value) = legacy.get(&key) {
            target.set(key, value);
        }
    }
    // clear the source only after the copy is safely on disk: clearing on a failed
    // save would drop the data from both files
    if target.save().is_err() {
        return;
    }
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

fn read_state(store: &Store<Wry>, key: &str) -> (bool, String, Option<i64>) {
    match store.get(key).as_ref().and_then(StoredReadState::parse) {
        Some(state) => (
            state.unread(),
            state.updated_at().unwrap_or_default().to_string(),
            state.review_request_todo_id(),
        ),
        None => (true, String::new(), None),
    }
}

pub(crate) fn read_review_request_todo_id(store: &Store<Wry>, key: &str) -> Option<i64> {
    read_state(store, key).2
}

pub(crate) fn write_state(
    store: &Store<Wry>,
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
    fn store_file_name_builds_per_account_name() {
        // assert
        assert_eq!(
            store_file_name(READ_STATE_PREFIX, "https://gl.example.com/", Some(7)),
            Some("mr_read_state_https___gl_example_com_7.json".to_string())
        );
    }

    #[test]
    fn store_file_name_none_without_user_id() {
        // assert
        assert_eq!(
            store_file_name(READ_STATE_PREFIX, "https://gl.example.com", None),
            None
        );
    }

    #[test]
    fn store_file_name_none_for_empty_url() {
        // assert
        assert_eq!(store_file_name(READ_STATE_PREFIX, "", Some(7)), None);
        assert_eq!(store_file_name(READ_STATE_PREFIX, "/", Some(7)), None);
    }

    #[test]
    fn parse_identity_reads_full_object() {
        // arrange
        let val = serde_json::json!({
            "url": "https://gl.example.com",
            "user_id": 42,
            "username": "alice",
        });

        // act
        let parsed = parse_identity(&val);

        // assert
        assert_eq!(
            parsed,
            Some(StoredIdentity {
                url: "https://gl.example.com".to_string(),
                user_id: Some(42),
                username: "alice".to_string(),
            })
        );
    }

    #[test]
    fn parse_identity_missing_user_id_is_none_field() {
        // arrange
        let val = serde_json::json!({ "url": "https://gl.example.com", "username": "" });

        // act
        let parsed = parse_identity(&val);

        // assert
        assert_eq!(
            parsed,
            Some(StoredIdentity {
                url: "https://gl.example.com".to_string(),
                user_id: None,
                username: String::new(),
            })
        );
    }

    #[test]
    fn parse_identity_non_object_returns_none() {
        // assert
        assert_eq!(parse_identity(&serde_json::Value::Null), None);
        assert_eq!(parse_identity(&serde_json::json!("string")), None);
    }

    #[test]
    fn encode_identity_roundtrips_through_parse() {
        // act
        let parsed = parse_identity(&encode_identity("https://gl.example.com", Some(7), "alice"));

        // assert
        assert_eq!(
            parsed,
            Some(StoredIdentity {
                url: "https://gl.example.com".to_string(),
                user_id: Some(7),
                username: "alice".to_string(),
            })
        );
    }

    #[test]
    fn identity_connected_requires_url_user_and_token() {
        // assert
        assert!(identity_connected("https://gl.example.com", Some(7), true));
        assert!(!identity_connected(
            "https://gl.example.com",
            Some(7),
            false
        ));
        assert!(!identity_connected("https://gl.example.com", None, true));
        assert!(!identity_connected("", Some(7), true));
    }

    #[test]
    fn token_committed_value_defaults_true_when_absent_or_non_bool() {
        // assert
        assert!(token_committed_value(None));
        assert!(token_committed_value(Some(&serde_json::Value::Null)));
        assert!(token_committed_value(Some(&serde_json::json!("yes"))));
    }

    #[test]
    fn token_committed_value_reads_explicit_bool() {
        // assert
        assert!(token_committed_value(Some(&serde_json::json!(true))));
        assert!(!token_committed_value(Some(&serde_json::json!(false))));
    }

    #[test]
    fn should_copy_legacy_only_when_source_has_data_and_target_empty() {
        // assert
        assert!(should_copy_legacy(false, true));
        assert!(!should_copy_legacy(true, true));
        assert!(!should_copy_legacy(false, false));
        assert!(!should_copy_legacy(true, false));
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
