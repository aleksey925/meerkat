use crate::commands::system::{encode_read_state, ReadStateSource, StoredReadState};
use crate::credentials;
use crate::models::*;
use chrono::Utc;
use reqwest::Client;
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;
use tokio::time::{sleep, Duration};

// activity-feed colors; ACCENT_COLOR mirrors the frontend `--accent` purple (see
// ACCENT_COLOR in src/App.jsx) and must be kept in sync across the boundary.
const ACCENT_COLOR: &str = "#5e5ce6";
const SYSTEM_EVENT_COLOR: &str = "#34c759";

fn build_client(token: &str) -> Result<Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "PRIVATE-TOKEN",
        reqwest::header::HeaderValue::from_str(token)
            .map_err(|e| format!("Invalid token format: {e}"))?,
    );
    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))
}

// statuses that signal a momentary server/proxy hiccup rather than a real
// failure - safe to retry the same GET
fn is_transient_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
}

// GET with one retry on transient failures (network errors and 408/429/5xx). a
// single blip would otherwise fail the whole poll and drop the app to offline; a
// short backoff lets a flaky request recover within the cycle. kept low on
// purpose - a persistently slow endpoint that 408s should fail fast and let the
// caller degrade, not stack up multi-minute hangs.
async fn gitlab_get(client: &Client, url: &str) -> Result<reqwest::Response, String> {
    const MAX_ATTEMPTS: u32 = 2;
    let mut attempt = 1;
    loop {
        let outcome = client.get(url).send().await;
        match &outcome {
            Ok(resp) if is_transient_status(resp.status()) => {
                log::warn!(
                    "transient {} from {} (attempt {attempt})",
                    resp.status(),
                    url
                )
            }
            Err(e) => log::warn!("request error from {} (attempt {attempt}): {e}", url),
            _ => {}
        }
        match outcome {
            Ok(resp) if is_transient_status(resp.status()) && attempt < MAX_ATTEMPTS => {}
            Ok(resp) => return Ok(resp),
            Err(e) if attempt >= MAX_ATTEMPTS => return Err(format!("API error: {e}")),
            Err(_) => {}
        }
        sleep(Duration::from_millis(500 * attempt as u64)).await;
        attempt += 1;
    }
}

struct Identity {
    url: String,
    token: String,
    user_id: i64,
    username: String,
}

// reads the (url, token, user_id, username) identity from settings + keychain.
// only `connect` writes the identity, and it does so while the poll task is
// stopped, so a read here never races a half-written identity.
fn load_identity(app: &AppHandle) -> Result<Identity, String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    let url = store
        .get("gitlab_url")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let token = credentials::get_token()?.unwrap_or_default();

    if url.is_empty() || token.is_empty() {
        return Err("GitLab URL or token not configured".to_string());
    }

    let user_id = store
        .get("user_id")
        .and_then(|v| v.as_i64())
        .ok_or("User ID not found. Connect first.")?;
    let username = store
        .get("username")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    Ok(Identity {
        url,
        token,
        user_id,
        username,
    })
}

// validates the token against the url, then commits the new identity and
// restarts polling. validation happens before any write, so a bad token leaves
// the existing identity untouched. the poll task is stopped before the write and
// started after, so no cycle ever runs against a half-applied identity or emits
// the previous account's data after the switch.
#[tauri::command]
pub async fn connect(app: AppHandle, url: String, token: String) -> Result<UserInfo, String> {
    let client = build_client(&token)?;
    let api_url = format!("{}/api/v4/user", url.trim_end_matches('/'));

    let resp = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;

    if resp.status() == 401 {
        return Err("Invalid token or token expired".to_string());
    }
    if resp.status() == 429 {
        return Err("Rate limited by GitLab. Try again later.".to_string());
    }
    if !resp.status().is_success() {
        return Err(format!("GitLab API error: {}", resp.status()));
    }

    let user: GitLabUser = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;

    crate::polling::stop_polling();

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    // write the validated token first; if persisting the identity then fails,
    // roll the token back so disk identity and keychain never disagree.
    let previous_token = credentials::get_token()?;
    credentials::store_token(&token)?;

    store.set("gitlab_url", serde_json::json!(url));
    store.set("user_id", serde_json::json!(user.id));
    store.set("username", serde_json::json!(user.username));
    if let Err(e) = store.save() {
        match previous_token {
            Some(prev) => {
                let _ = credentials::store_token(&prev);
            }
            None => {
                let _ = credentials::delete_token();
            }
        }
        return Err(format!("Save error: {e}"));
    }

    // the new account reads its own per-account stores, so only the in-memory
    // snapshot must be cleared; migrate any pre-namespacing data into this
    // account once.
    crate::polling::reset_previous_mrs();
    crate::commands::system::migrate_legacy_stores(&app);

    crate::polling::start_polling(app);

    Ok(UserInfo {
        id: user.id,
        username: user.username.clone(),
        name: user.name,
        avatar_url: user.avatar_url,
    })
}

async fn fetch_mrs_by_scope(
    client: &Client,
    base_url: &str,
    param: &str,
    uid: i64,
) -> Result<Vec<GitLabMr>, String> {
    let mut all = Vec::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "{}/api/v4/merge_requests?scope=all&{}={}&state=opened&per_page=100&page={}",
            base_url, param, uid, page
        );

        let resp = gitlab_get(client, &url).await?;

        if !resp.status().is_success() {
            if resp.status() == 401 {
                return Err("TOKEN_EXPIRED".to_string());
            }
            // a persistent non-success here must fail the cycle, not return a
            // truncated list: treating an outage as success would overwrite the
            // snapshot with an empty set, clear the UI, and fire a notification
            // storm once the endpoint recovers
            return Err(format!("GitLab API error: {}", resp.status()));
        }

        let next_page = resp
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        let mrs: Vec<GitLabMr> = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

        if mrs.is_empty() {
            break;
        }

        all.extend(mrs);

        match next_page {
            Some(np) if np > page => page = np,
            _ => break,
        }
    }

    Ok(all)
}

