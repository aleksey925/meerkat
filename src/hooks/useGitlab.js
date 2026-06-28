import { useState, useEffect, useCallback, useRef } from "react";

function getTauriInvoke() {
  if (typeof window !== "undefined" && window.__TAURI_INTERNALS__) {
    return window.__TAURI_INTERNALS__.invoke;
  }
  return null;
}

function getTauriListen() {
  if (typeof window !== "undefined" && window.__TAURI_INTERNALS__) {
    return import("@tauri-apps/api/event").then((m) => m.listen);
  }
  return null;
}

export function useGitlab(showToast) {
  const [mergeRequests, setMergeRequests] = useState([]);
  const [projects, setProjects] = useState([]);
  // the backend polls on launch when connected, so start in the loading state
  // and let the first mr-update (or connection-error) clear it
  const [loading, setLoading] = useState(true);
  const [offline, setOffline] = useState(false);
  const [lastChecked, setLastChecked] = useState(null);
  const [checking, setChecking] = useState(false);
  const manualCheckPending = useRef(false);

  useEffect(() => {
    const listenPromise = getTauriListen();
    if (!listenPromise) return;

    let unlistenMr, unlistenErr, unlistenReminders;

    listenPromise.then((listen) => {
      listen("mr-update", (event) => {
        const payload = event.payload;
        setMergeRequests(payload.active);
        setProjects(payload.projects);
        setOffline(false);
        setLastChecked(new Date());
        setLoading(false);
        setChecking(false);
        if (manualCheckPending.current) {
          manualCheckPending.current = false;
          showToast("Updated just now");
        }
      }).then((fn) => {
        unlistenMr = fn;
      });

      listen("connection-error", (event) => {
        setLoading(false);
        setChecking(false);
        manualCheckPending.current = false;
        // toast only on the offline transition; polling re-fires this every
        // interval while the outage lasts and would otherwise spam the user
        setOffline((wasOffline) => {
          if (!wasOffline) showToast(`Connection error: ${event.payload}`);
          return true;
        });
      }).then((fn) => {
        unlistenErr = fn;
      });

      listen("reminders-fired", (event) => {
        const firedIds = event.payload;
        setMergeRequests((prev) =>
          prev.map((m) =>
            firedIds.includes(m.id) ? { ...m, reminder: null, unread: true } : m,
          ),
        );
      }).then((fn) => {
        unlistenReminders = fn;
      });
    });

    return () => {
      if (unlistenMr) unlistenMr();
      if (unlistenErr) unlistenErr();
      if (unlistenReminders) unlistenReminders();
    };
  }, [showToast]);

  // clears the view and shows loading while a freshly connected account's first
  // poll cycle loads. the backend owns starting/stopping polling (on
  // connect/disconnect and at launch), so the UI only resets its own state here.
  const prepareReload = useCallback(() => {
    setMergeRequests([]);
    setProjects([]);
    setOffline(false);
    setLoading(true);
  }, []);

  const checkNow = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) return;
    manualCheckPending.current = true;
    setChecking(true);
    try {
      await invoke("check_now");
    } catch (e) {
      setChecking(false);
      manualCheckPending.current = false;
      console.error("Failed to check now:", e);
    }
  }, []);

  const toggleUnread = useCallback(
    async (id) => {
      const invoke = getTauriInvoke();
      if (!invoke) {
        showToast("Tauri not available.");
        return;
      }
      try {
        const newValue = await invoke("toggle_unread", { mrId: id });
        setMergeRequests((prev) =>
          prev.map((m) => (m.id === id ? { ...m, unread: newValue } : m)),
        );
        showToast(newValue ? "Marked as unread" : "Marked as read");
      } catch (e) {
        showToast(`Error: ${e}`);
      }
    },
    [showToast],
  );

  const openGitLab = useCallback(
    async (mr) => {
      const invoke = getTauriInvoke();
      if (!invoke) {
        showToast("Tauri not available. Cannot open browser.");
        return;
      }
      try {
        await invoke("open_in_browser", { url: mr.webUrl });
      } catch (e) {
        showToast(`Failed to open: ${e}`);
      }
    },
    [showToast],
  );

  const setReminder = useCallback(
    async (id, label, isoDate) => {
      const invoke = getTauriInvoke();
      if (!invoke) {
        showToast("Tauri not available.");
        return;
      }
      try {
        await invoke("set_reminder", {
          mrId: id,
          at: isoDate || label,
          label: label,
        });
      } catch (e) {
        showToast(`Error: ${e}`);
        return;
      }
      setMergeRequests((prev) =>
        prev.map((m) => (m.id === id ? { ...m, reminder: label } : m)),
      );
      showToast(`Reminder set: ${label}`);
    },
    [showToast],
  );

  const clearReminder = useCallback(
    async (id) => {
      const invoke = getTauriInvoke();
      if (!invoke) {
        showToast("Tauri not available.");
        return;
      }
      try {
        await invoke("clear_reminder", { mrId: id });
      } catch (e) {
        showToast(`Error: ${e}`);
        return;
      }
      setMergeRequests((prev) =>
        prev.map((m) => (m.id === id ? { ...m, reminder: null } : m)),
      );
      showToast("Reminder removed");
    },
    [showToast],
  );

  return {
    mergeRequests,
    projects,
    loading,
    offline,
    lastChecked,
    checking,
    prepareReload,
    checkNow,
    toggleUnread,
    openGitLab,
    setReminder,
    clearReminder,
  };
}
