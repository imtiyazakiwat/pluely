import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { STORAGE_KEYS } from "@/config";
import {
  LOCAL_STT_CHANGED,
  LOCAL_STT_EVENTS,
  LocalSttDownloadProgress,
  LocalSttStatus,
  deleteLocalSttModels,
  downloadLocalSttModels,
  getLocalSttStatus,
  isLocalSttEnabled,
  setLocalSttEnabled,
} from "@/lib/functions/local-stt.function";

/** Tracks the on-device STT toggle, including changes made in other windows. */
export function useLocalSttEnabled(): boolean {
  const [enabled, setEnabled] = useState(isLocalSttEnabled);

  useEffect(() => {
    const refresh = () => setEnabled(isLocalSttEnabled());
    const onStorage = (e: StorageEvent) => {
      if (e.key === STORAGE_KEYS.LOCAL_STT_ENABLED) refresh();
    };
    window.addEventListener(LOCAL_STT_CHANGED, refresh);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(LOCAL_STT_CHANGED, refresh);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  return enabled;
}

/** Status, model download and the toggle, for the settings card. */
export function useLocalStt() {
  const enabled = useLocalSttEnabled();
  const [status, setStatus] = useState<LocalSttStatus | null>(null);
  const [progress, setProgress] = useState<LocalSttDownloadProgress | null>(
    null
  );
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await getLocalSttStatus());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = listen<LocalSttDownloadProgress>(
      LOCAL_STT_EVENTS.DOWNLOAD,
      (event) => {
        const update = event.payload;
        if (update.finished) {
          setProgress(null);
          if (update.error) setError(update.error);
          refresh();
        } else {
          setProgress(update);
        }
      }
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);

  // Never leave the toggle on when the models are gone.
  useEffect(() => {
    if (status && !status.modelsReady && enabled) setLocalSttEnabled(false);
  }, [status, enabled]);

  const download = useCallback(async () => {
    setError("");
    setBusy(true);
    try {
      await downloadLocalSttModels();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      refresh();
    }
  }, [refresh]);

  const remove = useCallback(async () => {
    setError("");
    setBusy(true);
    try {
      setLocalSttEnabled(false);
      await deleteLocalSttModels();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      refresh();
    }
  }, [refresh]);

  return {
    enabled,
    setEnabled: setLocalSttEnabled,
    status,
    progress,
    error,
    busy,
    download,
    remove,
  };
}
