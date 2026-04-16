use crate::commands::gitlab::fetch_merge_requests;
use crate::models::{MergeRequest, PipelineStatus};
use crate::notifications;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;
use tokio::time::Duration;

static POLLING_ACTIVE: AtomicBool = AtomicBool::new(false);
static CHECK_CYCLE_RUNNING: AtomicBool = AtomicBool::new(false);
static PREVIOUS_MRS: Mutex<Option<Vec<MergeRequest>>> = Mutex::new(None);

fn detect_changes_and_notify(
    app: &AppHandle,
    new_active: &[MergeRequest],
    previous: &[MergeRequest],
) {
    let prev_map: HashMap<i64, &MergeRequest> = previous.iter().map(|m| (m.id, m)).collect();

    for mr in new_active {
        match prev_map.get(&mr.id) {
            None => {
                notifications::notify_new_mr(
                    app,
                    &mr.author_name,
                    &mr.title,
                    &mr.project_name,
                );
            }
            Some(prev_mr) => {
                if mr.updated_at != prev_mr.updated_at {
                    if let Some(ref actor) = mr.latest_actor {
                        notifications::notify_mr_updated(app, actor, &mr.title);
                    }
                }
                if mr.pipeline_status == Some(PipelineStatus::Fail)
                    && prev_mr.pipeline_status != Some(PipelineStatus::Fail)
                {
                    notifications::notify_pipeline_failed(app, &mr.title);
                }
            }
        }
    }
}

fn check_and_fire_reminders(app: &AppHandle) {
    let reminder_store = match app.store("reminders.json") {
        Ok(s) => s,
        Err(_) => return,
    };

    let now = Utc::now();
    let entries: Vec<(String, String)> = reminder_store
        .keys()
        .into_iter()
        .filter_map(|key| {
            let val = reminder_store.get(&key)?;
            let at_str = if let Some(obj) = val.as_object() {
                obj.get("at")?.as_str()?.to_string()
            } else {
                val.as_str()?.to_string()
            };
            Some((key, at_str))
        })
        .collect();

    let mut fired_ids: Vec<i64> = Vec::new();

    for (key, at_str) in entries {
        let reminder_time = match chrono::DateTime::parse_from_rfc3339(&at_str) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                // legacy unparseable value — will never fire, clean it up
                reminder_store.delete(&key);
                let _ = reminder_store.save();
                continue;
            }
        };

        if reminder_time <= now {
            let mr_id: i64 = match key.parse() {
                Ok(id) => id,
                Err(_) => continue,
            };

            let title = {
                let prev = PREVIOUS_MRS.lock().ok();
                prev.as_ref()
                    .and_then(|opt| opt.as_ref())
                    .and_then(|mrs| mrs.iter().find(|m| m.id == mr_id))
                    .map(|m| m.title.clone())
                    .unwrap_or_else(|| format!("MR #{}", mr_id))
            };

            notifications::notify_reminder(app, &title);

            // mark as unread
            if let Ok(read_store) = app.store("mr_read_state.json") {
                read_store.set(
                    &key,
                    serde_json::json!({ "unread": true }),
                );
                let _ = read_store.save();
            }

            reminder_store.delete(&key);
            let _ = reminder_store.save();

            fired_ids.push(mr_id);
        }
    }

    if !fired_ids.is_empty() {
        let _ = app.emit("reminders-fired", &fired_ids);
    }
}

pub async fn run_check_cycle(app: &AppHandle, manual: bool) -> Result<(), String> {
    if CHECK_CYCLE_RUNNING.swap(true, Ordering::SeqCst) {
        if manual {
            let _ = app.emit("check-already-running", ());
        }
        return Ok(());
    }

    if manual {
        let _ = app.emit("check-started", ());
    }

    let result = run_check_cycle_inner(app).await;

    if manual {
        let _ = app.emit("check-finished", result.is_ok());
    }

    CHECK_CYCLE_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_check_cycle_inner(app: &AppHandle) -> Result<(), String> {
    check_and_fire_reminders(app);

    match fetch_merge_requests(app.clone()).await {
        Ok(payload) => {
            {
                let prev = PREVIOUS_MRS.lock().ok();
                if let Some(ref prev_data) = prev.as_ref().and_then(|o| o.as_ref()) {
                    detect_changes_and_notify(app, &payload.active, prev_data);
                }
            }

            if let Ok(mut prev) = PREVIOUS_MRS.lock() {
                *prev = Some(payload.active.clone());
            }

            let _ = app.emit("mr-update", &payload);
            Ok(())
        }
        Err(e) => {
            log::warn!("Polling error: {}", e);
            let user_message = if e.contains("TOKEN_EXPIRED") || e.contains("Invalid token") {
                "Token expired. Update in Settings.".to_string()
            } else if e.contains("not configured") {
                e.clone()
            } else if e.contains("Connection failed") {
                "Connection failed. Check your network.".to_string()
            } else if e.contains("Rate limited") {
                "Rate limited by GitLab. Try again later.".to_string()
            } else {
                "Failed to update. Try again later.".to_string()
            };
            let _ = app.emit("connection-error", &user_message);
            Err(user_message)
        }
    }
}

pub fn start_polling_task(app: AppHandle) {
    if POLLING_ACTIVE.swap(true, Ordering::SeqCst) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        loop {
            if !POLLING_ACTIVE.load(Ordering::SeqCst) {
                break;
            }

            let poll_secs = {
                let store = app.store("settings.json").ok();
                store
                    .and_then(|s| s.get("poll_interval").and_then(|v| v.as_str().map(String::from)))
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30)
            };

            tokio::time::sleep(Duration::from_secs(poll_secs)).await;

            if !POLLING_ACTIVE.load(Ordering::SeqCst) {
                break;
            }

            let _ = run_check_cycle(&app, false).await;
        }
    });
}

#[tauri::command]
pub fn start_polling(app: AppHandle) -> Result<(), String> {
    start_polling_task(app);
    Ok(())
}

#[tauri::command]
pub fn stop_polling() -> Result<(), String> {
    POLLING_ACTIVE.store(false, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub async fn check_now(app: AppHandle) -> Result<(), String> {
    run_check_cycle(&app, true).await
}