// Err distinguishes a failed fetch from Ok(None) "no pipeline". the caller
// carries forward the previous status on Err: collapsing a transient failure to
// None would make an already-failing pipeline read Fail -> None -> Fail across
// polls and fire a duplicate pipeline-failed notification on recovery.
async fn fetch_pipeline_status(
    client: &Client,
    base_url: &str,
    project_id: i64,
    mr_iid: i64,
) -> Result<Option<PipelineStatus>, String> {
    let url = format!(
        "{}/api/v4/projects/{}/merge_requests/{}/pipelines?per_page=1",
        base_url, project_id, mr_iid
    );

    let resp = gitlab_get(client, &url).await?;
    if !resp.status().is_success() {
        return Err(format!("GitLab API error: {}", resp.status()));
    }
    let pipelines: Vec<GitLabPipeline> =
        resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    Ok(pipelines.first().map(|p| match p.status.as_str() {
        "success" => PipelineStatus::Pass,
        "failed" => PipelineStatus::Fail,
        "running" => PipelineStatus::Running,
        _ => PipelineStatus::Pending,
    }))
}

struct ApprovalInfo {
    current: u32,
    required: u32,
    approved_by_me: bool,
}

async fn fetch_approvals(
    client: &Client,
    base_url: &str,
    project_id: i64,
    mr_iid: i64,
    current_uid: i64,
) -> ApprovalInfo {
    let url = format!(
        "{}/api/v4/projects/{}/merge_requests/{}/approvals",
        base_url, project_id, mr_iid
    );

    let resp = match gitlab_get(client, &url).await {
        Ok(r) => r,
        Err(_) => {
            return ApprovalInfo {
                current: 0,
                required: 0,
                approved_by_me: false,
            }
        }
    };

    let approvals: GitLabApprovals = match resp.json().await {
        Ok(a) => a,
        Err(_) => {
            return ApprovalInfo {
                current: 0,
                required: 0,
                approved_by_me: false,
            }
        }
    };

    let approved_by = approvals.approved_by.as_deref().unwrap_or_default();
    let current = approved_by.len() as u32;
    let required = approvals.approvals_required.unwrap_or(0) as u32;
    let approved_by_me = approved_by.iter().any(|a| a.user.id == current_uid);

    ApprovalInfo {
        current,
        required,
        approved_by_me,
    }
}

async fn fetch_reviewer_states(
    client: &Client,
    base_url: &str,
    project_id: i64,
    mr_iid: i64,
) -> Vec<GitLabReviewerState> {
    let mut all = Vec::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/reviewers?per_page=100&page={}",
            base_url, project_id, mr_iid, page
        );

        let resp = match gitlab_get(client, &url).await {
            Ok(r) => r,
            Err(_) => break,
        };

        if !resp.status().is_success() {
            break;
        }

        let next_page = resp
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        let reviewers: Vec<GitLabReviewerState> = match resp.json().await {
            Ok(r) => r,
            Err(_) => break,
        };

        if reviewers.is_empty() {
            break;
        }

        all.extend(reviewers);

        match next_page {
            Some(np) if np > page => page = np,
            _ => break,
        }
    }

    all
}

fn me_requested_changes(reviewers: &[GitLabReviewerState], current_uid: i64) -> bool {
    reviewers
        .iter()
        .any(|r| r.user.id == current_uid && r.state.as_deref() == Some("requested_changes"))
}

#[derive(Clone)]
struct ReviewRequest {
    todo_id: i64,
    by: String,
}

// pending "review requested" todos keyed by MR global id. a re-request doesn't
// move the MR's updated_at and leaves no note we can attribute, so the todo is
// the only reliable signal that the author asked me to review again.
async fn fetch_review_request_todos(
    client: &Client,
    base_url: &str,
) -> Result<HashMap<i64, ReviewRequest>, String> {
    let mut map = HashMap::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "{}/api/v4/todos?state=pending&action=review_requested&per_page=100&page={}",
            base_url, page
        );

        let resp = gitlab_get(client, &url).await?;

        if !resp.status().is_success() {
            if resp.status() == 401 {
                return Err("TOKEN_EXPIRED".to_string());
            }
            // distinguish a failed fetch from "no pending todos": on an empty map
            // a still-pending re-request would flip the MR read and then re-notify
            // once the endpoint recovers, so fail the cycle instead
            return Err(format!("GitLab API error: {}", resp.status()));
        }

        let next_page = resp
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        let todos: Vec<GitLabTodo> = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

        if todos.is_empty() {
            break;
        }

        for todo in todos {
            if todo.target_type == "MergeRequest" && todo.action_name == "review_requested" {
                map.insert(
                    todo.target.id,
                    ReviewRequest {
                        todo_id: todo.id,
                        by: todo.author.name,
                    },
                );
            }
        }

        match next_page {
            Some(np) if np > page => page = np,
            _ => break,
        }
    }

    Ok(map)
}

async fn fetch_notes(
    client: &Client,
    base_url: &str,
    project_id: i64,
    mr_iid: i64,
) -> Option<Vec<GitLabNote>> {
    let url = format!(
        "{}/api/v4/projects/{}/merge_requests/{}/notes?sort=desc&per_page=50",
        base_url, project_id, mr_iid
    );

    let resp = gitlab_get(client, &url).await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

async fn fetch_project_info(
    client: &Client,
    base_url: &str,
    project_id: i64,
) -> Option<GitLabProject> {
    let url = format!("{}/api/v4/projects/{}", base_url, project_id);
    let resp = gitlab_get(client, &url).await.ok()?;
    resp.json().await.ok()
}

fn determine_role(mr: &GitLabMr, uid: i64) -> UserRole {
    if mr
        .assignees
        .as_ref()
        .is_some_and(|a| a.iter().any(|u| u.id == uid))
        && !mr
            .reviewers
            .as_ref()
            .is_some_and(|r| r.iter().any(|u| u.id == uid))
    {
        return UserRole::Assignee;
    }
    // came from the reviewer or assignee scope query; default to reviewer when the
    // listed reviewers/assignees don't include the user (incomplete list payload)
    UserRole::Reviewer
}

fn determine_status(
    mr: &GitLabMr,
    approvals_current: u32,
    approvals_required: u32,
    i_requested_changes: bool,
) -> MrStatus {
    match mr.state.as_str() {
        "merged" => MrStatus::Merged,
        "closed" => MrStatus::Closed,
        _ => {
            if i_requested_changes {
                MrStatus::Changes
            } else if approvals_required > 0 && approvals_current >= approvals_required {
                MrStatus::Approved
            } else {
                MrStatus::Open
            }
        }
    }
}

fn color_for_project(index: usize) -> &'static str {
    const COLORS: &[&str] = &[
        ACCENT_COLOR,
        "#ff6482",
        "#30d158",
        "#ff9f0a",
        "#0a84ff",
        "#af52de",
        "#ff375f",
        "#64d2ff",
    ];
    COLORS[index % COLORS.len()]
}

