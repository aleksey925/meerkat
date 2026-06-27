use crate::credentials;
use crate::models::*;
use chrono::Utc;
use reqwest::Client;
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

fn build_client(token: &str) -> Result<Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "PRIVATE-TOKEN",
        reqwest::header::HeaderValue::from_str(token)
            .map_err(|e| format!("Invalid token format: {e}"))?,
    );
    Client::builder()
        .default_headers(headers)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))
}

fn get_credentials(app: &AppHandle) -> Result<(String, String), String> {
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

    Ok((url, token))
}

#[tauri::command]
pub async fn test_connection(
    app: AppHandle,
    url: String,
    token: String,
) -> Result<UserInfo, String> {
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

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    store.set("gitlab_url", serde_json::json!(url));
    store.set("user_id", serde_json::json!(user.id));
    store.set("username", serde_json::json!(user.username));
    store.save().map_err(|e| format!("Save error: {e}"))?;

    credentials::store_token(&token)?;

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

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("API error: {e}"))?;

        if !resp.status().is_success() {
            if resp.status() == 401 {
                return Err("TOKEN_EXPIRED".to_string());
            }
            break;
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

async fn fetch_mentioned_mrs(
    client: &Client,
    base_url: &str,
    username: &str,
    uid: i64,
) -> Result<Vec<GitLabMr>, String> {
    let mut all = Vec::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "{}/api/v4/merge_requests?scope=all&state=opened&per_page=100&page={}&search=@{}",
            base_url, page, username
        );

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("API error: {e}"))?;

        if !resp.status().is_success() {
            break;
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

        // only keep MRs not authored by the user
        for mr in mrs {
            if mr.author.id != uid {
                all.push(mr);
            }
        }

        match next_page {
            Some(np) if np > page => page = np,
            _ => break,
        }
    }

    Ok(all)
}

