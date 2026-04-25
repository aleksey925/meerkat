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

    for note in notes.iter().rev().take(20) {
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
    // notes are sorted desc (newest first)
    for note in notes {
        if note.created_at.as_str() <= since {
            break;
        }
        if note.author.username != current_username {
            return Some(note.author.name.clone());
        }
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

#[derive(Debug, Clone, PartialEq)]
enum StoredReadState {
    Full {
        unread: bool,
        updated_at: String,
        source: ReadStateSource,
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
        return Some(StoredReadState::Full {
            unread,
            updated_at,
            source,
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
    approved_by_me: bool,
) -> ReadStatus {
    match stored {
        Some(StoredReadState::Full {
            unread: stored_unread,
            updated_at: stored_updated,
            source: stored_source,
        }) => {
            // user pinned the state and MR hasn't changed — respect it
            if *stored_source == ReadStateSource::User && stored_updated == updated_at {
                return ReadStatus {
                    unread: *stored_unread,
                    latest_actor: None,
                    source: ReadStateSource::User,
                };
            }

            // active approval from me — auto-read regardless of activity
            if approved_by_me {
                return ReadStatus {
                    unread: false,
                    latest_actor: None,
                    source: ReadStateSource::Auto,
                };
            }

            // MR changed since last fetch — check for activity from others
            if stored_updated != updated_at {
                let latest_actor =
                    find_latest_activity_from_others(notes, current_username, stored_updated);
                if latest_actor.is_some() {
                    return ReadStatus {
                        unread: true,
                        latest_actor,
                        source: ReadStateSource::Auto,
                    };
                }
                return ReadStatus {
                    unread: *stored_unread,
                    latest_actor: None,
                    source: ReadStateSource::Auto,
                };
            }

            // MR unchanged, no user pin, no approval — keep stored
            ReadStatus {
                unread: *stored_unread,
                latest_actor: None,
                source: ReadStateSource::Auto,
            }
        }
        Some(StoredReadState::LegacyBool(b)) => {
            if approved_by_me {
                ReadStatus {
                    unread: false,
                    latest_actor: None,
                    source: ReadStateSource::Auto,
                }
            } else {
                ReadStatus {
                    unread: *b,
                    latest_actor: None,
                    source: ReadStateSource::Auto,
                }
            }
        }
        None => {
            if approved_by_me {
                ReadStatus {
                    unread: false,
                    latest_actor: None,
                    source: ReadStateSource::Auto,
                }
            } else {
                ReadStatus {
                    unread: true,
                    latest_actor: None,
                    source: ReadStateSource::Auto,
                }
            }
        }
    }
}

fn resolve_unread_status(
    app: &AppHandle,
    mr_id: i64,
    updated_at: &str,
    notes: &[GitLabNote],
    current_username: &str,
    approved_by_me: bool,
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
        approved_by_me,
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
) {
    if let Ok(store) = app.store("mr_read_state.json") {
        store.set(
            mr_id.to_string(),
            serde_json::json!({
                "unread": unread,
                "updatedAt": updated_at,
                "source": source.as_str(),
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

    // Build MRs with details
    let mut active = Vec::new();

    for gl_mr in &all_gitlab_mrs {
        let notes = fetch_notes(&client, &base_url, gl_mr.project_id, gl_mr.iid).await;
        let approval_info =
            fetch_approvals(&client, &base_url, gl_mr.project_id, gl_mr.iid, uid).await;
        let pipeline = fetch_pipeline_status(&client, &base_url, gl_mr.project_id, gl_mr.iid).await;
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
            approval_info.approved_by_me,
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

    fn full(unread: bool, updated_at: &str, source: ReadStateSource) -> StoredReadState {
        StoredReadState::Full {
            unread,
            updated_at: updated_at.to_string(),
            source,
        }
    }

    fn decide(
        stored: Option<&StoredReadState>,
        updated_at: &str,
        notes: &[GitLabNote],
        approved_by_me: bool,
    ) -> ReadStatus {
        decide_unread_status(stored, updated_at, notes, ME, approved_by_me)
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
    fn find_latest_activity_from_others_returns_other_skipping_my_newer_note() {
        // notes are sorted desc (newest first), as GitLab returns them
        let other = note("alice", T2);
        let notes = vec![note(ME, T3), other.clone()];
        let actor = find_latest_activity_from_others(&notes, ME, T1);
        assert_eq!(actor, Some(other.author.name));
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
    fn auto_state_changed_mr_with_approval_marks_read_ignoring_others_activity() {
        // active approval beats activity-from-others rule (matches user's spec)
        let stored = full(false, T1, ReadStateSource::Auto);
        let r = decide(Some(&stored), T2, &[note("alice", T2)], true);
        assert!(!r.unread);
        assert_eq!(r.source, ReadStateSource::Auto);
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

    // --- ReadStateSource round-trip ---

    #[test]
    fn read_state_source_roundtrip() {
        for src in [ReadStateSource::User, ReadStateSource::Auto] {
            assert_eq!(ReadStateSource::from_stored(src.as_str()), src);
        }
    }
}