fn make_initials(name: &str) -> String {
    name.split(['-', '_', ' '])
        .filter(|s| !s.is_empty())
        .take(2)
        .map(|s| s.chars().next().unwrap_or('?').to_uppercase().to_string())
        .collect()
}

fn notes_to_activity(notes: &[GitLabNote], mr: &GitLabMr) -> Vec<ActivityEvent> {
    let mut events = Vec::new();

    events.push(ActivityEvent {
        who: mr.author.username.clone(),
        text: format!("{} opened merge request", mr.author.name),
        time: mr.created_at.clone(),
        color: ACCENT_COLOR.to_string(),
    });

    for note in notes.iter().take(20).rev() {
        let is_system = note.system.unwrap_or(false);
        events.push(ActivityEvent {
            who: if is_system {
                "sys".to_string()
            } else {
                note.author.username.clone()
            },
            text: if is_system {
                note.body.clone()
            } else {
                format!("{} commented", note.author.name)
            },
            time: note.created_at.clone(),
            color: if is_system {
                SYSTEM_EVENT_COLOR.to_string()
            } else {
                ACCENT_COLOR.to_string()
            },
        });
    }

    events
}

fn find_latest_activity_from_others(
    notes: &[GitLabNote],
    current_username: &str,
    since: &str,
) -> Option<String> {
    // notes are sorted desc (newest first). only the single most recent note
    // counts: if my own comment is the latest activity, there is nothing to
    // attribute to others. skipping my note to surface an older one would let
    // my own comment fire a notification naming whoever spoke before me.
    let note = notes.first()?;
    if note.created_at.as_str() <= since || note.author.username == current_username {
        return None;
    }
    Some(note.author.name.clone())
}

// newest human comment from someone else, but only if it is newer than my own
// most recent action (comment, approval, review). used to wake an approved MR:
// a real message should reach me, while my own approval and automated system
// notes (CI, pushes, label changes) must not.
fn find_latest_comment_from_others(
    notes: &[GitLabNote],
    current_username: &str,
    since: &str,
) -> Option<String> {
    for note in notes {
        if note.created_at.as_str() <= since {
            break;
        }
        if note.author.username == current_username {
            // I acted more recently than any other comment here - up to date
            return None;
        }
        if !note.system.unwrap_or(false) {
            return Some(note.author.name.clone());
        }
        // a system note from someone else (push, label) - not a comment, keep
        // scanning older notes for an actual message
    }
    None
}

struct ReadStatus {
    unread: bool,
    latest_actor: Option<String>,
    source: ReadStateSource,
}

#[derive(Clone, Copy)]
struct ReadSignals {
    approved_by_me: bool,
    i_requested_changes: bool,
    review_request_todo_id: Option<i64>,
}

fn decide_unread_status(
    stored: Option<&StoredReadState>,
    updated_at: &str,
    notes: &[GitLabNote],
    current_username: &str,
    signals: ReadSignals,
) -> ReadStatus {
    let (stored_unread, stored_updated, stored_source, stored_todo) = match stored {
        Some(StoredReadState::Full {
            unread,
            updated_at,
            source,
            review_request_todo_id,
        }) => (
            Some(*unread),
            Some(updated_at.as_str()),
            Some(*source),
            *review_request_todo_id,
        ),
        Some(StoredReadState::LegacyBool(b)) => (Some(*b), None, None, None),
        None => (None, None, None, None),
    };

    // 1. user pinned the state and MR hasn't changed - respect it. the pin is
    // anchored to the review-request todo too: a fresh re-request keeps the same
    // updated_at and leaves no note, so without this a manually-read MR would
    // stay read while the re-request notification fires (read state and todo
    // would disagree).
    if let (Some(unread), Some(stored_ts), Some(ReadStateSource::User)) =
        (stored_unread, stored_updated, stored_source)
    {
        if stored_ts == updated_at && stored_todo == signals.review_request_todo_id {
            return ReadStatus {
                unread,
                latest_actor: None,
                source: ReadStateSource::User,
            };
        }
    }

    // 2. a re-request review is pending for me - unread. sits above approval
    // because a re-request means the author wants another look even if a stale
    // approval is still on record. notification is driven separately by the
    // todo id (re-request leaves no note and doesn't move updated_at).
    if signals.review_request_todo_id.is_some() {
        return ReadStatus {
            unread: true,
            latest_actor: None,
            source: ReadStateSource::Auto,
        };
    }

    // 3. someone else left a real comment since I last looked - unread + notify,
    // even if I have approved. approval should silence CI/push noise, not a human
    // message; pushes are handled below because GitLab resets my approval on push.
    if let Some(stored_ts) = stored_updated {
        if stored_ts != updated_at {
            if let Some(actor) = find_latest_comment_from_others(notes, current_username, stored_ts)
            {
                return ReadStatus {
                    unread: true,
                    latest_actor: Some(actor),
                    source: ReadStateSource::Auto,
                };
            }
        }
    }

    // 4. active approval from me - auto-read (GitLab resets approval on push,
    // so the next "real" activity will fall through to step 5 naturally)
    if signals.approved_by_me {
        return ReadStatus {
            unread: false,
            latest_actor: None,
            source: ReadStateSource::Auto,
        };
    }

    // 5. MR changed since last fetch and someone else acted - auto-unread
    if let Some(stored_ts) = stored_updated {
        if stored_ts != updated_at {
            let latest_actor = find_latest_activity_from_others(notes, current_username, stored_ts);
            if latest_actor.is_some() {
                return ReadStatus {
                    unread: true,
                    latest_actor,
                    source: ReadStateSource::Auto,
                };
            }
        }
    }

    // 6. I left a "request changes" review - auto-read. Sits below activity-from-others
    // so any reaction from the author (push, comment) flips it back to unread for the
    // next round of the fix cycle.
    if signals.i_requested_changes {
        return ReadStatus {
            unread: false,
            latest_actor: None,
            source: ReadStateSource::Auto,
        };
    }

    // 7. fallback - keep stored, default to unread for new MRs
    ReadStatus {
        unread: stored_unread.unwrap_or(true),
        latest_actor: None,
        source: ReadStateSource::Auto,
    }
}

