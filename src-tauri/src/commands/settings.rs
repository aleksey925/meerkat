use crate::commands::system;
use crate::credentials;
use crate::models::{Preferences, Settings};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::{Store, StoreExt};

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    let identity = system::read_identity(&store);
    let url = identity.url;

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
    let connected = system::identity_connected(&url, identity.user_id, has_token)
        && system::token_committed(&store);

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
pub async fn save_preferences(app: AppHandle, preferences: Preferences) -> Result<(), String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    store.set(
        "poll_interval",
        serde_json::json!(preferences.poll_interval),
    );
    store.set("show_drafts", serde_json::json!(preferences.show_drafts));
    store.set(
        "desktop_notif",
        serde_json::json!(preferences.desktop_notif),
    );
    store.set("sound_notif", serde_json::json!(preferences.sound_notif));
    store.save().map_err(|e| format!("Save error: {e}"))?;

    Ok(())
}

// stops polling and drops the identity so the app returns to a disconnected
// state. the url is kept so the field stays prefilled; per-account stores are
// left in place, so reconnecting the same account restores its data.
#[tauri::command]
pub async fn disconnect(app: AppHandle) -> Result<(), String> {
    // serialize against connect so neither one swaps the identity while the
    // other's aborted cycle is still unwinding (see lock_lifecycle).
    let _guard = crate::polling::lock_lifecycle().await;

    // read before stopping, so a store/keychain read error leaves the account
    // polling instead of stopping it silently.
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;
    let previous = system::read_identity(&store);
    let previous_token = credentials::get_token()?;

    crate::polling::stop_polling().await;

    system::write_identity(&store, &previous.url, None, "");
    system::set_token_committed(&store, false);
    if let Err(e) = store.save() {
        restore_identity(&app, &store, &previous, previous_token.as_deref());
        return Err(format!("Save error: {e}"));
    }
    if let Err(e) = credentials::delete_token() {
        restore_identity(&app, &store, &previous, previous_token.as_deref());
        return Err(e);
    }

    crate::polling::reset_previous_mrs();
    Ok(())
}

// restores the previous identity and token after a failed connect/disconnect and
// resumes the previous account's polling when the token was restored and the
// identity is usable. a failed token restore could pair the previous identity
// with the other account's token, so it clears the identity to a disconnected
// state instead - keeping is_connected/get_settings in agreement with whether the
// poll task is running.
pub(crate) fn restore_identity(
    app: &AppHandle,
    store: &Store<Wry>,
    previous: &system::StoredIdentity,
    previous_token: Option<&str>,
) {
    let token_restored = match previous_token {
        Some(prev) => credentials::store_token(prev).is_ok(),
        None => credentials::delete_token().is_ok(),
    };
    // if the token could not be restored, the keychain may still hold the other
    // account's token; pairing it with the previous identity would poll the old
    // host with the wrong token. clear the identity to a disconnected state
    // instead of persisting that mismatch.
    let restored = if token_restored {
        previous.clone()
    } else {
        system::StoredIdentity {
            url: String::new(),
            user_id: None,
            username: String::new(),
        }
    };
    system::write_identity(store, &restored.url, restored.user_id, &restored.username);
    // the restored identity and keychain token match only when the token was
    // restored; mark committed accordingly so a half-restore stays fail closed.
    system::set_token_committed(store, token_restored);
    // the store singleton backs is_connected/get_settings and the poll task's
    // identity read, so all three see the in-memory values set above whether or
    // not this disk save lands. resume polling whenever the in-memory identity is
    // usable so the reported connection state and the running poll task agree; a
    // failed save only costs the next restart, which reads stale settings and
    // fails closed (the marker was set to token_restored above).
    let _ = store.save();
    // resume only for a full, usable identity: an empty url (cleared on a failed
    // token restore) or a missing token would start a poll loop that can never
    // fetch anything.
    let prev_connected =
        system::identity_connected(&restored.url, restored.user_id, previous_token.is_some());
    if token_restored && prev_connected {
        crate::polling::start_polling(app.clone());
    }
}

// whether a usable identity is configured: a url, a validated user_id, and a
// stored token. used at startup to decide whether to begin polling.
pub(crate) fn is_connected(app: &AppHandle) -> bool {
    let Ok(store) = app.store("settings.json") else {
        return false;
    };
    let identity = system::read_identity(&store);
    let has_token = credentials::get_token().ok().flatten().is_some();
    system::identity_connected(&identity.url, identity.user_id, has_token)
        && system::token_committed(&store)
}
