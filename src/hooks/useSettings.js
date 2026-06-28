import { useState, useEffect, useCallback } from "react";

function getTauriInvoke() {
  if (typeof window !== "undefined" && window.__TAURI_INTERNALS__) {
    return window.__TAURI_INTERNALS__.invoke;
  }
  return null;
}

const DEFAULT_SETTINGS = {
  url: "",
  token: "",
  pollInterval: "30",
  showDrafts: true,
  desktopNotif: true,
  soundNotif: true,
  connected: false,
  tokenVisible: false,
};

export function useSettings(showToast) {
  const [settings, setSettings] = useState(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const invoke = getTauriInvoke();
    if (!invoke) {
      setLoading(false);
      return;
    }
    invoke("get_settings")
      .then((s) => {
        setSettings((prev) => ({
          ...prev,
          url: s.url || "",
          token: s.token || "",
          pollInterval: s.pollInterval || "30",
          showDrafts: s.showDrafts ?? true,
          desktopNotif: s.desktopNotif ?? true,
          soundNotif: s.soundNotif ?? true,
          connected: s.connected ?? false,
        }));
      })
      .catch((e) => {
        console.error("Failed to load settings:", e);
        showToast(`Failed to load settings: ${e}`);
      })
      .finally(() => setLoading(false));
  }, []);

  const updateSettings = useCallback((newSettings) => {
    setSettings(newSettings);
  }, []);

  // persists non-identity settings (interval, drafts, notifications). the
  // identity is committed only by connect, so this never touches url/token.
  const savePreferences = useCallback(
    async (overrides) => {
      const invoke = getTauriInvoke();
      if (!invoke) {
        showToast("Tauri not available. Cannot save settings.");
        return;
      }
      const s = overrides || settings;
      try {
        await invoke("save_preferences", {
          settings: {
            url: s.url,
            token: s.token,
            pollInterval: s.pollInterval,
            showDrafts: s.showDrafts,
            desktopNotif: s.desktopNotif,
            soundNotif: s.soundNotif,
            connected: s.connected,
          },
        });
      } catch (e) {
        showToast(`Error: ${e}`);
      }
    },
    [settings, showToast],
  );

  // validates and commits the identity in one step; the backend restarts polling
  // on success. returns true so the caller can refresh the view.
  const connect = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) {
      showToast("Tauri not available. Cannot connect.");
      return false;
    }

    showToast("Connecting...");
    try {
      const user = await invoke("connect", {
        url: settings.url,
        token: settings.token,
      });
      setSettings((prev) => ({ ...prev, connected: true }));
      showToast(`Connected as ${user.name}`);
      return true;
    } catch (e) {
      setSettings((prev) => ({ ...prev, connected: false }));
      showToast(`Connection failed: ${e}`);
      return false;
    }
  }, [settings.url, settings.token, showToast]);

  const disconnect = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) {
      showToast("Tauri not available.");
      return;
    }
    try {
      await invoke("disconnect");
      setSettings((prev) => ({ ...prev, token: "", connected: false }));
      showToast("Disconnected");
    } catch (e) {
      showToast(`Error: ${e}`);
    }
  }, [showToast]);

  return {
    settings,
    loading,
    updateSettings,
    savePreferences,
    connect,
    disconnect,
  };
}
