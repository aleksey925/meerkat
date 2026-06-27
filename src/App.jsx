import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAutoAnimate } from "@formkit/auto-animate/react";
import "./App.css";
import formatDateTime from "./utils/formatDateTime";
import { useSettings } from "./hooks/useSettings";
import { useGitlab } from "./hooks/useGitlab";
import CustomReminderModal from "./components/CustomReminderModal";

// ─── Utility Helpers ───────────────────────────────────────────────────────
function roleLabel(role) {
  switch (role) {
    case "reviewer": return "Review";
    case "assignee": return "Assignee";
    case "mentioned": return "@ Mentioned";
    default: return role || "—";
  }
}

function pipelineLabel(pipeline) {
  switch (pipeline) {
    case "pass": return "CI passed";
    case "fail": return "CI failed";
    case "running": return "CI running";
    default: return null;
  }
}

function getAuthorInfo(mr) {
  return {
    name: mr.authorName || "Unknown",
    color: "#5e5ce6",
    initials: (mr.authorUsername || "?").substring(0, 2).toUpperCase(),
  };
}

function getProjectId(mr) {
  return mr.projectId || mr.project;
}

function useRelativeTime(date) {
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    if (!date) return;
    const id = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(id);
  }, [date]);

  if (!date) return null;
  const diff = Math.max(0, Math.floor((now - date.getTime()) / 1000));
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

// ─── Toast Component ───────────────────────────────────────────────────────
function Toast({ message }) {
  if (!message) return null;
  return <div className="toast">{message}</div>;
}

function computeReminderDate(label) {
  const now = new Date();
  switch (label) {
    case "In 30 minutes": now.setMinutes(now.getMinutes() + 30); break;
    case "In 1 hour": now.setHours(now.getHours() + 1); break;
    case "In 3 hours": now.setHours(now.getHours() + 3); break;
    case "Tomorrow, 9:00 AM": now.setDate(now.getDate() + 1); now.setHours(9, 0, 0, 0); break;
    case "Monday, 9:00 AM": {
      const day = now.getDay();
      const daysUntilMonday = day === 0 ? 1 : 8 - day;
      now.setDate(now.getDate() + daysUntilMonday);
      now.setHours(9, 0, 0, 0);
      break;
    }
  }
  return now.toISOString();
}