fn resolve_unread_status(
    app: &AppHandle,
    mr_id: i64,
    updated_at: &str,
    notes: &[GitLabNote],
    current_username: &str,
    signals: ReadSignals,
) -> ReadStatus {
    let read_store = crate::commands::system::account_store_name(
        app,
        crate::commands::system::READ_STATE_PREFIX,
    )
    .and_then(|name| app.store(name).ok());
    let key = mr_id.to_string();
    let stored = read_store
        .as_ref()
        .and_then(|s| s.get(&key))
        .as_ref()
        .and_then(StoredReadState::parse);
    decide_unread_status(
        stored.as_ref(),
        updated_at,
        notes,
        current_username,
        signals,
    )
}

fn resolve_reminder(app: &AppHandle, mr_id: i64) -> Option<String> {
    let name = crate::commands::system::account_store_name(
        app,
        crate::commands::reminders::REMINDERS_PREFIX,
    )?;
    let store = app.store(name).ok()?;
    let val = store.get(mr_id.to_string())?;
    crate::commands::reminders::reminder_field(&val, "label")
}

fn persist_read_state(
    app: &AppHandle,
    mr_id: i64,
    unread: bool,
    updated_at: &str,
    source: ReadStateSource,
    review_request_todo_id: Option<i64>,
) {
    let Some(name) = crate::commands::system::account_store_name(
        app,
        crate::commands::system::READ_STATE_PREFIX,
    ) else {
        return;
    };
    if let Ok(store) = app.store(name) {
        store.set(
            mr_id.to_string(),
            encode_read_state(unread, updated_at, source, review_request_todo_id),
        );
        let _ = store.save();
    }
}

fn dedup_by_id(mrs: impl IntoIterator<Item = GitLabMr>) -> Vec<GitLabMr> {
    let mut seen = HashSet::new();
    mrs.into_iter().filter(|mr| seen.insert(mr.id)).collect()
}

async fn fetch_project_cache(
    client: &Client,
    base_url: &str,
    mrs: &[GitLabMr],
) -> HashMap<i64, GitLabProject> {
    let project_ids: HashSet<i64> = mrs.iter().map(|m| m.project_id).collect();
    let mut cache = HashMap::new();
    for pid in project_ids {
        if let Some(proj) = fetch_project_info(client, base_url, pid).await {
            cache.insert(pid, proj);
        }
    }
    cache
}

fn build_projects(project_cache: &HashMap<i64, GitLabProject>) -> Vec<Project> {
    // sort before assigning colors: HashMap iteration order is nondeterministic,
    // so coloring by iteration index would shuffle project colors between runs
    let mut entries: Vec<(&i64, &GitLabProject)> = project_cache.iter().collect();
    entries.sort_by(|a, b| a.1.name.cmp(&b.1.name).then(a.0.cmp(b.0)));
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (id, proj))| Project {
            id: *id,
            name: proj.name.clone(),
            namespace: proj.namespace.name.clone(),
            color: color_for_project(i).to_string(),
            initials: make_initials(&proj.name),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn build_merge_request(
    app: &AppHandle,
    client: &Client,
    base_url: &str,
    uid: i64,
    username: &str,
    gl_mr: &GitLabMr,
    project_cache: &HashMap<i64, GitLabProject>,
    review_request: Option<ReviewRequest>,
) -> MergeRequest {
    let review_request = review_request.as_ref();
    let fetched_notes = fetch_notes(client, base_url, gl_mr.project_id, gl_mr.iid).await;
    let notes_ok = fetched_notes.is_some();
    let notes = fetched_notes.unwrap_or_default();
    let approval_info = fetch_approvals(client, base_url, gl_mr.project_id, gl_mr.iid, uid).await;
    let pipeline = fetch_pipeline_status(client, base_url, gl_mr.project_id, gl_mr.iid)
        .await
        .unwrap_or_else(|_| crate::polling::previous_mr_pipeline_status(gl_mr.id));
    let reviewer_states =
        fetch_reviewer_states(client, base_url, gl_mr.project_id, gl_mr.iid).await;
    let requested_changes = me_requested_changes(&reviewer_states, uid);
    let role = determine_role(gl_mr, uid);
    let status = determine_status(
        gl_mr,
        approval_info.current,
        approval_info.required,
        requested_changes,
    );
    let is_draft = gl_mr.draft.unwrap_or(false) || gl_mr.work_in_progress.unwrap_or(false);

    let proj = project_cache.get(&gl_mr.project_id);
    let project_name = proj.map(|p| p.name.clone()).unwrap_or_default();
    let project_namespace = proj.map(|p| p.namespace.name.clone()).unwrap_or_default();

    let activity = notes_to_activity(&notes, gl_mr);

    let read_status = resolve_unread_status(
        app,
        gl_mr.id,
        &gl_mr.updated_at,
        &notes,
        username,
        ReadSignals {
            approved_by_me: approval_info.approved_by_me,
            i_requested_changes: requested_changes,
            review_request_todo_id: review_request.map(|r| r.todo_id),
        },
    );
    let reminder = resolve_reminder(app, gl_mr.id);

    // when notes failed, carry forward the previous snapshot's updated_at so the
    // snapshot does not advance past the undetected change: keeping the new
    // updated_at would make compute_notifications see it as unchanged on recovery
    // and never fire the missed comment. fall back to the new value only when
    // there is no previous snapshot (a brand-new MR notifies via NewMr anyway)
    let updated_at_raw = if notes_ok {
        gl_mr.updated_at.clone()
    } else {
        crate::polling::previous_mr_updated_at_raw(gl_mr.id)
            .unwrap_or_else(|| gl_mr.updated_at.clone())
    };
    let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_raw)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    // skip persisting when notes could not be fetched: advancing the anchor to
    // the new updated_at would permanently swallow an unseen comment
    if notes_ok {
        persist_read_state(
            app,
            gl_mr.id,
            read_status.unread,
            &gl_mr.updated_at,
            read_status.source,
            review_request.map(|r| r.todo_id),
        );
    }

    MergeRequest {
        id: gl_mr.id,
        iid: gl_mr.iid,
        project_id: gl_mr.project_id,
        project_name,
        project_namespace,
        title: gl_mr.title.clone(),
        source_branch: gl_mr.source_branch.clone(),
        target_branch: gl_mr.target_branch.clone(),
        author_name: gl_mr.author.name.clone(),
        author_username: gl_mr.author.username.clone(),
        role,
        status,
        draft: is_draft,
        has_conflicts: gl_mr.has_conflicts.unwrap_or(false),
        pipeline_status: pipeline,
        approvals_current: approval_info.current,
        approvals_required: approval_info.required,
        web_url: gl_mr.web_url.clone(),
        updated_at,
        unread: read_status.unread,
        reminder,
        activity,
        latest_actor: read_status.latest_actor,
        updated_at_raw,
        review_request_todo_id: review_request.map(|r| r.todo_id),
        review_request_by: review_request.map(|r| r.by.clone()),
    }
}

