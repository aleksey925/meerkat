use crate::commands::gitlab::fetch_merge_requests;
use crate::models::{MergeRequest, MrUpdatePayload, PipelineStatus};
use crate::notifications;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;
use tokio::sync::Notify;
use tokio::time::Duration;

static PREVIOUS_MRS: Mutex<Option<Vec<MergeRequest>>> = Mutex::new(None);
// the single active polling task. starting a new one aborts the previous, so a
// connect/disconnect can never leave two loops running and a superseded loop
// cannot emit stale data (an aborted task is cancelled at its next await).
static POLL_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// wakes the polling loop early for a manual check_now (see start_polling).
static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);
// serializes connect/disconnect so the stop -> swap identity -> start sequence is
// never interleaved. without it a second lifecycle call could take the poll handle
// (see stop_polling) and mutate the identity while the first call's aborted cycle
// is still unwinding a write into the old account's store.
static LIFECYCLE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
// the most recent successful payload, served to a frontend that mounts after the
// first mr-update was already emitted so it shows data instead of "Loading...".
static LAST_PAYLOAD: Mutex<Option<MrUpdatePayload>> = Mutex::new(None);

// clears the in-memory snapshot so a freshly connected account does not diff its
// MRs against the previous account's and fire false notifications, and drops the
// cached payload so a reconnect never serves the old account's data on mount.
// per-account read-state and reminders live in their own files (see
// account_store_name), so nothing on disk needs clearing.
pub(crate) fn reset_previous_mrs() {
    if let Ok(mut prev) = PREVIOUS_MRS.lock() {
        *prev = None;
    }
    if let Ok(mut last) = LAST_PAYLOAD.lock() {
        *last = None;
    }
}

fn with_previous_mr<T>(mr_id: i64, f: impl FnOnce(&MergeRequest) -> T) -> Option<T> {
    let prev = PREVIOUS_MRS.lock().ok()?;
    let mr = prev.as_ref()?.iter().find(|m| m.id == mr_id)?;
    Some(f(mr))
}

pub(crate) fn previous_mr_updated_at_raw(mr_id: i64) -> Option<String> {
    with_previous_mr(mr_id, |m| m.updated_at_raw.clone())
}

pub(crate) fn previous_mr_pipeline_status(mr_id: i64) -> Option<PipelineStatus> {
    with_previous_mr(mr_id, |m| m.pipeline_status.clone())?
}

// previous (todo_id, requested_by) for an MR, used to carry the re-request
// forward when the todos fetch fails so a transient outage neither drops the
// re-request nor re-fires its notification on recovery
pub(crate) fn previous_mr_review_request(mr_id: i64) -> Option<(i64, String)> {
    with_previous_mr(mr_id, |m| {
        m.review_request_todo_id
            .map(|todo_id| (todo_id, m.review_request_by.clone().unwrap_or_default()))
    })?
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReminderDue {
    Due,
    Pending,
    Invalid,
}

fn reminder_due_state(at_str: &str, now: chrono::DateTime<Utc>) -> ReminderDue {
    match chrono::DateTime::parse_from_rfc3339(at_str) {
        Ok(dt) if dt.with_timezone(&Utc) <= now => ReminderDue::Due,
        Ok(_) => ReminderDue::Pending,
        Err(_) => ReminderDue::Invalid,
    }
}

fn check_and_fire_reminders(app: &AppHandle) {
    let Some(reminder_name) = crate::commands::system::account_store_name(
        app,
        crate::commands::reminders::REMINDERS_PREFIX,
    ) else {
        return;
    };
    let reminder_store = match app.store(reminder_name) {
        Ok(s) => s,
        Err(_) => return,
    };

    let now = Utc::now();
    let entries: Vec<(String, String)> = reminder_store
        .keys()
        .into_iter()
        .filter_map(|key| {
            let at_str =
                crate::commands::reminders::reminder_field(&reminder_store.get(&key)?, "at")?;
            Some((key, at_str))
        })
        .collect();

    let mut fired_ids: Vec<i64> = Vec::new();

    for (key, at_str) in entries {
        match reminder_due_state(&at_str, now) {
            ReminderDue::Pending => continue,
            ReminderDue::Invalid => {
                // unparseable value will never fire, clean it up
                reminder_store.delete(&key);
                let _ = reminder_store.save();
                continue;
            }
            ReminderDue::Due => {}
        }

        let mr_id: i64 = match key.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        let (title, prev_updated_at) =
            with_previous_mr(mr_id, |m| (m.title.clone(), m.updated_at_raw.clone()))
                .unwrap_or_else(|| (format!("MR #{mr_id}"), String::new()));

        notifications::notify_reminder(app, &title);

        // mark as unread, pinned by user (reminder is treated as a user action).
        // if PREVIOUS_MRS doesn't have the MR (e.g. first cycle of a fresh session),
        // preserve the updatedAt that's already in the store so the pin survives the
        // upcoming fetch.
        let read_store = crate::commands::system::account_store_name(
            app,
            crate::commands::system::READ_STATE_PREFIX,
        )
        .and_then(|name| app.store(name).ok());
        if let Some(read_store) = read_store {
            let updated_at = if !prev_updated_at.is_empty() {
                prev_updated_at
            } else {
                read_store
                    .get(&key)
                    .as_ref()
                    .and_then(crate::commands::system::StoredReadState::parse)
                    .and_then(|s| s.updated_at().map(String::from))
                    .unwrap_or_default()
            };
            let todo_id = crate::commands::system::read_review_request_todo_id(&read_store, &key);
            crate::commands::system::write_state(
                &read_store,
                &key,
                true,
                &updated_at,
                crate::commands::system::ReadStateSource::User,
                todo_id,
            );
            let _ = read_store.save();
        }

        reminder_store.delete(&key);
        let _ = reminder_store.save();

        fired_ids.push(mr_id);
    }

    if !fired_ids.is_empty() {
        let _ = app.emit("reminders-fired", &fired_ids);
    }
}

async fn run_check_cycle(app: &AppHandle) -> Result<(), String> {
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
            if let Ok(mut last) = LAST_PAYLOAD.lock() {
                *last = Some(payload.clone());
            }

            let _ = app.emit("mr-update", &payload);
            Ok(())
        }
        Err(e) => {
            log::warn!("Polling error: {e}");
            let user_message = user_facing_error(&e);
            let _ = app.emit("connection-error", &user_message);
            Err(user_message)
        }
    }
}

