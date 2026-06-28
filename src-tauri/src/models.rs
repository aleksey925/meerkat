use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: i64,
    pub username: String,
    pub name: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Reviewer,
    Assignee,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MrStatus {
    Open,
    Merged,
    Closed,
    Changes,
    Approved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PipelineStatus {
    Pass,
    Fail,
    Running,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequest {
    pub id: i64,
    pub iid: i64,
    pub project_id: i64,
    pub project_name: String,
    pub project_namespace: String,
    pub title: String,
    pub source_branch: String,
    pub target_branch: String,
    pub author_name: String,
    pub author_username: String,
    pub role: UserRole,
    pub status: MrStatus,
    pub draft: bool,
    pub has_conflicts: bool,
    pub pipeline_status: Option<PipelineStatus>,
    pub approvals_current: u32,
    pub approvals_required: u32,
    pub web_url: String,
    pub updated_at: DateTime<Utc>,
    pub unread: bool,
    pub reminder: Option<String>,
    pub activity: Vec<ActivityEvent>,
    #[serde(skip)]
    pub latest_actor: Option<String>,
    // raw GitLab updated_at string, used as anchor for read-state comparisons
    #[serde(skip)]
    pub updated_at_raw: String,
    // id of the pending "review requested" todo, used to detect a fresh
    // re-request between polls (re-request doesn't move updated_at)
    #[serde(skip)]
    pub review_request_todo_id: Option<i64>,
    #[serde(skip)]
    pub review_request_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub who: String,
    pub text: String,
    pub time: String,
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub namespace: String,
    pub color: String,
    pub initials: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub poll_interval: String,
    pub show_drafts: bool,
    pub desktop_notif: bool,
    pub sound_notif: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrUpdatePayload {
    pub active: Vec<MergeRequest>,
    pub projects: Vec<Project>,
}

// GitLab API response types
#[derive(Debug, Clone, Deserialize)]
pub struct GitLabMr {
    pub id: i64,
    pub iid: i64,
    pub title: String,
    pub source_branch: String,
    pub target_branch: String,
    pub author: GitLabUser,
    pub reviewers: Option<Vec<GitLabUser>>,
    pub assignees: Option<Vec<GitLabUser>>,
    pub state: String,
    pub draft: Option<bool>,
    pub work_in_progress: Option<bool>,
    pub has_conflicts: Option<bool>,
    pub web_url: String,
    pub created_at: String,
    pub updated_at: String,
    pub project_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabUser {
    pub id: i64,
    pub username: String,
    pub name: String,
    #[serde(default)]
    pub avatar_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabPipeline {
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabReviewerState {
    pub user: GitLabUser,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabTodo {
    pub id: i64,
    pub action_name: String,
    pub target_type: String,
    pub target: GitLabTodoTarget,
    pub author: GitLabUser,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabTodoTarget {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabApprovals {
    pub approvals_required: Option<i32>,
    pub approved_by: Option<Vec<GitLabApprovalUser>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabApprovalUser {
    pub user: GitLabUser,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabNote {
    pub body: String,
    pub author: GitLabUser,
    pub created_at: String,
    pub system: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabProject {
    pub name: String,
    pub namespace: GitLabNamespace,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabNamespace {
    pub name: String,
}