pub(crate) async fn fetch_merge_requests(app: AppHandle) -> Result<MrUpdatePayload, String> {
    let identity = load_identity(&app)?;
    let base_url = identity.url.trim_end_matches('/').to_string();
    let client = build_client(&identity.token)?;
    let uid = identity.user_id;
    let username = identity.username;

    let (reviewer_mrs, assignee_mrs) = tokio::join!(
        fetch_mrs_by_scope(&client, &base_url, "reviewer_id", uid),
        fetch_mrs_by_scope(&client, &base_url, "assignee_id", uid),
    );

    let all_gitlab_mrs = dedup_by_id(reviewer_mrs?.into_iter().chain(assignee_mrs?));

    let project_cache = fetch_project_cache(&client, &base_url, &all_gitlab_mrs).await;
    let projects = build_projects(&project_cache);

    // a failed todos fetch must not take the whole app offline: re-request
    // detection is best-effort. on failure carry each MR's previous re-request
    // forward so a transient outage neither drops it nor re-fires the
    // notification once the endpoint recovers
    let review_request_todos = fetch_review_request_todos(&client, &base_url).await;
    let todos_ok = review_request_todos.is_ok();
    let review_request_todos = review_request_todos.unwrap_or_default();

    let mut active = Vec::new();
    for gl_mr in &all_gitlab_mrs {
        let review_request = if todos_ok {
            review_request_todos.get(&gl_mr.id).cloned()
        } else {
            crate::polling::previous_mr_review_request(gl_mr.id)
                .map(|(todo_id, by)| ReviewRequest { todo_id, by })
        };
        let mr = build_merge_request(
            &app,
            &client,
            &base_url,
            uid,
            &username,
            gl_mr,
            &project_cache,
            review_request,
        )
        .await;
        active.push(mr);
    }

    active.sort_by_key(|m| std::cmp::Reverse(m.updated_at));

    Ok(MrUpdatePayload { active, projects })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me";
    const T1: &str = "2026-01-01T10:00:00Z";
    const T2: &str = "2026-01-02T10:00:00Z";
    const T3: &str = "2026-01-03T10:00:00Z";

    fn note(username: &str, created_at: &str) -> GitLabNote {
        GitLabNote {
            body: String::new(),
            author: GitLabUser {
                id: 1,
                username: username.to_string(),
                name: format!("Name {username}"),
                avatar_url: String::new(),
            },
            created_at: created_at.to_string(),
            system: None,
        }
    }

    fn system_note(username: &str, created_at: &str) -> GitLabNote {
        GitLabNote {
            system: Some(true),
            ..note(username, created_at)
        }
    }

    fn full(unread: bool, updated_at: &str, source: ReadStateSource) -> StoredReadState {
        StoredReadState::Full {
            unread,
            updated_at: updated_at.to_string(),
            source,
            review_request_todo_id: None,
        }
    }

    fn full_todo(
        unread: bool,
        updated_at: &str,
        source: ReadStateSource,
        review_request_todo_id: Option<i64>,
    ) -> StoredReadState {
        StoredReadState::Full {
            unread,
            updated_at: updated_at.to_string(),
            source,
            review_request_todo_id,
        }
    }

    fn decide(
        stored: Option<&StoredReadState>,
        updated_at: &str,
        notes: &[GitLabNote],
        approved_by_me: bool,
    ) -> ReadStatus {
        decide_unread_status(
            stored,
            updated_at,
            notes,
            ME,
            ReadSignals {
                approved_by_me,
                i_requested_changes: false,
                review_request_todo_id: None,
            },
        )
    }

    fn decide_with_rc(
        stored: Option<&StoredReadState>,
        updated_at: &str,
        notes: &[GitLabNote],
        approved_by_me: bool,
        i_requested_changes: bool,
    ) -> ReadStatus {
        decide_unread_status(
            stored,
            updated_at,
            notes,
            ME,
            ReadSignals {
                approved_by_me,
                i_requested_changes,
                review_request_todo_id: None,
            },
        )
    }

    fn decide_with_review_request(
        stored: Option<&StoredReadState>,
        updated_at: &str,
        approved_by_me: bool,
        review_request_todo_id: Option<i64>,
    ) -> ReadStatus {
        decide_unread_status(
            stored,
            updated_at,
            &[],
            ME,
            ReadSignals {
                approved_by_me,
                i_requested_changes: false,
                review_request_todo_id,
            },
        )
    }

    fn gl_mr() -> GitLabMr {
        GitLabMr {
            id: 1,
            iid: 1,
            title: "Title".to_string(),
            source_branch: "src".to_string(),
            target_branch: "main".to_string(),
            author: GitLabUser {
                id: 99,
                username: "author".to_string(),
                name: "Author Name".to_string(),
                avatar_url: String::new(),
            },
            reviewers: None,
            assignees: None,
            state: "opened".to_string(),
            draft: None,
            work_in_progress: None,
            has_conflicts: None,
            web_url: String::new(),
            created_at: T1.to_string(),
            updated_at: T3.to_string(),
            project_id: 1,
        }
    }

    fn gl_user(id: i64, username: &str) -> GitLabUser {
        GitLabUser {
            id,
            username: username.to_string(),
            name: format!("Name {username}"),
            avatar_url: String::new(),
        }
    }

    fn gl_mr_roles(
        reviewers: Option<Vec<GitLabUser>>,
        assignees: Option<Vec<GitLabUser>>,
        state: &str,
    ) -> GitLabMr {
        GitLabMr {
            reviewers,
            assignees,
            state: state.to_string(),
            updated_at: T1.to_string(),
            ..gl_mr()
        }
    }

    fn reviewer(uid: i64, state: Option<&str>) -> GitLabReviewerState {
        GitLabReviewerState {
            user: GitLabUser {
                id: uid,
                username: format!("user{}", uid),
                name: format!("User {}", uid),
                avatar_url: String::new(),
            },
            state: state.map(String::from),
        }
    }

    // --- find_latest_activity_from_others ---

    #[test]
    fn find_latest_activity_from_others_empty_notes_returns_none() {
        // act
        let actor = find_latest_activity_from_others(&[], ME, T1);

        // assert
        assert!(actor.is_none());
    }

    #[test]
    fn find_latest_activity_from_others_only_my_notes_returns_none() {
        // arrange
        let notes = vec![note(ME, T3), note(ME, T2)];

        // act
        let actor = find_latest_activity_from_others(&notes, ME, T1);

        // assert
        assert!(actor.is_none());
    }

    #[test]
    fn find_latest_activity_from_others_returns_none_when_my_note_is_newest() {
        // arrange
        let notes = vec![note(ME, T3), note("alice", T2)];

        // act
        let actor = find_latest_activity_from_others(&notes, ME, T1);

        // assert
        assert!(actor.is_none());
    }

    #[test]
    fn find_latest_activity_from_others_skips_notes_at_or_before_since() {
        // arrange
        let notes = vec![note("alice", T1)];

        // act
        let actor = find_latest_activity_from_others(&notes, ME, T1);

        // assert
        assert!(actor.is_none());
    }

    #[test]
    fn find_latest_activity_from_others_picks_most_recent_when_multiple_others() {
        // arrange
        let newest_other = note("alice", T3);
        let older_other = note("bob", T2);
        let notes = vec![newest_other.clone(), older_other];

        // act
        let actor = find_latest_activity_from_others(&notes, ME, T1);

        // assert
        assert_eq!(actor, Some(newest_other.author.name));
    }

    // --- find_latest_comment_from_others ---

    #[test]
    fn find_latest_comment_returns_human_comment_from_others() {
        // arrange
        let other = note("alice", T2);

        // act
        let actor = find_latest_comment_from_others(std::slice::from_ref(&other), ME, T1);

        // assert
        assert_eq!(actor, Some(other.author.name));
    }

    #[test]
    fn find_latest_comment_skips_others_system_note_for_older_comment() {
        // arrange
        let comment = note("alice", T2);
        let notes = vec![system_note("alice", T3), comment.clone()];

        // act
        let actor = find_latest_comment_from_others(&notes, ME, T1);

        // assert
        assert_eq!(actor, Some(comment.author.name));
    }

    #[test]
    fn find_latest_comment_returns_none_when_only_others_system_notes() {
        // arrange
        let notes = vec![system_note("alice", T3)];

        // act
        let actor = find_latest_comment_from_others(&notes, ME, T1);

        // assert
        assert!(actor.is_none());
    }

    #[test]
    fn find_latest_comment_returns_none_when_my_action_is_newest() {
        // arrange
        let notes = vec![system_note(ME, T3), note("alice", T2)];

        // act
        let actor = find_latest_comment_from_others(&notes, ME, T1);

        // assert
        assert!(actor.is_none());
    }

    #[test]
    fn find_latest_comment_returns_none_at_or_before_since() {
        // arrange
        let notes = vec![note("alice", T1)];

        // act
        let actor = find_latest_comment_from_others(&notes, ME, T1);

        // assert
        assert!(actor.is_none());
    }

    // --- decide_unread_status: new MR ---

    #[test]
    fn new_mr_not_approved_is_unread_auto() {
        // act
        let r = decide(None, T1, &[], false);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn new_mr_approved_is_read_auto() {
        // act
        let r = decide(None, T1, &[], true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: user pin ---

    #[test]
    fn user_pin_unchanged_mr_respected_even_when_approved() {
        // arrange
        let stored = full(true, T1, ReadStateSource::User);

        // act
        let r = decide(Some(&stored), T1, &[note("alice", T2)], true);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn user_pin_to_read_unchanged_mr_respected() {
        // arrange
        let stored = full(false, T1, ReadStateSource::User);

        // act
        let r = decide(Some(&stored), T1, &[note("alice", T2)], false);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn user_pin_invalidated_then_approval_marks_read() {
        // arrange
        let stored = full(true, T1, ReadStateSource::User);

        // act
        let r = decide(Some(&stored), T2, &[], true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_invalidated_then_others_activity_marks_unread() {
        // arrange
        let stored = full(false, T1, ReadStateSource::User);
        let other = note("alice", T2);

        // act
        let r = decide(Some(&stored), T2, std::slice::from_ref(&other), false);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn user_pin_invalidated_with_only_my_activity_keeps_stored_unread() {
        // arrange
        let stored = full(false, T1, ReadStateSource::User);

        // act
        let r = decide(Some(&stored), T2, &[note(ME, T2)], false);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: approval auto-mark ---

    #[test]
    fn auto_state_unchanged_mr_with_approval_marks_read() {
        // arrange
        let stored = full(true, T1, ReadStateSource::Auto);

        // act
        let r = decide(Some(&stored), T1, &[], true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn approval_keeps_read_when_only_system_notes_follow() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);

        // act
        let r = decide(Some(&stored), T2, &[system_note("alice", T2)], true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn approval_does_not_silence_human_comment_from_others() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);
        let other = note("alice", T2);

        // act
        let r = decide(Some(&stored), T2, std::slice::from_ref(&other), true);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn approval_then_my_own_comment_stays_read() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);
        let notes = [note(ME, T3), system_note(ME, T2)];

        // act
        let r = decide(Some(&stored), T3, &notes, true);

        // assert
        assert!(!r.unread);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn approval_with_others_comment_before_my_action_stays_read() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);
        let notes = [system_note(ME, T3), note("alice", T2)];

        // act
        let r = decide(Some(&stored), T3, &notes, true);

        // assert
        assert!(!r.unread);
        assert!(r.latest_actor.is_none());
    }

    // --- decide_unread_status: activity from others ---

    #[test]
    fn auto_state_changed_mr_with_others_activity_marks_unread() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);
        let other = note("alice", T2);

        // act
        let r = decide(Some(&stored), T2, std::slice::from_ref(&other), false);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn auto_state_changed_mr_with_only_my_activity_keeps_stored_unread() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);

        // act
        let r = decide(Some(&stored), T2, &[note(ME, T2)], false);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn my_comment_after_others_note_does_not_set_actor() {
        // arrange
        let stored = full(true, T1, ReadStateSource::User);

        // act
        let r = decide(Some(&stored), T3, &[note(ME, T3), note("alice", T2)], false);

        // assert
        assert!(r.unread);
        assert!(r.latest_actor.is_none());
    }

    // --- decide_unread_status: unchanged MR without pin/approval ---

    #[test]
    fn auto_state_unchanged_mr_no_approval_keeps_stored_unread() {
        // arrange
        let stored = full(true, T1, ReadStateSource::Auto);

        // act
        let r = decide(Some(&stored), T1, &[note("alice", T2)], false);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: legacy bool ---

    #[test]
    fn legacy_bool_unread_true_not_approved_stays_unread() {
        // arrange
        let stored = StoredReadState::LegacyBool(true);

        // act
        let r = decide(Some(&stored), T1, &[], false);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn legacy_bool_unread_false_not_approved_stays_read() {
        // arrange
        let stored = StoredReadState::LegacyBool(false);

        // act
        let r = decide(Some(&stored), T1, &[], false);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn legacy_bool_with_approval_marks_read() {
        // arrange
        let stored = StoredReadState::LegacyBool(true);

        // act
        let r = decide(Some(&stored), T1, &[], true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: requested_changes by me ---

    #[test]
    fn new_mr_with_my_requested_changes_is_read_auto() {
        // act
        let r = decide_with_rc(None, T1, &[], false, true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn auto_state_unchanged_mr_with_my_requested_changes_marks_read() {
        // arrange
        let stored = full(true, T1, ReadStateSource::Auto);

        // act
        let r = decide_with_rc(Some(&stored), T1, &[], false, true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn auto_state_changed_mr_with_my_requested_changes_and_others_activity_marks_unread() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);
        let other = note("alice", T2);

        // act
        let r = decide_with_rc(Some(&stored), T2, std::slice::from_ref(&other), false, true);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn auto_state_changed_mr_with_my_requested_changes_and_only_my_activity_marks_read() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);

        // act
        let r = decide_with_rc(Some(&stored), T2, &[note(ME, T2)], false, true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn approval_takes_precedence_over_requested_changes() {
        // act
        let r = decide_with_rc(None, T1, &[], true, true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_unchanged_mr_with_my_requested_changes_respects_pin() {
        // arrange
        let stored = full(true, T1, ReadStateSource::User);

        // act
        let r = decide_with_rc(Some(&stored), T1, &[], false, true);

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn legacy_bool_with_my_requested_changes_marks_read() {
        // arrange
        let stored = StoredReadState::LegacyBool(true);

        // act
        let r = decide_with_rc(Some(&stored), T1, &[], false, true);

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: pending re-request review ---

    #[test]
    fn review_request_pending_marks_unread() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);

        // act
        let r = decide_with_review_request(Some(&stored), T1, false, Some(1));

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn review_request_overrides_active_approval() {
        // arrange
        let stored = full(false, T1, ReadStateSource::Auto);

        // act
        let r = decide_with_review_request(Some(&stored), T1, true, Some(1));

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_unchanged_mr_with_same_review_request_respects_pin() {
        // arrange
        let stored = full_todo(false, T1, ReadStateSource::User, Some(7));

        // act
        let r = decide_with_review_request(Some(&stored), T1, false, Some(7));

        // assert
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn user_pin_broken_by_new_review_request() {
        // arrange
        let stored = full_todo(false, T1, ReadStateSource::User, None);

        // act
        let r = decide_with_review_request(Some(&stored), T1, false, Some(9));

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_broken_by_changed_review_request_todo() {
        // arrange
        let stored = full_todo(false, T1, ReadStateSource::User, Some(7));

        // act
        let r = decide_with_review_request(Some(&stored), T1, false, Some(9));

        // assert
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- me_requested_changes helper ---

    #[test]
    fn me_requested_changes_returns_true_when_my_state_is_requested_changes() {
        // arrange
        let reviewers = vec![
            reviewer(2, Some("approved")),
            reviewer(1, Some("requested_changes")),
        ];

        // assert
        assert!(me_requested_changes(&reviewers, 1));
    }

    #[test]
    fn me_requested_changes_returns_false_for_other_states() {
        for state in ["unreviewed", "reviewed", "approved"] {
            // arrange
            let reviewers = vec![reviewer(1, Some(state))];

            // assert
            assert!(!me_requested_changes(&reviewers, 1));
        }
    }

    #[test]
    fn me_requested_changes_returns_false_when_state_missing() {
        // arrange
        let reviewers = vec![reviewer(1, None)];

        // assert
        assert!(!me_requested_changes(&reviewers, 1));
    }

    #[test]
    fn me_requested_changes_ignores_other_reviewers_state() {
        // arrange
        let reviewers = vec![reviewer(2, Some("requested_changes"))];

        // assert
        assert!(!me_requested_changes(&reviewers, 1));
    }

    #[test]
    fn me_requested_changes_returns_false_for_empty_reviewers() {
        // assert
        assert!(!me_requested_changes(&[], 1));
    }

    #[test]
    fn reviewer_states_deserialize_from_real_gitlab_response() {
        // arrange
        let body = serde_json::json!([
            {
                "user": {
                    "id": 1,
                    "name": "John Doe",
                    "username": "jdoe",
                    "state": "active",
                    "avatar_url": "https://example.com/avatar.png",
                    "web_url": "https://gitlab.example.com/jdoe"
                },
                "state": "requested_changes",
                "created_at": "2020-10-06T12:34:56.000Z"
            },
            {
                "user": {
                    "id": 2,
                    "name": "Jane Roe",
                    "username": "jroe",
                    "state": "active",
                    "avatar_url": "",
                    "web_url": "https://gitlab.example.com/jroe"
                },
                "state": "unreviewed",
                "created_at": "2020-10-06T12:34:56.000Z"
            }
        ]);

        // act
        let reviewers: Vec<GitLabReviewerState> = serde_json::from_value(body).unwrap();

        // assert
        assert!(me_requested_changes(&reviewers, 1));
        assert!(!me_requested_changes(&reviewers, 2));
    }

    #[test]
    fn todos_deserialize_from_real_gitlab_response() {
        // arrange
        let body = serde_json::json!([
            {
                "id": 719314370,
                "action_name": "review_requested",
                "target_type": "MergeRequest",
                "target": { "id": 500732801, "iid": 1, "title": "wip" },
                "author": {
                    "id": 2,
                    "name": "temp temp",
                    "username": "temp925",
                    "state": "active",
                    "avatar_url": ""
                },
                "state": "pending"
            }
        ]);

        // act
        let todos: Vec<GitLabTodo> = serde_json::from_value(body).unwrap();

        // assert
        assert_eq!(todos.len(), 1);
        let todo = &todos[0];
        assert_eq!(todo.id, 719314370);
        assert_eq!(todo.action_name, "review_requested");
        assert_eq!(todo.target_type, "MergeRequest");
        assert_eq!(todo.target.id, 500732801);
        assert_eq!(todo.author.name, "temp temp");
    }

    // --- notes_to_activity ---

    #[test]
    fn notes_to_activity_starts_with_synthetic_opened_event() {
        // arrange
        let mr = gl_mr();

        // act
        let events = notes_to_activity(&[], &mr);

        // assert
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].who, mr.author.username);
        assert_eq!(events[0].time, mr.created_at);
        assert!(events[0].text.contains("opened"));
    }

    #[test]
    fn notes_to_activity_keeps_newest_20_notes_in_chronological_order() {
        // arrange
        let notes: Vec<GitLabNote> = (0..25)
            .map(|i| note("alice", &format!("2026-03-{:02}T10:00:00Z", 25 - i)))
            .collect();
        let mr = gl_mr();

        // act
        let events = notes_to_activity(&notes, &mr);

        // assert
        let note_times: Vec<String> = events[1..].iter().map(|e| e.time.clone()).collect();
        let expected: Vec<String> = (6..=25)
            .map(|d| format!("2026-03-{:02}T10:00:00Z", d))
            .collect();
        assert_eq!(note_times, expected);
    }

    #[test]
    fn determine_role_reviewer_wins_over_assignee() {
        // arrange
        let mr = gl_mr_roles(
            Some(vec![gl_user(1, "me")]),
            Some(vec![gl_user(1, "me")]),
            "opened",
        );

        // assert
        assert_eq!(determine_role(&mr, 1), UserRole::Reviewer);
    }

    #[test]
    fn determine_role_assignee_when_not_reviewer() {
        // arrange
        let mr = gl_mr_roles(
            Some(vec![gl_user(2, "other")]),
            Some(vec![gl_user(1, "me")]),
            "opened",
        );

        // assert
        assert_eq!(determine_role(&mr, 1), UserRole::Assignee);
    }

    #[test]
    fn determine_role_falls_back_to_reviewer() {
        // arrange
        let mr = gl_mr_roles(Some(vec![gl_user(2, "other")]), None, "opened");

        // assert
        assert_eq!(determine_role(&mr, 1), UserRole::Reviewer);
    }

    #[test]
    fn determine_status_reflects_state_and_approvals() {
        // assert
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "merged"), 0, 0, false),
            MrStatus::Merged
        );
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "closed"), 0, 0, false),
            MrStatus::Closed
        );
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "opened"), 2, 2, false),
            MrStatus::Approved
        );
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "opened"), 1, 2, false),
            MrStatus::Open
        );
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "opened"), 0, 0, false),
            MrStatus::Open
        );
    }

    #[test]
    fn determine_status_requested_changes_overrides_approval() {
        // assert
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "opened"), 2, 2, true),
            MrStatus::Changes
        );
        assert_eq!(
            determine_status(&gl_mr_roles(None, None, "merged"), 0, 0, true),
            MrStatus::Merged
        );
    }

    #[test]
    fn make_initials_takes_first_two_word_starts_uppercased() {
        // assert
        assert_eq!(make_initials("backend-api-gateway"), "BA");
        assert_eq!(make_initials("frontend"), "F");
        assert_eq!(make_initials("my_cool service"), "MC");
        assert_eq!(make_initials(""), "");
    }

    #[test]
    fn notes_to_activity_orders_open_then_oldest_to_newest() {
        // arrange
        let mr = gl_mr_roles(None, None, "opened");
        let notes = vec![note("alice", T3), note("bob", T2)];

        // act
        let activity = notes_to_activity(&notes, &mr);

        // assert
        let times: Vec<&str> = activity.iter().map(|e| e.time.as_str()).collect();
        assert_eq!(times, vec![T1, T2, T3]);
    }

    #[test]
    fn notes_to_activity_marks_system_notes_with_sys_author() {
        // arrange
        let mr = gl_mr_roles(None, None, "opened");
        let sys = system_note("alice", T2);

        // act
        let activity = notes_to_activity(std::slice::from_ref(&sys), &mr);

        // assert
        let event = activity.last().unwrap();
        assert_eq!(event.who, "sys");
        assert_eq!(event.text, sys.body);
    }

    #[test]
    fn is_transient_status_matches_retryable_codes() {
        // assert
        for code in [408u16, 429, 500, 502, 503, 504] {
            assert!(is_transient_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
        for code in [200u16, 301, 400, 401, 403, 404] {
            assert!(!is_transient_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
    }

    #[test]
    fn color_for_project_wraps_around_palette() {
        // act
        let first = color_for_project(0);
        let wrapped = color_for_project(8);
        let second = color_for_project(1);

        // assert
        assert_eq!(first, wrapped);
        assert_ne!(first, second);
    }

    fn gl_project(name: &str, namespace: &str) -> GitLabProject {
        GitLabProject {
            name: name.to_string(),
            namespace: GitLabNamespace {
                name: namespace.to_string(),
            },
        }
    }

    #[test]
    fn build_projects_sorts_by_name_and_colors_deterministically() {
        // arrange
        let cache = HashMap::from([
            (30, gl_project("charlie", "ns")),
            (10, gl_project("alpha", "ns")),
            (20, gl_project("bravo", "ns")),
        ]);

        // act
        let projects = build_projects(&cache);

        // assert
        assert_eq!(
            projects,
            vec![
                Project {
                    id: 10,
                    name: "alpha".to_string(),
                    namespace: "ns".to_string(),
                    color: color_for_project(0).to_string(),
                    initials: make_initials("alpha"),
                },
                Project {
                    id: 20,
                    name: "bravo".to_string(),
                    namespace: "ns".to_string(),
                    color: color_for_project(1).to_string(),
                    initials: make_initials("bravo"),
                },
                Project {
                    id: 30,
                    name: "charlie".to_string(),
                    namespace: "ns".to_string(),
                    color: color_for_project(2).to_string(),
                    initials: make_initials("charlie"),
                },
            ]
        );
    }

    #[test]
    fn find_latest_comment_handles_millisecond_timestamps() {
        // arrange
        let since = "2026-01-01T10:00:00.000Z";
        let newer = note("alice", "2026-01-01T10:00:01.000Z");

        // act
        let actor = find_latest_comment_from_others(std::slice::from_ref(&newer), ME, since);

        // assert
        assert_eq!(actor, Some(newer.author.name));
    }
}
