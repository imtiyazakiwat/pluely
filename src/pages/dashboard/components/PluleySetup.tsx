import React, { useCallback, useEffect, useRef, useState } from "react";
import { CheckIcon, KeyIcon, LoaderCircleIcon, ZapIcon } from "lucide-react";
import { useApp } from "@/contexts";
import { Button, Input } from "@/components";
import { PLUELY_PROVIDER_ID } from "@/config";
import {
  downloadLocalSttModels,
  getLocalSttStatus,
  setLocalSttEnabled,
} from "@/lib/functions/local-stt.function";
import { safeLocalStorage } from "@/lib";
import { STORAGE_KEYS } from "@/config";

const PLUELY_DEFAULT_MODEL = "gemini-3.5-flash-lite";

/**
 * PluleySetup — first-run onboarding card.
 *
 * Shows when:
 *  - The selected AI provider is "pluely", AND
 *  - No API key has been saved yet.
 *
 * The user pastes their Pluely API key and clicks "Get started".
 * That's it. On-device STT models are downloaded silently in the background
 * so speech recognition is ready by the time the user opens the mic.
 */
export const PluleySetup = () => {
  const { selectedAIProvider, onSetSelectedAIProvider } = useApp();

  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [done, setDone] = useState(false);
  const [sttDownloading, setSttDownloading] = useState(false);
  const [localSttAvailable, setLocalSttAvailable] = useState(false);
  const downloadStarted = useRef(false);

  // Probe whether on-device STT is available in this build
  useEffect(() => {
    getLocalSttStatus()
      .then((s) => setLocalSttAvailable(!!s?.available))
      .catch(() => setLocalSttAvailable(false));
  }, []);

  // Show only when Pluely is the active provider and no API key is set
  const isPluely = selectedAIProvider?.provider === PLUELY_PROVIDER_ID;
  const apiKeyAlreadySaved = !!(
    selectedAIProvider?.variables?.["api_key"] || ""
  ).trim();

  if (!isPluely || apiKeyAlreadySaved) return null;

  const startSttDownload = useCallback(async () => {
    if (!localSttAvailable || downloadStarted.current) return;
    try {
      const status = await getLocalSttStatus();
      if (status?.modelsReady || status?.downloading) return;
      downloadStarted.current = true;
      setSttDownloading(true);
      await downloadLocalSttModels();
      // Enable on-device STT automatically once models are ready
      setLocalSttEnabled(true);
    } catch {
      // Non-fatal — the user can always download models manually later
    } finally {
      setSttDownloading(false);
    }
  }, [localSttAvailable]);

  const handleSubmit = async () => {
    const trimmed = apiKey.trim();
    if (!trimmed) return;

    setSaving(true);
    try {
      // Save the provider + API key
      onSetSelectedAIProvider({
        provider: PLUELY_PROVIDER_ID,
        variables: {
          api_key: trimmed,
          model: PLUELY_DEFAULT_MODEL,
        },
      });

      // Persist setup-done flag so the card disappears on reload too
      safeLocalStorage.setItem(STORAGE_KEYS.PLUELY_SETUP_DONE, "true");

      setDone(true);

      // Kick off on-device STT download silently in the background
      startSttDownload();
    } finally {
      setSaving(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") handleSubmit();
  };

  if (done) {
    return (
      <div className="rounded-xl border border-green-200 bg-green-50 dark:border-green-800 dark:bg-green-950 p-4 flex items-center gap-3">
        <div className="flex-shrink-0 h-8 w-8 rounded-full bg-green-500 flex items-center justify-center">
          <CheckIcon className="h-4 w-4 text-white" />
        </div>
        <div>
          <p className="text-sm font-semibold text-green-800 dark:text-green-200">
            You're all set!
          </p>
          <p className="text-xs text-green-700 dark:text-green-400">
            Pluely is ready.
            {sttDownloading
              ? " On-device speech recognition is downloading in the background…"
              : localSttAvailable
              ? " On-device speech recognition is enabled."
              : ""}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-primary/20 bg-gradient-to-br from-primary/5 via-background to-primary/5 p-5 space-y-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <div className="flex-shrink-0 h-9 w-9 rounded-lg bg-primary/10 flex items-center justify-center">
          <ZapIcon className="h-5 w-5 text-primary" />
        </div>
        <div>
          <h3 className="text-sm font-semibold">Welcome to Pluely</h3>
          <p className="text-xs text-muted-foreground">
            One-click setup — just paste your API key below.
          </p>
        </div>
      </div>

      {/* What's included callout */}
      <div className="bg-muted/60 rounded-lg px-4 py-3 space-y-1.5 text-xs text-muted-foreground">
        <p className="text-foreground font-medium text-xs mb-1">
          What gets configured automatically:
        </p>
        <div className="flex items-center gap-2">
          <CheckIcon className="h-3.5 w-3.5 text-primary flex-shrink-0" />
          <span>
            Pluely backend API (
            <code className="text-[10px] bg-muted px-1 rounded">
              sss-pluely-backend.vercel.app
            </code>
            )
          </span>
        </div>
        <div className="flex items-center gap-2">
          <CheckIcon className="h-3.5 w-3.5 text-primary flex-shrink-0" />
          <span>Default model: {PLUELY_DEFAULT_MODEL}</span>
        </div>
        {localSttAvailable && (
          <div className="flex items-center gap-2">
            <CheckIcon className="h-3.5 w-3.5 text-primary flex-shrink-0" />
            <span>
              On-device speech recognition (downloads silently, no extra steps)
            </span>
          </div>
        )}
      </div>

      {/* API Key input */}
      <div className="space-y-1.5">
        <label className="text-xs font-medium">
          Pluely API Key{" "}
          <span className="text-muted-foreground font-normal">
            — stored locally, never shared
          </span>
        </label>
        <div className="flex gap-2">
          <Input
            type="password"
            placeholder="sk-…  or your Pluely API key"
            value={apiKey}
            onChange={(v) =>
              setApiKey(typeof v === "string" ? v : v.target.value)
            }
            onKeyDown={handleKeyDown}
            disabled={saving}
            className="flex-1 h-11 border border-input/50 focus:border-primary/50 transition-colors"
          />
          <Button
            onClick={handleSubmit}
            disabled={saving || !apiKey.trim()}
            size="icon"
            className="shrink-0 h-11 w-11"
            title="Activate Pluely"
          >
            {saving ? (
              <LoaderCircleIcon className="h-4 w-4 animate-spin" />
            ) : (
              <KeyIcon className="h-4 w-4" />
            )}
          </Button>
        </div>
      </div>

      <p className="text-[11px] text-muted-foreground">
        Press <kbd className="text-[10px] border border-input/50 rounded px-1 py-0.5 bg-muted">Enter</kbd> or click the key icon to get started. That's the only step.
      </p>
    </div>
  );
};
