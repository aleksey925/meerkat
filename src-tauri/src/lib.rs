#![allow(dead_code)]

mod commands;
mod credentials;
mod models;
mod notifications;
mod polling;

use tauri::{Emitter, Listener, Manager, RunEvent};

#[cfg(target_os = "macos")]
static APP_WAS_INACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(desktop)]
pub fn update_tray(handle: &tauri::AppHandle, unread: usize) {
    if let Some(tray) = handle.tray_by_id("main") {
        let title = if unread > 0 {
            format!("{}", unread)
        } else {
            String::new()
        };
        let _ = tray.set_title(Some(title.as_str()));
    }
}

#[cfg(not(desktop))]
pub fn update_tray(_handle: &tauri::AppHandle, _unread: usize) {}

pub fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "macos")]
        {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        }
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
fn build_tray_menu(app: &tauri::AppHandle) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};

    let open_item = MenuItemBuilder::with_id("open", "Open").build(app)?;
    let check_now_item = MenuItemBuilder::with_id("check_now", "Check Now").build(app)?;
    let settings_item = MenuItemBuilder::with_id("settings", "Settings...").build(app)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    MenuBuilder::new(app)
        .item(&open_item)
        .item(&check_now_item)
        .item(&settings_item)
        .item(&sep)
        .item(&quit_item)
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            #[cfg(desktop)]
            {
                let handle = app.handle().clone();

                // set initial menu on the config-created tray
                if let Some(tray) = app.tray_by_id("main") {
                    let menu = build_tray_menu(&handle)?;
                    let _ = tray.set_menu(Some(menu));
                    let _ = tray.set_show_menu_on_left_click(true);

                    tray.on_menu_event(move |app, event| match event.id().as_ref() {
                        "open" => {
                            focus_main_window(app);
                        }
                        "settings" => {
                            focus_main_window(app);
                            let _ = app.emit("navigate", "settings");
                        }
                        "check_now" => {
                            let handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = polling::run_check_cycle(&handle, true).await;
                            });
                        }
                        "quit" => {
                            std::process::exit(0);
                        }
                        _ => {}
                    });
                }

                // update tray on mr-update events
                let handle2 = app.handle().clone();
                app.listen("mr-update", move |event: tauri::Event| {
                    if let Ok(payload) =
                        serde_json::from_str::<models::MrUpdatePayload>(event.payload())
                    {
                        let unread = payload.active.iter().filter(|m| m.unread).count();
                        update_tray(&handle2, unread);
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                #[cfg(target_os = "macos")]
                {
                    let _ = window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::gitlab::test_connection,
            commands::gitlab::fetch_merge_requests,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::reminders::set_reminder,
            commands::reminders::clear_reminder,
            commands::system::open_in_browser,
            commands::system::toggle_unread,
            commands::system::update_tray_badge,
            commands::system::check_notification_permission,
            commands::system::prompt_notification_permission,
            commands::system::send_test_notification,
            commands::system::get_app_version,
            polling::start_polling,
            polling::stop_polling,
            polling::check_now,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // mac-notification-sys (used by tauri-plugin-notification) sends notifications
            // via deprecated NSUserNotificationCenter without setting a delegate, so
            // notification click events are lost. Instead, we detect app activation
            // via NSApplication.isActive() on each event loop iteration and show the
            // hidden window when the app transitions from inactive to active.
            #[cfg(target_os = "macos")]
            match &event {
                RunEvent::Reopen {
                    has_visible_windows,
                    ..
                } => {
                    if !has_visible_windows {
                        focus_main_window(app_handle);
                    }
                }
                RunEvent::MainEventsCleared => {
                    // SAFETY: the run callback is always invoked on the main thread
                    let is_active = unsafe {
                        use objc2_app_kit::NSApplication;
                        let mtm = objc2::MainThreadMarker::new_unchecked();
                        NSApplication::sharedApplication(mtm).isActive()
                    };
                    let was_inactive =
                        APP_WAS_INACTIVE.swap(!is_active, std::sync::atomic::Ordering::Relaxed);
                    if is_active && was_inactive {
                        if let Some(window) = app_handle.get_webview_window("main") {
                            if !window.is_visible().unwrap_or(true) {
                                focus_main_window(app_handle);
                            }
                        }
                    }
                }
                _ => {}
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (&app_handle, &event);
            }
        });
}