// ─── Context Menu Component ────────────────────────────────────────────────
function ContextMenu({ mr, position, onClose, onToggleUnread, onSetReminder, onClearReminder, onOpenGitLab, onCustomReminder }) {
  const menuRef = useRef(null);
  const [pos, setPos] = useState(null);

  useEffect(() => {
    function handleClick(e) {
      if (menuRef.current && !menuRef.current.contains(e.target)) onClose();
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [onClose]);

  useEffect(() => {
    if (!position || !menuRef.current) {
      setPos(null);
      return;
    }
    const rect = menuRef.current.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const margin = 16;
    // flip to left side of cursor if overflows right
    let x = position.x + rect.width > vw - margin
      ? position.x - rect.width - 4
      : position.x;
    // flip upward if overflows bottom
    let y = position.y + rect.height > vh - margin
      ? position.y - rect.height - 4
      : position.y;
    if (x < margin) x = margin;
    if (y < margin) y = margin;
    setPos({ x, y });
  }, [position]);

  if (!mr || !position) return null;

  return (
    <div className="ctx-menu" style={{ top: pos?.y ?? position.y, left: pos?.x ?? position.x, visibility: pos ? "visible" : "hidden" }} ref={menuRef}>
      <div className="ctx-item" onClick={() => { onToggleUnread(mr.id); onClose(); }}>
        <span className="ctx-icon">{"\u2709"}</span>
        <span className="ctx-label">{mr.unread ? "Mark as read" : "Mark as unread"}</span>
      </div>
      <div className="ctx-item" onClick={() => { onOpenGitLab(mr); onClose(); }}>
        <span className="ctx-icon">{"\u21D7"}</span>
        <span className="ctx-label">Open in GitLab</span>
      </div>
      <div className="ctx-sep" />
      <div className="ctx-sub-header">Remind me</div>
      {["In 30 minutes", "In 1 hour", "In 3 hours", "Tomorrow, 9:00 AM", "Monday, 9:00 AM"].map((label) => (
        <div className="ctx-item" key={label} onClick={() => { onSetReminder(mr.id, label, computeReminderDate(label)); onClose(); }}>
          <span className="ctx-icon">{"\u23F0"}</span>
          <span className="ctx-label">{label === "Tomorrow, 9:00 AM" ? "Tomorrow morning" : label === "Monday, 9:00 AM" ? "Next Monday" : label}</span>
        </div>
      ))}
      <div className="ctx-sep" />
      <div className="ctx-item" onClick={() => { onCustomReminder(mr.id); onClose(); }}>
        <span className="ctx-icon">{"\u270E"}</span>
        <span className="ctx-label">Custom time...</span>
      </div>
      {mr.reminder && (
        <>
          <div className="ctx-sep" />
          <div className="ctx-item" onClick={() => { onClearReminder(mr.id); onClose(); }}>
            <span className="ctx-icon ctx-danger">{"\u2715"}</span>
            <span className="ctx-label ctx-danger">Remove reminder</span>
          </div>
        </>
      )}
    </div>
  );
}

// ─── MR Card Component ─────────────────────────────────────────────────────
function MrCard({ mr, isSelected, onSelect, onContextMenu, onOpenGitLab, onToggleUnread }) {
  const author = getAuthorInfo(mr);
  const pipeline = mr.pipelineStatus || mr.pipeline;
  const approvals = mr.approvalsCurrent ?? mr.approvals ?? 0;
  const needed = mr.approvalsRequired ?? mr.needed ?? 0;
  const conflicts = mr.hasConflicts ?? mr.conflicts ?? false;

  return (
    <div
      className={`mr-card${mr.unread ? " unread" : ""} ${isSelected ? "selected" : ""}`}
      onClick={(e) => { e.stopPropagation(); onSelect(mr.id); }}
      onContextMenu={(e) => { e.preventDefault(); onContextMenu(e, mr.id); }}
    >
      <div className="mr-top">
        <div className="mr-content">
          <div className="mr-title-row">
            <div className="mr-title">{mr.title}</div>
            <div className="mr-actions">
              <button
                className={`mr-action-btn read-btn${mr.unread ? "" : " is-read"}`}
                onClick={(e) => { e.stopPropagation(); onToggleUnread(mr.id); }}
                title={mr.unread ? "Mark as read" : "Mark as unread"}
              >
                {mr.unread ? (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
                    <circle cx="12" cy="12" r="3" />
                  </svg>
                ) : (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M20 6L9 17l-5-5" />
                  </svg>
                )}
              </button>
              <button className="mr-action-btn" onClick={(e) => { e.stopPropagation(); onContextMenu(e, mr.id); }} title="Actions">{"\u2026"}</button>
              <button className="mr-action-btn gl-btn" onClick={(e) => { e.stopPropagation(); onOpenGitLab(mr); }} title="Open in GitLab">{"\u21D7"} GitLab</button>
            </div>
          </div>
          <div className="mr-subtitle">
            <span className="mr-author-ava" style={{ background: author.color }}>{author.initials || (mr.authorUsername || "?").substring(0, 2).toUpperCase()}</span>
            <span style={{ marginLeft: 4 }}>{author.name}</span>
            <span className="mr-sep">{"\u00B7"}</span>
            <span className="mr-branch">{mr.branch || `${mr.sourceBranch} \u2192 ${mr.targetBranch}`}</span>
          </div>
          <div className="mr-meta">
            {mr.unread && <span className="mr-pill new">New</span>}
            <span className={`mr-pill ${mr.role}`}>{roleLabel(mr.role)}</span>
            {mr.draft && <span className="mr-pill draft">Draft</span>}
            {conflicts && <span className="mr-pill conflicts">Conflicts</span>}
            {(mr.status === "approved" || approvals > 0) && (
              <span className="mr-pill approved">{"\u2713"} {approvals}/{needed}</span>
            )}
            {mr.status === "changes" && <span className="mr-pill changes">Changes requested</span>}
            {mr.status === "merged" && <span className="mr-pill merged">Merged</span>}
            {pipeline && (
              <span className={`mr-pill pipe-${pipeline}`}>{pipelineLabel(pipeline)}</span>
            )}
            {mr.reminder && <span className="mr-pill reminder-pill">{"\u23F0"} {mr.reminder}</span>}
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Detail Panel Component ────────────────────────────────────────────────
function DetailPanel({ mr, closing, onClose, onAnimDone, onToggleUnread, onOpenGitLab, onRemindClick }) {
  const author = getAuthorInfo(mr);
  const pipeline = mr.pipelineStatus || mr.pipeline;
  const approvals = mr.approvalsCurrent ?? mr.approvals ?? 0;
  const needed = mr.approvalsRequired ?? mr.needed ?? 0;
  const conflicts = mr.hasConflicts ?? mr.conflicts ?? false;

  return (
    <div className={`detail-panel${closing ? " closing" : ""}`} onAnimationEnd={closing ? onAnimDone : undefined} onClick={(e) => e.stopPropagation()}>
      <div className="dp-header">
        <div style={{ flex: 1 }}>
          <div className="dp-title">{mr.title}</div>
          <div className="dp-branch">{mr.branch || `${mr.sourceBranch} \u2192 ${mr.targetBranch}`}</div>
        </div>
        <button className="dp-close" onClick={onClose}>{"\u2715"}</button>
      </div>

      <div className="dp-section">
        <div className="dp-label">Details</div>
        <div className="dp-row"><span className="dp-row-label">Author</span><span className="dp-row-val">{author.name}</span></div>
        <div className="dp-row"><span className="dp-row-label">Your role</span><span className="dp-row-val">{roleLabel(mr.role)}</span></div>
        <div className="dp-row"><span className="dp-row-label">Pipeline</span><span className="dp-row-val">{pipelineLabel(pipeline) || "—"}</span></div>
        <div className="dp-row"><span className="dp-row-label">Approvals</span><span className="dp-row-val">{approvals} / {needed}</span></div>
        <div className="dp-row"><span className="dp-row-label">Conflicts</span><span className="dp-row-val">{conflicts ? "Yes" : "None"}</span></div>
        {mr.reminder && (
          <div className="dp-row"><span className="dp-row-label">Reminder</span><span className="remind-badge">{"\u23F0"} {mr.reminder}</span></div>
        )}
        <div className="dp-actions">
          <button className="dp-btn primary" onClick={() => onOpenGitLab(mr)}>{"\u21D7"} Open in GitLab</button>
          <button className="dp-btn" onClick={() => onToggleUnread(mr.id)}>
            {mr.unread ? "\u2709 Mark as read" : "\u2709 Mark as unread"}
          </button>
          <button className="dp-btn" onClick={() => onRemindClick(mr.id)}>{"\u23F0"} Remind me</button>
        </div>
      </div>

      <div className="dp-section dp-activity-section">
        <div className="dp-label">Activity</div>
        <div className="activity-list">
          {(mr.activity || []).map((a, i) => (
            <div className="activity-item" key={i}>
              <div className="act-dot" style={{ background: a.color }}>
                {a.who === "sys" ? "\u26A1" : a.who === "you" ? "Y" : a.who.charAt(0).toUpperCase()}
              </div>
              <div className="act-body">
                <div className="act-text">{a.text}</div>
                <div className="act-time">{formatDateTime(a.time)}</div>
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

// ─── Settings View Component ───────────────────────────────────────────────
function SettingsView({ settings, onUpdate, onTestConnection, onSave, notifPermission, onNotifPermissionChange, showToast, appVersion }) {
  const update = (key, value) => onUpdate({ ...settings, [key]: value });
  const saveField = (key, value) => {
    const next = { ...settings, [key]: value };
    onSave(next);
  };
  const toggle = (key) => {
    const next = { ...settings, [key]: !settings[key] };
    onUpdate(next);
    onSave(next);
  };

  return (
    <div className="main">
      <div className="topbar">
        <div className="topbar-title">Settings</div>
      </div>
      <div className="settings-view">
        <div className="settings-title">GitLab connection</div>
        <div className="settings-subtitle">Configure your GitLab instance and personal access token</div>

        <div className="settings-card">
          <div className="settings-card-header">
            <div className="settings-card-title">Connection</div>
            <div className={`status-badge ${settings.connected ? "connected" : "disconnected"}`}>
              <div className="status-dot-sm" style={{ background: settings.connected ? "var(--green)" : "var(--red)" }} />
              {settings.connected ? "Connected" : "Disconnected"}
            </div>
          </div>

          <div className="field-group">
            <div className="field-label">GitLab URL</div>
            <input
              className="field-input mono"
              type="text"
              value={settings.url}
              placeholder="https://gitlab.example.com"
              onChange={(e) => update("url", e.target.value)}
              onBlur={(e) => saveField("url", e.target.value)}
            />
            <div className="field-hint">Full URL of your GitLab instance, including https://</div>
          </div>

          <div className="field-group">
            <div className="field-label">
              Personal access token <span className="field-label-hint">(read_api scope required)</span>
            </div>
            <div className="field-row">
              <input
                className="field-input mono"
                type={settings.tokenVisible ? "text" : "password"}
                value={settings.token || ""}
                placeholder="glpat-xxxxxxxxxxxxxxxx"
                onChange={(e) => update("token", e.target.value)}
                onBlur={(e) => saveField("token", e.target.value)}
              />
              <button className="dp-btn" style={{ whiteSpace: "nowrap", height: 38 }} onClick={() => update("tokenVisible", !settings.tokenVisible)}>
                {settings.tokenVisible ? "Hide" : "Show"}
              </button>
            </div>
            <div className="field-hint">
              Token is stored securely in your operating system's keychain. Generate one in GitLab {">"} Preferences {">"} Access Tokens with <code className="inline-code">read_api</code> scope
            </div>
          </div>

          <div className="settings-actions">
            <button className="dp-btn primary" onClick={onTestConnection}>Test connection</button>
          </div>
        </div>

        <div className="settings-card">
          <div className="settings-card-title">Polling</div>
          <div className="settings-card-desc">How often to check for new merge requests and updates</div>
          <div className="field-group">
            <div className="field-label">Check interval</div>
            <select className="poll-select" value={settings.pollInterval} onChange={(e) => { const next = { ...settings, pollInterval: e.target.value }; onUpdate(next); onSave(next); }}>
              <option value="15">Every 15 seconds</option>
              <option value="30">Every 30 seconds</option>
              <option value="60">Every 1 minute</option>
              <option value="120">Every 2 minutes</option>
              <option value="300">Every 5 minutes</option>
            </select>
          </div>
        </div>

        <div className="settings-card">
          <div className="settings-card-title">Notifications</div>
          <div className="settings-card-desc">Control what triggers notifications</div>
          {notifPermission === false && (
            <div className="notif-warning">
              <span>&#9888;</span>
              <span>Notifications are blocked by the system. Open System Settings &rarr; Notifications to allow them. This warning may take a few seconds to disappear after granting access. You can also use the test button below to verify.</span>
            </div>
          )}
          {[
            { key: "desktopNotif", label: "Desktop notifications", desc: "Show system notifications for new events" },
            { key: "soundNotif", label: "Sound", desc: "Play a sound when a new MR needs attention" },
            { key: "showDrafts", label: "Show drafts", desc: "Include draft merge requests in the feed" },
            { key: "showMentions", label: "Show mentions", desc: "Include MRs where you are only mentioned" },
          ].map(({ key, label, desc }) => (
            <div className="toggle-row" key={key}>
              <div>
                <div className="toggle-row-label">{label}</div>
                <div className="toggle-row-desc">{desc}</div>
              </div>
              <button className={`toggle ${settings[key] ? "on" : ""}`} onClick={() => toggle(key)}>
                <div className="toggle-knob" />
              </button>
            </div>
          ))}
          <div className="settings-actions">
            <button
              className="dp-btn primary"
              onClick={() => {
                const invoke = window.__TAURI_INTERNALS__?.invoke;
                if (!invoke) return;
                invoke("send_test_notification")
                  .then(() => {
                    showToast("Test notification sent. If you don't see a banner, check Notification Center — macOS hides banners for active apps.");
                    invoke("check_notification_permission").then((granted) => onNotifPermissionChange(granted));
                  })
                  .catch(() => showToast("Failed to send test notification"));
              }}
            >
              Send test notification
            </button>
          </div>
        </div>

        <div className="settings-version">Meerkat v{appVersion}</div>
      </div>
    </div>
  );
}

// ─── Inbox View Component ──────────────────────────────────────────────────
function InboxView({
  mergeRequests, projects, selectedProject, selectedRole,
  selectedMrId, searchQuery, loading, lastChecked, checking,
  closingPanel, onClosePanel, onPanelAnimDone,
  onSelectProject, onSelectRole, onSelectMr,
  onSearch, onContextMenu, onToggleUnread, onOpenGitLab, onRemindClick, onCheckNow,
  sortBy, onSortChange,
}) {
  const lastCheckedLabel = useRelativeTime(lastChecked);
  const [listRef] = useAutoAnimate({ duration: 250 });
  const filtered = (() => {
    let list = mergeRequests;
    if (selectedProject > 0) list = list.filter((m) => getProjectId(m) === selectedProject);
    if (selectedRole !== "all") list = list.filter((m) => m.role === selectedRole);
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      list = list.filter((m) =>
        m.title.toLowerCase().includes(q) ||
        (m.branch && m.branch.toLowerCase().includes(q)) ||
        (m.sourceBranch && m.sourceBranch.toLowerCase().includes(q))
      );
    }
    list = [...list].sort((a, b) => {
      if (sortBy === "unread") {
        if (a.unread !== b.unread) return a.unread ? -1 : 1;
      }
      const ta = new Date(a.updatedAt || 0).getTime();
      const tb = new Date(b.updatedAt || 0).getTime();
      return tb - ta;
    });
    return list;
  })();

  const roleCounts = (() => {
    let list = mergeRequests;
    if (selectedProject > 0) list = list.filter((m) => getProjectId(m) === selectedProject);
    return {
      all: list.length,
      reviewer: list.filter((m) => m.role === "reviewer").length,
      assignee: list.filter((m) => m.role === "assignee").length,
      mentioned: list.filter((m) => m.role === "mentioned").length,
    };
  })();

  const selectedMr = mergeRequests.find((m) => m.id === selectedMrId);
  const projectName = selectedProject === 0 ? "All merge requests" : projects.find((p) => p.id === selectedProject)?.name;

  return (
    <div className="main">
      <div className="topbar">
        <div className="topbar-title">{projectName}</div>
        <div className="topbar-right">
          <button
            className="check-now-btn dp-btn"
            onClick={onCheckNow}
            disabled={checking || loading}
            title={lastChecked ? `Last checked: ${lastChecked.toLocaleTimeString()}` : "Check now"}
          >
            <span className={`check-icon${checking || loading ? " spinning" : ""}`}>{"\u21BB"}</span>
            <span className="check-now-text">
              <span className="check-now-label">{checking || loading ? "Checking\u2026" : "Check Now"}</span>
              {lastCheckedLabel && lastCheckedLabel !== "just now" && (
                <span className="check-now-time">{lastCheckedLabel}</span>
              )}
            </span>
          </button>
          <div className="seg-control">
            {[
              ["all", "All", roleCounts.all],
              ["reviewer", "Review", roleCounts.reviewer],
              ["assignee", "Assignee", roleCounts.assignee],
              ["mentioned", "Mentioned", roleCounts.mentioned],
            ].map(([key, label, count]) => (
              <button key={key} className={`seg-btn ${selectedRole === key ? "active" : ""}`} onClick={() => onSelectRole(key)}>
                {label}{count ? ` \u00B7 ${count}` : ""}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div className="filter-bar">
        <input
          className="search-box"
          type="text"
          placeholder="Search merge requests..."
          value={searchQuery}
          onChange={(e) => onSearch(e.target.value)}
        />
        <div className="sort-wrapper">
          <span className="sort-icon">{"\u21C5"}</span>
          <select
            className="sort-select"
            value={sortBy}
            onChange={(e) => onSortChange(e.target.value)}
            title="Sort order"
          >
            <option value="unread">Unread first</option>
            <option value="updated">By update time</option>
          </select>
        </div>
      </div>

      <div className="main-wrap">
        <div className="main-content">
          <div className="mr-list" ref={listRef}>
            {loading ? (
              <div className="empty-state">
                <div className="empty-icon">{"\u23F3"}</div>
                <div className="empty-title">Loading...</div>
                <div className="empty-desc">Fetching merge requests from GitLab</div>
              </div>
            ) : filtered.length === 0 ? (
              <div className="empty-state">
                <div className="empty-icon">{"\u2713"}</div>
                <div className="empty-title">All clear</div>
                <div className="empty-desc">No merge requests here</div>
              </div>
            ) : (
              filtered.map((mr) => (
                <MrCard
                  key={mr.id}
                  mr={mr}
                  isSelected={selectedMrId === mr.id}
                  onSelect={onSelectMr}
                  onContextMenu={onContextMenu}
                  onOpenGitLab={onOpenGitLab}
                  onToggleUnread={onToggleUnread}
                />
              ))
            )}
          </div>
        </div>
        {selectedMr && (
          <DetailPanel
            mr={selectedMr}
            closing={closingPanel}
            onClose={onClosePanel}
            onAnimDone={onPanelAnimDone}
            onToggleUnread={onToggleUnread}
            onOpenGitLab={onOpenGitLab}
            onRemindClick={onRemindClick}
          />
        )}
      </div>
    </div>
  );
}

// ─── App Root ──────────────────────────────────────────────────────────────
export default function App() {
  const [view, setView] = useState("inbox");
  const [selectedProject, setSelectedProject] = useState(0);
  const [selectedRole, setSelectedRole] = useState("all");
  const [selectedMrId, setSelectedMrId] = useState(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [sortBy, setSortBy] = useState("unread");
  const [toast, setToast] = useState(null);
  const [notifPermission, setNotifPermission] = useState(null);
  const [ctxMenu, setCtxMenu] = useState(null);
  const [customReminderState, setCustomReminderState] = useState({ open: false, mrId: null, position: null });
  const [closingPanel, setClosingPanel] = useState(false);
  const [appVersion, setAppVersion] = useState("0.0.0");
  const toastTimer = useRef(null);

  useEffect(() => { invoke("get_app_version").then(setAppVersion); }, []);

  const showToast = useCallback((msg, duration = 2500) => {
    setToast(msg);
    clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), duration);
  }, []);

  const {
    settings,
    loading: settingsLoading,
    updateSettings,
    saveSettings,
    testConnection,
  } = useSettings(showToast);

  const gitlab = useGitlab(showToast);

  const mergeRequests = gitlab.mergeRequests;
  const projects = gitlab.projects;

  useEffect(() => {
    if (settings.connected) {
      gitlab.fetchData();
      gitlab.startPolling();
      return () => { gitlab.stopPolling(); };
    }
  }, [settings.connected]);

  useEffect(() => {
    if (!settingsLoading && !settings.connected) {
      setView("settings");
    }
  }, [settingsLoading, settings.connected]);

  useEffect(() => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (!invoke) return;
    let interval = null;

    invoke("prompt_notification_permission").catch(() => {});

    const check = () =>
      invoke("check_notification_permission").then((granted) => {
        setNotifPermission(granted);
        if (granted && interval) {
          clearInterval(interval);
          interval = null;
        }
        return granted;
      });

    check().then((granted) => {
      if (!granted) {
        showToast("Notifications are disabled. Enable in System Settings \u2192 Notifications.", 5000);
        interval = setInterval(check, 5000);
      }
    }).catch(() => {});

    return () => { if (interval) clearInterval(interval); };
  }, [showToast]);

  // listen for tray menu "Settings..." navigation
  useEffect(() => {
    const listenPromise = typeof window !== "undefined" && window.__TAURI_INTERNALS__
      ? import("@tauri-apps/api/event").then((m) => m.listen)
      : null;
    if (!listenPromise) return;

    let unlisten;
    listenPromise.then((listen) => {
      listen("navigate", (event) => {
        if (event.payload === "settings") {
          setView("settings");
          setSelectedMrId(null);
        }
      }).then((fn) => { unlisten = fn; });
    });

    return () => { if (unlisten) unlisten(); };
  }, []);

  const toggleUnread = useCallback((id) => {
    gitlab.toggleUnread(id);
  }, [gitlab.toggleUnread]);

  const closePanel = useCallback(() => {
    setClosingPanel((prev) => prev || true);
  }, []);

  const onPanelAnimDone = useCallback(() => {
    setClosingPanel(false);
    setSelectedMrId(null);
  }, []);

  const handleSelectMr = useCallback((id) => {
    if (id === null || id === selectedMrId) {
      if (selectedMrId) closePanel();
      return;
    }
    setClosingPanel(false);
    setSelectedMrId(id);
  }, [selectedMrId, closePanel]);

  const openGitLab = useCallback((mr) => {
    gitlab.openGitLab(mr);
  }, [gitlab.openGitLab]);

  const handleSetReminder = useCallback((id, label, isoDate) => {
    gitlab.setReminder(id, label, isoDate);
  }, [gitlab.setReminder]);

  const handleClearReminder = useCallback((id) => {
    gitlab.clearReminder(id);
  }, [gitlab.clearReminder]);

  const handleContextMenu = useCallback((e, mrId) => {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ mrId, x: e.clientX, y: e.clientY });
  }, []);

  const handleRemindClick = useCallback((mrId) => {
    setCtxMenu({ mrId, x: 400, y: 300 });
  }, []);

  const handleCustomReminder = useCallback((mrId) => {
    setCustomReminderState({ open: true, mrId, position: { x: 400, y: 200 } });
  }, []);

  // sync tray badge whenever totalUnread changes
  const totalUnread = mergeRequests.filter((m) => m.unread).length;

  useEffect(() => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (invoke) {
      invoke("update_tray_badge", { count: totalUnread }).catch(console.error);
    }
  }, [totalUnread]);

  // Cmd+, / Ctrl+, to open settings
  useEffect(() => {
    function handleKeyDown(e) {
      if (e.key === "," && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setView("settings");
        setSelectedMrId(null);
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  const countForProject = (pid) => {
    const list = pid > 0 ? mergeRequests.filter((m) => getProjectId(m) === pid) : mergeRequests;
    return list.filter((m) => m.unread).length;
  };

  const ctxMr = ctxMenu ? mergeRequests.find((m) => m.id === ctxMenu.mrId) : null;

  return (
    <div className="app" onClick={() => { if (selectedMrId) closePanel(); }}>
      <div className="sidebar">
        <div className="sidebar-header">Inbox</div>
        <div className="sidebar-section">
          <div
            className={`sidebar-all ${view === "inbox" && selectedProject === 0 ? "active" : ""}`}
            onClick={() => { setView("inbox"); setSelectedProject(0); setSelectedMrId(null); }}
          >
            <div className="sa-icon">{"\u229E"}</div>
            <div className="si-info"><div className="si-name">All projects</div></div>
            {totalUnread > 0 && <div className="sa-count">{totalUnread}</div>}
          </div>
        </div>

        <div className="sidebar-header">Projects</div>
        <div className="sidebar-section">
          {projects.length === 0 && !settings.connected && (
            <div style={{ padding: "8px 16px", fontSize: 12, color: "var(--text-tertiary)" }}>
              Configure GitLab in Settings to see projects
            </div>
          )}
          {projects.map((p) => {
            const c = countForProject(p.id);
            return (
              <div
                key={p.id}
                className={`sidebar-item ${view === "inbox" && selectedProject === p.id ? "active" : ""}`}
                onClick={() => { setView("inbox"); setSelectedProject(p.id); setSelectedMrId(null); }}
              >
                <div className="si-avatar" style={{ background: p.color }}>{p.initials}</div>
                <div className="si-info">
                  <div className="si-name">{p.name}</div>
                  <div className="si-ns">{p.namespace}</div>
                </div>
                {c > 0 && <div className="si-count">{c}</div>}
              </div>
            );
          })}
        </div>

        {gitlab.offline && (
          <div style={{ padding: "4px 16px", fontSize: 11, color: "var(--orange)", fontWeight: 500 }}>
            {"\u26A0"} Offline
          </div>
        )}

        <div className="sidebar-bottom">
          <div
            className={`sidebar-settings ${view === "settings" ? "active" : ""}`}
            onClick={() => { setView("settings"); setSelectedMrId(null); }}
          >
            <div className="ss-icon">{"\u2699"}</div>
            <div className="si-info"><div className="si-name">Settings</div></div>
          </div>
        </div>
      </div>

      {view === "settings" ? (
        <SettingsView
          settings={settings}
          onUpdate={updateSettings}
          onTestConnection={testConnection}
          onSave={saveSettings}
          notifPermission={notifPermission}
          onNotifPermissionChange={setNotifPermission}
          showToast={showToast}
          appVersion={appVersion}
        />
      ) : !settings.connected ? (
        <div className="main">
          <div className="topbar">
            <div className="topbar-title">All merge requests</div>
          </div>
          <div className="empty-state" style={{ marginTop: 120 }}>
            <div className="empty-icon">{"\u2699"}</div>
            <div className="empty-title">Not connected</div>
            <div className="empty-desc">Configure your GitLab connection in Settings to get started</div>
            <button className="dp-btn primary" style={{ marginTop: 16 }} onClick={() => setView("settings")}>
              Open Settings
            </button>
          </div>
        </div>
      ) : (
        <InboxView
          mergeRequests={mergeRequests}
          projects={projects}
          selectedProject={selectedProject}
          selectedRole={selectedRole}
          selectedMrId={selectedMrId}
          searchQuery={searchQuery}
          loading={gitlab.loading}
          lastChecked={gitlab.lastChecked}
          checking={gitlab.checking}
          closingPanel={closingPanel}
          onClosePanel={closePanel}
          onPanelAnimDone={onPanelAnimDone}
          onSelectProject={setSelectedProject}
          onSelectRole={setSelectedRole}
          onSelectMr={handleSelectMr}
          onSearch={setSearchQuery}
          onContextMenu={handleContextMenu}
          onToggleUnread={toggleUnread}
          onOpenGitLab={openGitLab}
          onRemindClick={handleRemindClick}
          onCheckNow={gitlab.checkNow}
          sortBy={sortBy}
          onSortChange={setSortBy}
        />
      )}

      <Toast message={toast} />
      <ContextMenu
        mr={ctxMr}
        position={ctxMenu}
        onClose={() => setCtxMenu(null)}
        onToggleUnread={toggleUnread}
        onSetReminder={handleSetReminder}
        onClearReminder={handleClearReminder}
        onOpenGitLab={openGitLab}
        onCustomReminder={handleCustomReminder}
      />
      <CustomReminderModal
        isOpen={customReminderState.open}
        position={customReminderState.position}
        onClose={() => setCustomReminderState({ open: false, mrId: null, position: null })}
        onConfirm={(display, isoDate) => {
          handleSetReminder(customReminderState.mrId, display, isoDate);
        }}
      />
    </div>
  );
}
