import { Button, Header, Label, Switch } from "@/components";
import { useLocalStt } from "@/hooks";
import { DownloadIcon, LoaderCircleIcon, TrashIcon } from "lucide-react";

const megabytes = (bytes: number) => `${Math.round(bytes / 1_000_000)} MB`;

/** Settings card for on-device speech recognition (beta). */
export const LocalStt = () => {
  const { enabled, setEnabled, status, progress, error, busy, download, remove } =
    useLocalStt();

  // Builds without the `local-stt` feature have nothing to configure.
  if (status && !status.available) return null;

  const ready = !!status?.modelsReady;
  const downloading = busy || !!status?.downloading || !!progress;
  const percent =
    progress && progress.totalBytes > 0
      ? Math.round((progress.doneBytes / progress.totalBytes) * 100)
      : null;

  return (
    <div id="local-stt" className="space-y-3">
      <Header
        title="On-device speech recognition (beta)"
        description="Transcribe system audio and your microphone on this computer instead of sending audio to an STT provider. English only. If on-device recognition fails, Clocks uses the STT provider below."
      />

      <div className="flex items-center justify-between gap-4">
        <div>
          <Label className="text-sm font-medium">Use on-device recognition</Label>
          <p className="text-xs text-muted-foreground mt-1" aria-live="polite">
            {ready
              ? enabled
                ? "On: audio stays on this device"
                : "Off: audio goes to your STT provider"
              : `Download the models first (${megabytes(status?.totalBytes ?? 0)})`}
          </p>
        </div>
        <Switch
          checked={enabled && ready}
          disabled={!ready || downloading}
          onCheckedChange={setEnabled}
          aria-label="Use on-device speech recognition"
        />
      </div>

      <div className="flex items-center gap-3">
        {ready ? (
          <Button size="sm" variant="outline" onClick={remove} disabled={busy}>
            <TrashIcon className="h-3 w-3 mr-1.5" />
            Delete models
          </Button>
        ) : (
          <Button size="sm" onClick={download} disabled={downloading}>
            {downloading ? (
              <LoaderCircleIcon className="h-3 w-3 mr-1.5 animate-spin" />
            ) : (
              <DownloadIcon className="h-3 w-3 mr-1.5" />
            )}
            <span aria-live="polite">
              {downloading
                ? `Downloading${percent !== null ? ` ${percent}%` : "…"}`
                : `Download models (${megabytes(status?.missingBytes ?? 0)})`}
            </span>
          </Button>
        )}
        <p className="text-[10px] text-muted-foreground">
          Silero VAD · Streaming Zipformer · Smart Turn v3
        </p>
      </div>

      {error ? (
        <p className="text-xs text-destructive" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
};