fn user_facing_error(raw: &str) -> String {
    if raw.contains("TOKEN_EXPIRED") || raw.contains("Invalid token") {
        "Token expired. Update in Settings.".to_string()
    } else if raw.contains("not configured") {
        raw.to_string()
    } else if raw.contains("Connection failed") {
        "Connection failed. Check your network.".to_string()
    } else if raw.contains("Rate limited") {
        "Rate limited by GitLab. Try again later.".to_string()
    } else {
        "Failed to update. Try again later.".to_string()
    }
}

fn poll_interval_secs(app: &AppHandle) -> u64 {
    app.store("settings.json")
        .ok()
        .and_then(|s| {
            s.get("poll_interval")
                .and_then(|v| v.as_str().map(String::from))
        })
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30)
}

fn take_poll_task() -> Option<JoinHandle<()>> {
    POLL_TASK.lock().unwrap_or_else(|e| e.into_inner()).take()
}

// held for the whole duration of a connect/disconnect, so the stop/swap/start
// sequence runs to completion before another lifecycle call can start.
pub(crate) async fn lock_lifecycle() -> tokio::sync::MutexGuard<'static, ()> {
    LIFECYCLE.lock().await
}

// starts the single polling task, aborting any previous one first.
pub(crate) fn start_polling(app: AppHandle) {
    if let Some(handle) = take_poll_task() {
        handle.abort();
    }
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            // run a cycle immediately on start, not just after the first
            // interval: this seeds PREVIOUS_MRS and populates the UI. the seeding
            // cycle has no previous snapshot, so it sets the baseline without
            // notifying for every existing MR.
            let _ = run_check_cycle(&app).await;

            let interval = Duration::from_secs(poll_interval_secs(&app));
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                // a manual check_now wakes the loop so the refresh reuses this
                // single fetch path instead of running a second concurrent one
                _ = WAKE.notified() => {}
            }
        }
    });
    *POLL_TASK.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
}

// aborts the poll task and waits for it to fully unwind. awaiting matters because
// abort only schedules cancellation: without the join, the caller could swap the
// identity while the old cycle is still mid-write into the old account's store.
pub(crate) async fn stop_polling() {
    if let Some(handle) = take_poll_task() {
        handle.abort();
        let _ = handle.await;
    }
}

#[tauri::command]
pub async fn check_now(app: AppHandle) {
    // hold LIFECYCLE so the connected-check and the wake are atomic against a
    // concurrent connect/disconnect. without it, a check landing in the stopped
    // window could see is_connected true, store a permit, and have the next poll
    // task consume it for one spurious immediate cycle.
    let _guard = lock_lifecycle().await;
    // ignore when disconnected: there is no loop to wake, and notify_one would
    // otherwise leave a permit that fires one extra cycle the moment polling starts.
    if crate::commands::settings::is_connected(&app) {
        WAKE.notify_one();
    }
}

#[tauri::command]
pub fn get_last_update() -> Option<MrUpdatePayload> {
    LAST_PAYLOAD.lock().ok().and_then(|p| p.clone())
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
        // arrange
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
        // arrange
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
        // arrange
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
        // arrange
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

    #[test]
    fn user_facing_error_maps_each_branch_with_precedence() {
        // assert
        assert_eq!(
            user_facing_error("TOKEN_EXPIRED"),
            "Token expired. Update in Settings."
        );
        assert_eq!(
            user_facing_error("Invalid token format"),
            "Token expired. Update in Settings."
        );
        assert_eq!(
            user_facing_error("GitLab URL or token not configured"),
            "GitLab URL or token not configured"
        );
        assert_eq!(
            user_facing_error("Connection failed: timeout"),
            "Connection failed. Check your network."
        );
        assert_eq!(
            user_facing_error("Rate limited by GitLab. Try again later."),
            "Rate limited by GitLab. Try again later."
        );
        assert_eq!(
            user_facing_error("GitLab API error: 503"),
            "Failed to update. Try again later."
        );
    }

    #[test]
    fn user_facing_error_token_branch_wins_over_connection() {
        // arrange
        let raw = "Connection failed: Invalid token";

        // act
        let msg = user_facing_error(raw);

        // assert
        assert_eq!(msg, "Token expired. Update in Settings.");
    }

    #[test]
    fn reminder_due_state_classifies_by_time_and_validity() {
        // arrange
        let now = chrono::DateTime::parse_from_rfc3339(T2)
            .unwrap()
            .with_timezone(&Utc);

        // assert
        assert_eq!(reminder_due_state(T1, now), ReminderDue::Due);
        assert_eq!(reminder_due_state(T2, now), ReminderDue::Due);
        assert_eq!(
            reminder_due_state("2099-01-01T00:00:00Z", now),
            ReminderDue::Pending
        );
        assert_eq!(reminder_due_state("not-a-date", now), ReminderDue::Invalid);
    }
}
