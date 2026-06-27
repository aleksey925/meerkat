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

pub(crate) fn previous_mr_updated_at_raw(mr_id: i64) -> Option<String> {
    let prev = PREVIOUS_MRS.lock().ok()?;
    let mrs = prev.as_ref()?;
    mrs.iter()
        .find(|m| m.id == mr_id)
        .map(|m| m.updated_at_raw.clone())
}

#[derive(Debug, Clone, PartialEq)]
enum NotifyAction {
    NewMr {
        author: String,
        title: String,
        project: String,
    },
    MrUpdated {
        author: String,
        title: String,
    },
    ReviewRequested {
        author: String,
        title: String,
    },
    PipelineFailed {
        title: String,
    },
}

fn compute_notifications(
    new_active: &[MergeRequest],
    previous: &[MergeRequest],
) -> Vec<NotifyAction> {
    let prev_map: HashMap<i64, &MergeRequest> = previous.iter().map(|m| (m.id, m)).collect();
    let mut actions = Vec::new();

    for mr in new_active {
        match prev_map.get(&mr.id) {
            None => {
                actions.push(NotifyAction::NewMr {
                    author: mr.author_name.clone(),
                    title: mr.title.clone(),
                    project: mr.project_name.clone(),
                });
            }
            Some(prev_mr) => {
                if mr.updated_at != prev_mr.updated_at {
                    if let Some(ref author) = mr.latest_actor {
                        actions.push(NotifyAction::MrUpdated {
                            author: author.clone(),
                            title: mr.title.clone(),
                        });
                    }
                }
                if mr.review_request_todo_id.is_some()
                    && mr.review_request_todo_id != prev_mr.review_request_todo_id
                {
                    let author = mr.review_request_by.as_deref().unwrap_or(&mr.author_name);
                    actions.push(NotifyAction::ReviewRequested {
                        author: author.to_string(),
                        title: mr.title.clone(),
                    });
                }
                if mr.pipeline_status == Some(PipelineStatus::Fail)
                    && prev_mr.pipeline_status != Some(PipelineStatus::Fail)
                {
                    actions.push(NotifyAction::PipelineFailed {
                        title: mr.title.clone(),
                    });
                }
            }
        }
    }

    actions
}

