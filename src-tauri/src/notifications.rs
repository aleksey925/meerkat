use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_store::StoreExt;

/// Check notification permission by reading macOS notification preferences plist.
/// The `auth` field in ncprefs.plist is non-zero when notifications are authorized.
#[cfg(target_os = "macos")]
pub fn check_permission() -> bool {
    const BUNDLE_ID: &str = "com.meerkat.app";

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return true,
    };
    let plist_path = format!("{home}/Library/Preferences/com.apple.ncprefs.plist");

    let data: plist::Value = match plist::from_file(&plist_path) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Failed to read ncprefs.plist: {e}");
            return true;
        }
    };

    let Some(apps) = data
        .as_dictionary()
        .and_then(|d| d.get("apps"))
        .and_then(|a| a.as_array())
    else {
        log::warn!("No apps array in ncprefs.plist");
        return true;
    };

    for app in apps {
        let Some(dict) = app.as_dictionary() else {
            continue;
        };
        let Some(bid) = dict.get("bundle-id").and_then(|b| b.as_string()) else {
            continue;
        };
        if bid == BUNDLE_ID {
            let auth = dict
                .get("auth")
                .and_then(|a| a.as_signed_integer())
                .unwrap_or(0);
            log::info!("Notification permission for {BUNDLE_ID}: auth={auth}");
            return auth > 0;
        }
    }

    log::info!("App {BUNDLE_ID} not found in ncprefs.plist (NotDetermined)");
    false
}

#[cfg(not(target_os = "macos"))]
pub fn check_permission() -> bool {
    true
}

/// Send a trigger notification to provoke the macOS permission dialog.
pub fn prompt_permission(app: &AppHandle) {
    if !check_permission() {
        log::info!("Permission not granted, sending trigger notification...");
        let _ = app
            .notification()
            .builder()
            .title("Meerkat")
            .body("Please allow notifications to get alerts about merge requests.")
            .show();
    }
}

fn read_notif_settings(app: &AppHandle) -> (bool, bool) {
    let store = match app.store("settings.json").ok() {
        Some(s) => s,
        None => return (true, true),
    };
    let desktop = store
        .get("desktop_notif")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let sound = store
        .get("sound_notif")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    (desktop, sound)
}

pub fn send_notification(app: &AppHandle, title: &str, body: &str) {
    let (desktop_enabled, sound_enabled) = read_notif_settings(app);
    if !desktop_enabled {
        return;
    }

    // switch to Regular so macOS can activate the app when the notification is clicked,
    // then revert to Accessory after a timeout if the user didn't interact
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        let app_clone = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if let Some(window) = app_clone.get_webview_window("main") {
                if !window.is_visible().unwrap_or(true) {
                    let _ = app_clone.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        });
    }

    let mut builder = app.notification().builder().title(title).body(body);
    if sound_enabled {
        builder = builder.sound("Default");
    }

    if let Err(e) = builder.show() {
        log::warn!("Failed to send notification: {e}");
    }
}

pub fn notify_new_mr(app: &AppHandle, author: &str, title: &str, project: &str) {
    send_notification(
        app,
        "New review request",
        &format!("{} opened '{}' in {}", author, title, project),
    );
}

pub fn notify_mr_updated(app: &AppHandle, author: &str, title: &str) {
    send_notification(
        app,
        "MR updated",
        &format!("{} updated '{}'", author, title),
    );
}

pub fn notify_review_requested(app: &AppHandle, author: &str, title: &str) {
    send_notification(
        app,
        "Review requested",
        &format!("{} requested your review on '{}'", author, title),
    );
}

pub fn notify_pipeline_failed(app: &AppHandle, title: &str) {
    send_notification(app, "Pipeline failed", &format!("CI failed on '{}'", title));
}

pub fn notify_reminder(app: &AppHandle, title: &str) {
    send_notification(app, "Reminder", &format!("Time to check '{}'", title));
}