async fn fetch_pipeline_status(
    client: &Client,
    base_url: &str,
    project_id: i64,
    mr_iid: i64,
) -> Option<PipelineStatus> {
    let url = format!(
        "{}/api/v4/projects/{}/merge_requests/{}/pipelines?per_page=1",
        base_url, project_id, mr_iid
    );

    let resp = client.get(&url).send().await.ok()?;
    let pipelines: Vec<GitLabPipeline> = resp.json().await.ok()?;

    pipelines.first().map(|p| match p.status.as_str() {
        "success" => PipelineStatus::Pass,
        "failed" => PipelineStatus::Fail,
        "running" => PipelineStatus::Running,
        _ => PipelineStatus::Pending,
    })
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

    let resp = match client.get(&url).send().await {
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

        let resp = match client.get(&url).send().await {
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
) -> HashMap<i64, ReviewRequest> {
    let mut map = HashMap::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "{}/api/v4/todos?state=pending&action=review_requested&per_page=100&page={}",
            base_url, page
        );

        let resp = match client.get(&url).send().await {
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

        let todos: Vec<GitLabTodo> = match resp.json().await {
            Ok(t) => t,
            Err(_) => break,
        };

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

    map
}

async fn fetch_notes(
    client: &Client,
    base_url: &str,
    project_id: i64,
    mr_iid: i64,
) -> Vec<GitLabNote> {
    let url = format!(
        "{}/api/v4/projects/{}/merge_requests/{}/notes?sort=desc&per_page=50",
        base_url, project_id, mr_iid
    );

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    resp.json().await.unwrap_or_default()
}

async fn fetch_project_info(
    client: &Client,
    base_url: &str,
    project_id: i64,
) -> Option<GitLabProject> {
    let url = format!("{}/api/v4/projects/{}", base_url, project_id);
    let resp = client.get(&url).send().await.ok()?;
    resp.json().await.ok()
}

fn determine_role(mr: &GitLabMr, uid: i64, username: &str, notes: &[GitLabNote]) -> UserRole {
    if mr
        .reviewers
        .as_ref()
        .is_some_and(|r| r.iter().any(|u| u.id == uid))
    {
        return UserRole::Reviewer;
    }
    if mr
        .assignees
        .as_ref()
        .is_some_and(|a| a.iter().any(|u| u.id == uid))
    {
        return UserRole::Assignee;
    }
    let mention_tag = format!("@{}", username);
    if notes.iter().any(|n| n.body.contains(&mention_tag)) {
        return UserRole::Mentioned;
    }
    // came from reviewer/assignee query but user isn't listed — fallback
    UserRole::Mentioned
}

fn determine_status(mr: &GitLabMr, approvals_current: u32, approvals_required: u32) -> MrStatus {
    match mr.state.as_str() {
        "merged" => MrStatus::Merged,
        "closed" => MrStatus::Closed,
        _ => {
            if approvals_required > 0 && approvals_current >= approvals_required {
                MrStatus::Approved
            } else {
                MrStatus::Open
            }
        }
    }
}

fn color_for_project(index: usize) -> &'static str {
    const COLORS: &[&str] = &[
        "#5e5ce6", "#ff6482", "#30d158", "#ff9f0a", "#0a84ff", "#af52de", "#ff375f", "#64d2ff",
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
        time: mr.updated_at.clone(),
        color: "#5e5ce6".to_string(),
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
                "#34c759".to_string()
            } else {
                "#5e5ce6".to_string()
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
            // I acted more recently than any other comment here — up to date
            return None;
        }
        if !note.system.unwrap_or(false) {
            return Some(note.author.name.clone());
        }
        // a system note from someone else (push, label) — not a comment, keep
        // scanning older notes for an actual message
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadStateSource {
    User,
    Auto,
}

impl ReadStateSource {
    fn as_str(self) -> &'static str {
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

#[derive(Debug, Clone, PartialEq)]
enum StoredReadState {
    Full {
        unread: bool,
        updated_at: String,
        source: ReadStateSource,
        review_request_todo_id: Option<i64>,
    },
    LegacyBool(bool),
}

fn parse_stored_read_state(val: &serde_json::Value) -> Option<StoredReadState> {
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
    if let Some(b) = val.as_bool() {
        return Some(StoredReadState::LegacyBool(b));
    }
    None
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

    // 2. a re-request review is pending for me — unread. sits above approval
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

    // 3. someone else left a real comment since I last looked — unread + notify,
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

    // 4. active approval from me — auto-read (GitLab resets approval on push,
    // so the next "real" activity will fall through to step 5 naturally)
    if signals.approved_by_me {
        return ReadStatus {
            unread: false,
            latest_actor: None,
            source: ReadStateSource::Auto,
        };
    }

    // 5. MR changed since last fetch and someone else acted — auto-unread
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

    // 6. I left a "request changes" review — auto-read. Sits below activity-from-others
    // so any reaction from the author (push, comment) flips it back to unread for the
    // next round of the fix cycle.
    if signals.i_requested_changes {
        return ReadStatus {
            unread: false,
            latest_actor: None,
            source: ReadStateSource::Auto,
        };
    }

    // 7. fallback — keep stored, default to unread for new MRs
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
    let read_store = app.store("mr_read_state.json").ok();
    let key = mr_id.to_string();
    let stored = read_store
        .as_ref()
        .and_then(|s| s.get(&key))
        .as_ref()
        .and_then(parse_stored_read_state);
    decide_unread_status(
        stored.as_ref(),
        updated_at,
        notes,
        current_username,
        signals,
    )
}

fn resolve_reminder(app: &AppHandle, mr_id: i64) -> Option<String> {
    let store = app.store("reminders.json").ok()?;
    let key = mr_id.to_string();
    let val = store.get(&key)?;
    if let Some(obj) = val.as_object() {
        return obj.get("label").and_then(|v| v.as_str()).map(String::from);
    }
    val.as_str().map(String::from)
}

fn persist_read_state(
    app: &AppHandle,
    mr_id: i64,
    unread: bool,
    updated_at: &str,
    source: ReadStateSource,
    review_request_todo_id: Option<i64>,
) {
    if let Ok(store) = app.store("mr_read_state.json") {
        store.set(
            mr_id.to_string(),
            serde_json::json!({
                "unread": unread,
                "updatedAt": updated_at,
                "source": source.as_str(),
                "reviewRequestTodoId": review_request_todo_id,
            }),
        );
        let _ = store.save();
    }
}

#[tauri::command]
pub async fn fetch_merge_requests(app: AppHandle) -> Result<MrUpdatePayload, String> {
    let (base_url, token) = get_credentials(&app)?;
    let base_url = base_url.trim_end_matches('/').to_string();
    let client = build_client(&token)?;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Store error: {e}"))?;

    let uid = store
        .get("user_id")
        .and_then(|v| v.as_i64())
        .ok_or("User ID not found. Test connection first.")?;

    let username = store
        .get("username")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    let show_mentions = store
        .get("show_mentions")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // Fetch MRs by reviewer, assignee, and mentions in parallel
    let (reviewer_mrs, assignee_mrs, mentioned_mrs) = tokio::join!(
        fetch_mrs_by_scope(&client, &base_url, "reviewer_id", uid),
        fetch_mrs_by_scope(&client, &base_url, "assignee_id", uid),
        async {
            if show_mentions && !username.is_empty() {
                fetch_mentioned_mrs(&client, &base_url, &username, uid).await
            } else {
                Ok(Vec::new())
            }
        },
    );

    let reviewer_mrs = reviewer_mrs?;
    let assignee_mrs = assignee_mrs?;
    let mentioned_mrs = mentioned_mrs.unwrap_or_default();

    // Deduplicate
    let mut seen = HashSet::new();
    let mut all_gitlab_mrs = Vec::new();

    for mr in reviewer_mrs
        .into_iter()
        .chain(assignee_mrs)
        .chain(mentioned_mrs)
    {
        if seen.insert(mr.id) {
            all_gitlab_mrs.push(mr);
        }
    }

    // Fetch project info and cache
    let mut project_cache: HashMap<i64, GitLabProject> = HashMap::new();
    let project_ids: Vec<i64> = all_gitlab_mrs
        .iter()
        .map(|m| m.project_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    for pid in &project_ids {
        if let Some(proj) = fetch_project_info(&client, &base_url, *pid).await {
            project_cache.insert(*pid, proj);
        }
    }

    // Build projects list
    let mut projects: Vec<Project> = project_cache
        .iter()
        .enumerate()
        .map(|(i, (id, proj))| Project {
            id: *id,
            name: proj.name.clone(),
            namespace: proj.namespace.name.clone(),
            color: color_for_project(i).to_string(),
            initials: make_initials(&proj.name),
        })
        .collect();
    projects.sort_by(|a, b| a.name.cmp(&b.name));

    let review_request_todos = fetch_review_request_todos(&client, &base_url).await;

    // Build MRs with details
    let mut active = Vec::new();

    for gl_mr in &all_gitlab_mrs {
        let review_request = review_request_todos.get(&gl_mr.id);
        let notes = fetch_notes(&client, &base_url, gl_mr.project_id, gl_mr.iid).await;
        let approval_info =
            fetch_approvals(&client, &base_url, gl_mr.project_id, gl_mr.iid, uid).await;
        let pipeline = fetch_pipeline_status(&client, &base_url, gl_mr.project_id, gl_mr.iid).await;
        let reviewer_states =
            fetch_reviewer_states(&client, &base_url, gl_mr.project_id, gl_mr.iid).await;
        let requested_changes = me_requested_changes(&reviewer_states, uid);
        let role = determine_role(gl_mr, uid, &username, &notes);
        let status = determine_status(gl_mr, approval_info.current, approval_info.required);
        let is_draft = gl_mr.draft.unwrap_or(false) || gl_mr.work_in_progress.unwrap_or(false);

        let proj = project_cache.get(&gl_mr.project_id);
        let project_name = proj.map(|p| p.name.clone()).unwrap_or_default();
        let project_namespace = proj.map(|p| p.namespace.name.clone()).unwrap_or_default();

        let activity = notes_to_activity(&notes, gl_mr);

        let read_status = resolve_unread_status(
            &app,
            gl_mr.id,
            &gl_mr.updated_at,
            &notes,
            &username,
            ReadSignals {
                approved_by_me: approval_info.approved_by_me,
                i_requested_changes: requested_changes,
                review_request_todo_id: review_request.map(|r| r.todo_id),
            },
        );
        let reminder = resolve_reminder(&app, gl_mr.id);

        let updated_at = chrono::DateTime::parse_from_rfc3339(&gl_mr.updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        persist_read_state(
            &app,
            gl_mr.id,
            read_status.unread,
            &gl_mr.updated_at,
            read_status.source,
            review_request.map(|r| r.todo_id),
        );

        let mr = MergeRequest {
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
            status: status.clone(),
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
            updated_at_raw: gl_mr.updated_at.clone(),
            review_request_todo_id: review_request.map(|r| r.todo_id),
            review_request_by: review_request.map(|r| r.by.clone()),
        };

        active.push(mr);
    }

    // Sort by updated_at desc
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
            id: 1,
            body: String::new(),
            author: GitLabUser {
                id: 1,
                username: username.to_string(),
                name: format!("Name {username}"),
                avatar_url: String::new(),
            },
            created_at: created_at.to_string(),
            system: None,
            noteable_type: None,
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
        assert!(find_latest_activity_from_others(&[], ME, T1).is_none());
    }

    #[test]
    fn find_latest_activity_from_others_only_my_notes_returns_none() {
        let notes = vec![note(ME, T3), note(ME, T2)];
        assert!(find_latest_activity_from_others(&notes, ME, T1).is_none());
    }

    #[test]
    fn find_latest_activity_from_others_returns_none_when_my_note_is_newest() {
        // my own comment is the latest activity: nothing to attribute to others,
        // otherwise my comment would notify me naming whoever spoke before me
        let notes = vec![note(ME, T3), note("alice", T2)];
        assert!(find_latest_activity_from_others(&notes, ME, T1).is_none());
    }

    #[test]
    fn find_latest_activity_from_others_skips_notes_at_or_before_since() {
        let notes = vec![note("alice", T1)];
        assert!(find_latest_activity_from_others(&notes, ME, T1).is_none());
    }

    #[test]
    fn find_latest_activity_from_others_picks_most_recent_when_multiple_others() {
        // first note in desc-sorted list is the newest — function should return that one
        let newest_other = note("alice", T3);
        let older_other = note("bob", T2);
        let notes = vec![newest_other.clone(), older_other];
        let actor = find_latest_activity_from_others(&notes, ME, T1);
        assert_eq!(actor, Some(newest_other.author.name));
    }

    // --- find_latest_comment_from_others ---

    #[test]
    fn find_latest_comment_returns_human_comment_from_others() {
        let other = note("alice", T2);
        let actor = find_latest_comment_from_others(std::slice::from_ref(&other), ME, T1);
        assert_eq!(actor, Some(other.author.name));
    }

    #[test]
    fn find_latest_comment_skips_others_system_note_for_older_comment() {
        // a push (system) on top of a real comment still surfaces the comment
        let comment = note("alice", T2);
        let notes = vec![system_note("alice", T3), comment.clone()];
        let actor = find_latest_comment_from_others(&notes, ME, T1);
        assert_eq!(actor, Some(comment.author.name));
    }

    #[test]
    fn find_latest_comment_returns_none_when_only_others_system_notes() {
        let notes = vec![system_note("alice", T3)];
        assert!(find_latest_comment_from_others(&notes, ME, T1).is_none());
    }

    #[test]
    fn find_latest_comment_returns_none_when_my_action_is_newest() {
        // my approval (system, mine) on top of others' comment — I'm up to date
        let notes = vec![system_note(ME, T3), note("alice", T2)];
        assert!(find_latest_comment_from_others(&notes, ME, T1).is_none());
    }

    #[test]
    fn find_latest_comment_returns_none_at_or_before_since() {
        let notes = vec![note("alice", T1)];
        assert!(find_latest_comment_from_others(&notes, ME, T1).is_none());
    }

    // --- parse_stored_read_state ---

    #[test]
    fn parse_full_object_extracts_all_fields() {
        let val = serde_json::json!({
            "unread": false,
            "updatedAt": T1,
            "source": "user",
        });
        assert_eq!(
            parse_stored_read_state(&val),
            Some(full(false, T1, ReadStateSource::User))
        );
    }

    #[test]
    fn parse_full_object_extracts_review_request_todo_id() {
        let val = serde_json::json!({
            "unread": false,
            "updatedAt": T1,
            "source": "user",
            "reviewRequestTodoId": 42,
        });
        assert_eq!(
            parse_stored_read_state(&val),
            Some(full_todo(false, T1, ReadStateSource::User, Some(42)))
        );
    }

    #[test]
    fn parse_object_missing_source_defaults_to_auto() {
        let val = serde_json::json!({ "unread": true, "updatedAt": T1 });
        assert_eq!(
            parse_stored_read_state(&val),
            Some(full(true, T1, ReadStateSource::Auto))
        );
    }

    #[test]
    fn parse_object_unknown_source_defaults_to_auto() {
        let val = serde_json::json!({ "unread": true, "updatedAt": T1, "source": "weird" });
        assert_eq!(
            parse_stored_read_state(&val),
            Some(full(true, T1, ReadStateSource::Auto))
        );
    }

    #[test]
    fn parse_object_missing_updated_at_defaults_to_empty() {
        let val = serde_json::json!({ "unread": true });
        assert_eq!(
            parse_stored_read_state(&val),
            Some(full(true, "", ReadStateSource::Auto))
        );
    }

    #[test]
    fn parse_legacy_bool_returns_legacy_variant() {
        assert_eq!(
            parse_stored_read_state(&serde_json::Value::Bool(false)),
            Some(StoredReadState::LegacyBool(false))
        );
        assert_eq!(
            parse_stored_read_state(&serde_json::Value::Bool(true)),
            Some(StoredReadState::LegacyBool(true))
        );
    }

    #[test]
    fn parse_unknown_value_returns_none() {
        assert_eq!(parse_stored_read_state(&serde_json::Value::Null), None);
        assert_eq!(parse_stored_read_state(&serde_json::json!("string")), None);
        assert_eq!(parse_stored_read_state(&serde_json::json!(42)), None);
    }

    // --- decide_unread_status: new MR ---

    #[test]
    fn new_mr_not_approved_is_unread_auto() {
        let r = decide(None, T1, &[], false);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn new_mr_approved_is_read_auto() {
        let r = decide(None, T1, &[], true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: user pin ---

    #[test]
    fn user_pin_unchanged_mr_respected_even_when_approved() {
        let stored = full(true, T1, ReadStateSource::User);
        let r = decide(Some(&stored), T1, &[note("alice", T2)], true);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn user_pin_to_read_unchanged_mr_respected() {
        let stored = full(false, T1, ReadStateSource::User);
        let r = decide(Some(&stored), T1, &[note("alice", T2)], false);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn user_pin_invalidated_then_approval_marks_read() {
        // pin valid for T1, but MR is now at T2 — pin protected only while updatedAt matches
        let stored = full(true, T1, ReadStateSource::User);
        let r = decide(Some(&stored), T2, &[], true);
        // approval kicks in next, marks read
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_invalidated_then_others_activity_marks_unread() {
        let stored = full(false, T1, ReadStateSource::User);
        let other = note("alice", T2);
        let r = decide(Some(&stored), T2, std::slice::from_ref(&other), false);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn user_pin_invalidated_with_only_my_activity_keeps_stored_unread() {
        let stored = full(false, T1, ReadStateSource::User);
        let r = decide(Some(&stored), T2, &[note(ME, T2)], false);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: approval auto-mark ---

    #[test]
    fn auto_state_unchanged_mr_with_approval_marks_read() {
        let stored = full(true, T1, ReadStateSource::Auto);
        let r = decide(Some(&stored), T1, &[], true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn approval_keeps_read_when_only_system_notes_follow() {
        // approval silences CI/push noise: a system note from another must not
        // resurface the MR
        let stored = full(false, T1, ReadStateSource::Auto);
        let r = decide(Some(&stored), T2, &[system_note("alice", T2)], true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn approval_does_not_silence_human_comment_from_others() {
        // a real comment must reach me even after I approved (matches user's spec)
        let stored = full(false, T1, ReadStateSource::Auto);
        let other = note("alice", T2);
        let r = decide(Some(&stored), T2, std::slice::from_ref(&other), true);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn approval_then_my_own_comment_stays_read() {
        // approving and then commenting myself: I'm up to date, no resurface
        let stored = full(false, T1, ReadStateSource::Auto);
        let notes = [note(ME, T3), system_note(ME, T2)];
        let r = decide(Some(&stored), T3, &notes, true);
        assert!(!r.unread);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn approval_with_others_comment_before_my_action_stays_read() {
        // others commented, then I approved on top — I've seen it, stay read
        let stored = full(false, T1, ReadStateSource::Auto);
        let notes = [system_note(ME, T3), note("alice", T2)];
        let r = decide(Some(&stored), T3, &notes, true);
        assert!(!r.unread);
        assert!(r.latest_actor.is_none());
    }

    // --- decide_unread_status: activity from others ---

    #[test]
    fn auto_state_changed_mr_with_others_activity_marks_unread() {
        let stored = full(false, T1, ReadStateSource::Auto);
        let other = note("alice", T2);
        let r = decide(Some(&stored), T2, std::slice::from_ref(&other), false);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn auto_state_changed_mr_with_only_my_activity_keeps_stored_unread() {
        let stored = full(false, T1, ReadStateSource::Auto);
        let r = decide(Some(&stored), T2, &[note(ME, T2)], false);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert!(r.latest_actor.is_none());
    }

    #[test]
    fn my_comment_after_others_note_does_not_set_actor() {
        // user pinned unread, then commented themselves: must stay unread but
        // expose no actor, so polling won't fire a notification for my own comment
        let stored = full(true, T1, ReadStateSource::User);
        let r = decide(Some(&stored), T3, &[note(ME, T3), note("alice", T2)], false);
        assert!(r.unread);
        assert!(r.latest_actor.is_none());
    }

    // --- decide_unread_status: unchanged MR without pin/approval ---

    #[test]
    fn auto_state_unchanged_mr_no_approval_keeps_stored_unread() {
        let stored = full(true, T1, ReadStateSource::Auto);
        let r = decide(Some(&stored), T1, &[note("alice", T2)], false);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: legacy bool ---

    #[test]
    fn legacy_bool_unread_true_not_approved_stays_unread() {
        let stored = StoredReadState::LegacyBool(true);
        let r = decide(Some(&stored), T1, &[], false);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn legacy_bool_unread_false_not_approved_stays_read() {
        let stored = StoredReadState::LegacyBool(false);
        let r = decide(Some(&stored), T1, &[], false);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn legacy_bool_with_approval_marks_read() {
        let stored = StoredReadState::LegacyBool(true);
        let r = decide(Some(&stored), T1, &[], true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: requested_changes by me ---

    #[test]
    fn new_mr_with_my_requested_changes_is_read_auto() {
        let r = decide_with_rc(None, T1, &[], false, true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn auto_state_unchanged_mr_with_my_requested_changes_marks_read() {
        let stored = full(true, T1, ReadStateSource::Auto);
        let r = decide_with_rc(Some(&stored), T1, &[], false, true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn auto_state_changed_mr_with_my_requested_changes_and_others_activity_marks_unread() {
        // key fix-cycle case: I asked for changes, author pushed/replied — back to unread
        let stored = full(false, T1, ReadStateSource::Auto);
        let other = note("alice", T2);
        let r = decide_with_rc(Some(&stored), T2, std::slice::from_ref(&other), false, true);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
        assert_eq!(r.latest_actor, Some(other.author.name));
    }

    #[test]
    fn auto_state_changed_mr_with_my_requested_changes_and_only_my_activity_marks_read() {
        let stored = full(false, T1, ReadStateSource::Auto);
        let r = decide_with_rc(Some(&stored), T2, &[note(ME, T2)], false, true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn approval_takes_precedence_over_requested_changes() {
        let r = decide_with_rc(None, T1, &[], true, true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_unchanged_mr_with_my_requested_changes_respects_pin() {
        let stored = full(true, T1, ReadStateSource::User);
        let r = decide_with_rc(Some(&stored), T1, &[], false, true);
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn legacy_bool_with_my_requested_changes_marks_read() {
        let stored = StoredReadState::LegacyBool(true);
        let r = decide_with_rc(Some(&stored), T1, &[], false, true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- decide_unread_status: pending re-request review ---

    #[test]
    fn review_request_pending_marks_unread() {
        let stored = full(false, T1, ReadStateSource::Auto);
        let r = decide_with_review_request(Some(&stored), T1, false, Some(1));
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn review_request_overrides_active_approval() {
        let stored = full(false, T1, ReadStateSource::Auto);
        let r = decide_with_review_request(Some(&stored), T1, true, Some(1));
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_unchanged_mr_with_same_review_request_respects_pin() {
        // user read the MR while this re-request todo was already pending: the todo
        // id matches the pin anchor, so the pin holds
        let stored = full_todo(false, T1, ReadStateSource::User, Some(7));
        let r = decide_with_review_request(Some(&stored), T1, false, Some(7));
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::User);
    }

    #[test]
    fn user_pin_broken_by_new_review_request() {
        // user read the MR with no re-request pending, then a re-request arrives:
        // a fresh todo id breaks the pin and marks the MR unread
        let stored = full_todo(false, T1, ReadStateSource::User, None);
        let r = decide_with_review_request(Some(&stored), T1, false, Some(9));
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    #[test]
    fn user_pin_broken_by_changed_review_request_todo() {
        // user read the MR during one re-request, then the author re-requests again
        // (new todo id) without touching updated_at: the pin must break
        let stored = full_todo(false, T1, ReadStateSource::User, Some(7));
        let r = decide_with_review_request(Some(&stored), T1, false, Some(9));
        assert!(r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
    }

    // --- me_requested_changes helper ---

    #[test]
    fn me_requested_changes_returns_true_when_my_state_is_requested_changes() {
        let reviewers = vec![
            reviewer(2, Some("approved")),
            reviewer(1, Some("requested_changes")),
        ];
        assert!(me_requested_changes(&reviewers, 1));
    }

    #[test]
    fn me_requested_changes_returns_false_for_other_states() {
        for state in ["unreviewed", "reviewed", "approved"] {
            let reviewers = vec![reviewer(1, Some(state))];
            assert!(!me_requested_changes(&reviewers, 1));
        }
    }

    #[test]
    fn me_requested_changes_returns_false_when_state_missing() {
        let reviewers = vec![reviewer(1, None)];
        assert!(!me_requested_changes(&reviewers, 1));
    }

    #[test]
    fn me_requested_changes_ignores_other_reviewers_state() {
        let reviewers = vec![reviewer(2, Some("requested_changes"))];
        assert!(!me_requested_changes(&reviewers, 1));
    }

    #[test]
    fn me_requested_changes_returns_false_for_empty_reviewers() {
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

    // --- ReadStateSource round-trip ---

    #[test]
    fn read_state_source_roundtrip() {
        for src in [ReadStateSource::User, ReadStateSource::Auto] {
            assert_eq!(ReadStateSource::from_stored(src.as_str()), src);
        }
    }
}
