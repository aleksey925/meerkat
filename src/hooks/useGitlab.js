import { useState, useEffect, useCallback } from "react";

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
  const [loading, setLoading] = useState(false);
  const [offline, setOffline] = useState(false);
  const [lastChecked, setLastChecked] = useState(null);
  const [checking, setChecking] = useState(false);

  const fetchData = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) {
      showToast("Tauri not available. Cannot fetch data.");
      return;
    }
    setLoading(true);
    try {
      const payload = await invoke("fetch_merge_requests");
      setMergeRequests(payload.active);
      setProjects(payload.projects);
      setOffline(false);
      setLastChecked(new Date());
    } catch (e) {
      console.error("Fetch error:", e);
      if (typeof e === "string" && e.includes("TOKEN_EXPIRED")) {
        showToast("Token expired. Update in Settings.");
      } else {
        setOffline(true);
      }
    } finally {
      setLoading(false);
    }
  }, [showToast]);

  useEffect(() => {
    const listenPromise = getTauriListen();
    if (!listenPromise) return;

    let unlistenMr, unlistenErr, unlistenReminders, unlistenCheckStarted, unlistenCheckFinished, unlistenCheckBusy;

    listenPromise.then((listen) => {
      listen("check-started", () => {
        setChecking(true);
      }).then((fn) => { unlistenCheckStarted = fn; });

      listen("check-finished", (event) => {
        setChecking(false);
        if (event.payload) {
          showToast("Updated just now");
        }
      }).then((fn) => { unlistenCheckFinished = fn; });

      listen("check-already-running", () => {
        showToast("Update already in progress…");
      }).then((fn) => { unlistenCheckBusy = fn; });

      listen("mr-update", (event) => {
        const payload = event.payload;
        setMergeRequests(payload.active);
        setProjects(payload.projects);
        setOffline(false);
        setLastChecked(new Date());
      }).then((fn) => { unlistenMr = fn; });

      listen("connection-error", (event) => {
        showToast(`Connection error: ${event.payload}`);
      }).then((fn) => { unlistenErr = fn; });

      listen("reminders-fired", (event) => {
        const firedIds = event.payload;
        setMergeRequests((prev) =>
          prev.map((m) =>
            firedIds.includes(m.id) ? { ...m, reminder: null, unread: true } : m,
          ),
        );
      }).then((fn) => { unlistenReminders = fn; });
    });

    return () => {
      if (unlistenCheckStarted) unlistenCheckStarted();
      if (unlistenCheckFinished) unlistenCheckFinished();
      if (unlistenCheckBusy) unlistenCheckBusy();
      if (unlistenMr) unlistenMr();
      if (unlistenErr) unlistenErr();
      if (unlistenReminders) unlistenReminders();
    };
  }, [showToast]);

  const startPolling = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) return;
    try {
      await invoke("start_polling");
    } catch (e) {
      console.error("Failed to start polling:", e);
    }
  }, []);

  const stopPolling = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) return;
    try {
      await invoke("stop_polling");
    } catch (e) {
      console.error("Failed to stop polling:", e);
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

  const markAsRead = useCallback(
    async (id) => {
      const invoke = getTauriInvoke();
      if (!invoke) return;
      try {
        await invoke("mark_as_read", { mrId: id });
        setMergeRequests((prev) =>
          prev.map((m) => (m.id === id ? { ...m, unread: false } : m)),
        );
      } catch (e) {
        console.error("Failed to mark as read:", e);
      }
    },
    [],
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

  const checkNow = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) return;
    try {
      await invoke("check_now");
    } catch (e) {
      console.error("Failed to check now:", e);
    }
  }, []);

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
    fetchData,
    startPolling,
    stopPolling,
    toggleUnread,
    markAsRead,
    openGitLab,
    checking,
    checkNow,
    setReminder,
    clearReminder,
  };
}
