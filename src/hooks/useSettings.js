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

  const saveSettings = useCallback(async (overrides) => {
    const invoke = getTauriInvoke();
    if (!invoke) {
      showToast("Tauri not available. Cannot save settings.");
      return;
    }
    const s = overrides || settings;
    try {
      await invoke("save_settings", {
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
      if (!overrides) {
        showToast("Settings saved");
      }
    } catch (e) {
      showToast(`Error: ${e}`);
    }
  }, [settings, showToast]);

  const testConnection = useCallback(async () => {
    const invoke = getTauriInvoke();
    if (!invoke) {
      showToast("Tauri not available. Cannot test connection.");
      return;
    }

    showToast("Testing connection...");
    try {
      const user = await invoke("test_connection", {
        url: settings.url,
        token: settings.token,
      });
      setSettings((prev) => ({ ...prev, connected: true }));
      showToast(`Connected as ${user.name}`);
    } catch (e) {
      setSettings((prev) => ({ ...prev, connected: false }));
      showToast(`Connection failed: ${e}`);
    }
  }, [settings.url, settings.token, showToast]);

  return {
    settings,
    loading,
    updateSettings,
    saveSettings,
    testConnection,
  };
}
