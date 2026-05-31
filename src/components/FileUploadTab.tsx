import React, { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { Button } from "./ui/Button";

const LABELS = {
  title: "Datei-Upload Transkription",
  description:
    "Wähle eine Audio-Datei aus (MP3, MP4, M4A, WAV, FLAC, OGG) und transkribiere sie nachträglich.",
  chooseFile: "Datei auswählen",
  transcribing: "Transkribiere...",
  selectedFile: "Ausgewählte Datei",
  progress: "Fortschritt",
  resultTitle: "Transkriptionsergebnis",
  copyResult: "Ergebnis kopieren",
  copySuccess: "Transkription wurde in die Zwischenablage kopiert.",
  copyError: "Kopieren in die Zwischenablage fehlgeschlagen.",
  transcribeError: "Die Datei konnte nicht transkribiert werden.",
} as const;

export const FileUploadTab: React.FC = () => {
  const [isTranscribing, setIsTranscribing] = useState(false);
  const [progress, setProgress] = useState(0);
  const [selectedFilePath, setSelectedFilePath] = useState<string | null>(null);
  const [result, setResult] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTranscribing) {
      return;
    }

    const interval = setInterval(() => {
      setProgress((current) => Math.min(current + 5, 90));
    }, 250);

    return () => clearInterval(interval);
  }, [isTranscribing]);

  const selectedFileName = useMemo(() => {
    if (!selectedFilePath) {
      return null;
    }
    return selectedFilePath.split(/[/\\]/).pop() ?? selectedFilePath;
  }, [selectedFilePath]);

  const handleSelectFile = async () => {
    setError(null);

    const selected = await open({
      multiple: false,
      directory: false,
      filters: [
        {
          name: "Audio",
          extensions: ["mp3", "mp4", "m4a", "wav", "flac", "ogg"],
        },
      ],
    });

    const selectedPath = Array.isArray(selected) ? selected[0] : selected;
    if (!selectedPath) {
      return;
    }

    setSelectedFilePath(selectedPath);
    setResult("");
    setProgress(5);
    setIsTranscribing(true);

    try {
      const transcription = await invoke<string>("transcribe_file", {
        filePath: selectedPath,
      });
      setResult(transcription);
      setProgress(100);
    } catch (invokeError) {
      const message =
        invokeError instanceof Error
          ? invokeError.message
          : LABELS.transcribeError;
      setError(message);
      setProgress(0);
    } finally {
      setIsTranscribing(false);
    }
  };

  const handleCopyResult = async () => {
    try {
      await navigator.clipboard.writeText(result);
      toast.success(LABELS.copySuccess);
    } catch {
      toast.error(LABELS.copyError);
    }
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-4">
      <div className="space-y-1">
        <h2 className="text-lg font-semibold">{LABELS.title}</h2>
        <p className="text-sm text-mid-gray">{LABELS.description}</p>
      </div>

      <Button
        variant="primary"
        size="md"
        onClick={handleSelectFile}
        disabled={isTranscribing}
      >
        {isTranscribing ? LABELS.transcribing : LABELS.chooseFile}
      </Button>

      {selectedFileName && (
        <div className="text-sm text-mid-gray">
          {LABELS.selectedFile}:{" "}
          <span className="text-foreground">{selectedFileName}</span>
        </div>
      )}

      {isTranscribing && (
        <div className="space-y-2">
          <div className="flex justify-between text-xs text-mid-gray">
            <span>{LABELS.progress}</span>
            <span>{progress}%</span>
          </div>
          <div className="h-2 w-full rounded bg-mid-gray/20 overflow-hidden">
            <div
              className="h-full bg-logo-primary transition-all duration-200"
              style={{ width: `${progress}%` }}
            />
          </div>
        </div>
      )}

      {error && <div className="text-sm text-red-500">{error}</div>}

      {result && (
        <div className="space-y-2">
          <h3 className="text-sm font-semibold">{LABELS.resultTitle}</h3>
          <div className="text-sm p-3 rounded border border-mid-gray/30 bg-mid-gray/10 whitespace-pre-wrap">
            {result}
          </div>
          <Button variant="secondary" size="sm" onClick={handleCopyResult}>
            {LABELS.copyResult}
          </Button>
        </div>
      )}
    </div>
  );
};