fn detect_changes_and_notify(
    app: &AppHandle,
    new_active: &[MergeRequest],
    previous: &[MergeRequest],
) {
    for action in compute_notifications(new_active, previous) {
        match action {
            NotifyAction::NewMr {
                author,
                title,
                project,
            } => notifications::notify_new_mr(app, &author, &title, &project),
            NotifyAction::MrUpdated { author, title } => {
                notifications::notify_mr_updated(app, &author, &title)
            }
            NotifyAction::ReviewRequested { author, title } => {
                notifications::notify_review_requested(app, &author, &title)
            }
            NotifyAction::PipelineFailed { title } => {
                notifications::notify_pipeline_failed(app, &title)
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

            let (title, prev_updated_at) = {
                let prev = PREVIOUS_MRS.lock().ok();
                let mr = prev
                    .as_ref()
                    .and_then(|opt| opt.as_ref())
                    .and_then(|mrs| mrs.iter().find(|m| m.id == mr_id).cloned());
                match mr {
                    Some(m) => (m.title, m.updated_at_raw),
                    None => (format!("MR #{}", mr_id), String::new()),
                }
            };

            notifications::notify_reminder(app, &title);

            // mark as unread, pinned by user (reminder is treated as a user action).
            // if PREVIOUS_MRS doesn't have the MR (e.g. first cycle of a fresh session),
            // preserve the updatedAt that's already in the store so the pin survives the
            // upcoming fetch.
            if let Ok(read_store) = app.store(crate::commands::system::READ_STATE_STORE) {
                let updated_at = if !prev_updated_at.is_empty() {
                    prev_updated_at
                } else {
                    read_store
                        .get(&key)
                        .and_then(|v| {
                            v.as_object().and_then(|o| {
                                o.get("updatedAt")
                                    .and_then(|u| u.as_str())
                                    .map(String::from)
                            })
                        })
                        .unwrap_or_default()
                };
                let todo_id =
                    crate::commands::system::read_review_request_todo_id(&read_store, &key);
                crate::commands::system::write_state(
                    &read_store,
                    &key,
                    true,
                    &updated_at,
                    "user",
                    todo_id,
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
                if let Some(prev_data) = prev.as_ref().and_then(|o| o.as_ref()) {
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
                    .and_then(|s| {
                        s.get("poll_interval")
                            .and_then(|v| v.as_str().map(String::from))
                    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MrStatus, UserRole};

    fn mr(id: i64, updated_at: &str) -> MergeRequest {
        MergeRequest {
            id,
            iid: id,
            project_id: 1,
            project_name: "proj".to_string(),
            project_namespace: "ns".to_string(),
            title: format!("MR {id}"),
            source_branch: "src".to_string(),
            target_branch: "main".to_string(),
            author_name: "Author".to_string(),
            author_username: "author".to_string(),
            role: UserRole::Reviewer,
            status: MrStatus::Open,
            draft: false,
            has_conflicts: false,
            pipeline_status: None,
            approvals_current: 0,
            approvals_required: 0,
            web_url: String::new(),
            updated_at: chrono::DateTime::parse_from_rfc3339(updated_at)
                .unwrap()
                .with_timezone(&Utc),
            unread: true,
            reminder: None,
            activity: Vec::new(),
            latest_actor: None,
            updated_at_raw: updated_at.to_string(),
            review_request_todo_id: None,
            review_request_by: None,
        }
    }

    const T1: &str = "2026-01-01T10:00:00Z";
    const T2: &str = "2026-01-02T10:00:00Z";

    #[test]
    fn new_mr_fires_new_mr_notification() {
        // arrange
        let new = mr(1, T1);

        // act
        let actions = compute_notifications(std::slice::from_ref(&new), &[]);

        // assert
        assert_eq!(
            actions,
            vec![NotifyAction::NewMr {
                author: new.author_name.clone(),
                title: new.title.clone(),
                project: new.project_name.clone(),
            }]
        );
    }

    #[test]
    fn updated_at_change_with_actor_fires_mr_updated() {
        // arrange
        let prev = mr(1, T1);
        let mut new = mr(1, T2);
        new.latest_actor = Some("Alice".to_string());

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert_eq!(
            actions,
            vec![NotifyAction::MrUpdated {
                author: new.latest_actor.clone().unwrap(),
                title: new.title.clone(),
            }]
        );
    }

    #[test]
    fn updated_at_change_without_actor_fires_nothing() {
        // arrange — own action: updated_at moved but no actor attributed
        let prev = mr(1, T1);
        let new = mr(1, T2);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert!(actions.is_empty());
    }

    #[test]
    fn new_review_request_todo_fires_review_requested() {
        // arrange
        let prev = mr(1, T1);
        let mut new = mr(1, T1);
        new.review_request_todo_id = Some(5);
        new.review_request_by = Some("Bob".to_string());

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert_eq!(
            actions,
            vec![NotifyAction::ReviewRequested {
                author: new.review_request_by.clone().unwrap(),
                title: new.title.clone(),
            }]
        );
    }

    #[test]
    fn review_request_falls_back_to_author_name() {
        // arrange
        let prev = mr(1, T1);
        let mut new = mr(1, T1);
        new.review_request_todo_id = Some(5);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert_eq!(
            actions,
            vec![NotifyAction::ReviewRequested {
                author: new.author_name.clone(),
                title: new.title.clone(),
            }]
        );
    }

    #[test]
    fn cleared_review_request_todo_fires_nothing() {
        // arrange — todo went Some(5) -> None: the is_some() guard suppresses it
        let mut prev = mr(1, T1);
        prev.review_request_todo_id = Some(5);
        let new = mr(1, T1);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert!(actions.is_empty());
    }

    #[test]
    fn unchanged_review_request_todo_fires_nothing() {
        // arrange
        let mut prev = mr(1, T1);
        prev.review_request_todo_id = Some(5);
        let mut new = mr(1, T1);
        new.review_request_todo_id = Some(5);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert!(actions.is_empty());
    }

    #[test]
    fn combined_update_and_new_review_request_fires_both() {
        // arrange — updated_at moved with an actor AND a fresh re-request todo
        let prev = mr(1, T1);
        let mut new = mr(1, T2);
        new.latest_actor = Some("Alice".to_string());
        new.review_request_todo_id = Some(9);
        new.review_request_by = Some("Bob".to_string());

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert_eq!(
            actions,
            vec![
                NotifyAction::MrUpdated {
                    author: new.latest_actor.clone().unwrap(),
                    title: new.title.clone(),
                },
                NotifyAction::ReviewRequested {
                    author: new.review_request_by.clone().unwrap(),
                    title: new.title.clone(),
                },
            ]
        );
    }

    #[test]
    fn pipeline_pass_to_fail_fires_pipeline_failed() {
        // arrange
        let mut prev = mr(1, T1);
        prev.pipeline_status = Some(PipelineStatus::Pass);
        let mut new = mr(1, T1);
        new.pipeline_status = Some(PipelineStatus::Fail);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert_eq!(
            actions,
            vec![NotifyAction::PipelineFailed {
                title: new.title.clone()
            }]
        );
    }

    #[test]
    fn pipeline_pending_to_fail_fires_pipeline_failed() {
        // arrange
        let mut prev = mr(1, T1);
        prev.pipeline_status = Some(PipelineStatus::Pending);
        let mut new = mr(1, T1);
        new.pipeline_status = Some(PipelineStatus::Fail);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert_eq!(
            actions,
            vec![NotifyAction::PipelineFailed {
                title: new.title.clone()
            }]
        );
    }

    #[test]
    fn pipeline_fail_to_fail_fires_nothing() {
        // arrange — already failing, no fresh transition
        let mut prev = mr(1, T1);
        prev.pipeline_status = Some(PipelineStatus::Fail);
        let mut new = mr(1, T1);
        new.pipeline_status = Some(PipelineStatus::Fail);

        // act
        let actions =
            compute_notifications(std::slice::from_ref(&new), std::slice::from_ref(&prev));

        // assert
        assert!(actions.is_empty());
    }
}
