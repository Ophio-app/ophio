// SPDX-License-Identifier: Apache-2.0
import { CircleCheck } from "lucide-react";
import { useEffect, useState } from "react";
import { describeError, useApp, useHost } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { CHECK_WORD, remedyText } from "../../design/remedy";
import { Sheet } from "../../design/Sheet";
import {
  annotationStore,
  blobs,
  editedSinceSent,
} from "../../state/annotations";
import { useStore } from "../../state/store";
import { saveVoicePreferences, voicePreferences } from "./preferences";
import { saveBlob, plural } from "./helpers";
import { useWarning } from "../../app/warnings";
import { VoiceSettings } from "../control/VoiceSettings";
export function DataAndVoice() {
  const { profile, ui, platform } = useApp(),
    readiness = useHost((s) => s.readiness);
  const voice = useStore(voicePreferences(profile.profileId), (v) => v),
    drafts = useStore(annotationStore(profile.profileId), (v) => v);
  const [error, setError] = useState<string | null>(null),
    [clear, setClear] = useState(false),
    [used, setUsed] = useState<number | null>(null),
    [busy, setBusy] = useState(false),
    [exporting, setExporting] = useState(false),
    [saved, setSaved] = useState<string | null>(null);
  const warn = useWarning("drafts.clear");
  const all = Object.values(drafts),
    sent = all.filter((d) => d.sentRevision > 0 && !editedSinceSent(d));
  useEffect(() => {
    let cancelled = false;
    const keys = [
      ...new Set(
        Object.values(drafts).flatMap((d) => [
          ...d.sources.flatMap((s) => (s.blobKey ? [s.blobKey] : [])),
          ...(d.retainedBlobKeys ?? []),
        ]),
      ),
    ];
    void Promise.all(keys.map(async (key) => (await blobs.get(key))?.size ?? 0))
      .then((sizes) => {
        if (!cancelled) setUsed(sizes.reduce((a, b) => a + b, 0));
      })
      .catch(() => {
        if (!cancelled) setUsed(null);
      });
    return () => {
      cancelled = true;
    };
  }, [drafts]);
  const saveVoice = (patch: Partial<typeof voice>) => {
    try {
      saveVoicePreferences(profile.profileId, patch);
      setError(null);
    } catch {
      setError(
        "Voice settings weren’t saved on this device. Free storage and try again.",
      );
    }
  };
  const exportData = async () => {
    // An explicit allowlist: never enumerate localStorage or native profile storage.
    const annotations = all.map((d) => ({
      id: d.id,
      projectId: d.projectId,
      taskId: d.taskId,
      createdAt: d.createdAt,
      updatedAt: d.updatedAt,
      sources: d.sources.map(({ blobKey: _blob, ...s }) => s),
      marks: d.marks,
      comments: d.comments,
      steps: d.steps,
      generalNote: d.generalNote,
      composer: d.composer,
      descriptions: d.descriptions,
      clipEdits: d.clipEdits,
      expected: d.expected,
      actual: d.actual,
      sentRevision: d.sentRevision,
    }));
    const blob = new Blob(
        [
          JSON.stringify(
            {
              version: 1,
              exportedAt: Date.now(),
              computer: profile.name,
              talkDrafts: Object.values(ui.get().drafts),
              annotations,
              voice,
            },
            null,
            2,
          ),
        ],
        { type: "application/json" },
      ),
      name = "phone-data.json";
    setError(null);
    setSaved(null);
    // A phone's web view ignores link downloads; the app saves through the platform.
    if (!platform.saveToPhone) return saveBlob(blob, name);
    setExporting(true);
    try {
      const { savedAs } = await platform.saveToPhone(blob, name);
      setSaved(savedAs);
      announce(`Saved to ${savedAs}`);
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setExporting(false);
    }
  };
  const clearSent = () => {
    setBusy(true);
    setError(null);
    void Promise.all(
      sent.map((d) => annotationStore(profile.profileId).remove(d.id)),
    )
      .then(() => setClear(false))
      .catch(() =>
        setError(
          "Some sources on this device could not be removed. Try again after checking storage.",
        ),
      )
      .finally(() => setBusy(false));
  };
  return (
    <>
      <Section title="Voice" id="settings-voice">
        <div className="settings-card settings-stack">
          {readiness?.checks
            .filter((c) => c.area === "speech")
            .map((c) => (
              <p key={c.label}>
                {c.label} · {CHECK_WORD[c.status]}
                {c.detail ? ` · ${c.detail}` : ""}
                {c.remedy ? ` · ${remedyText(c.remedy)}` : ""}
              </p>
            ))}
          {!readiness?.checks.some((c) => c.area === "speech") && (
            <p>Speaking availability is unknown. Typing stays available.</p>
          )}
          <TextField
            label="Language preference"
            value={voice.language}
            onChange={(language) => saveVoice({ language })}
            hint="Language code, such as en or es. Leave blank to detect it."
          />
          <label className="settings-check">
            <input
              type="checkbox"
              checked={voice.keepAudio}
              onChange={(e) => saveVoice({ keepAudio: e.target.checked })}
            />
            Keep audio on the computer
          </label>
          <p className="settings-meta">
            Off by default. Retained audio is separate from agent requests;
            review sends text and images only.
          </p>
        </div>
      </Section>
      <VoiceSettings />
      <Section title="Device data" id="settings-data">
        <div className="settings-card settings-stack">
          <p>
            {used === null
              ? "Source storage size is unavailable"
              : `${(used / (1024 * 1024)).toFixed(1)} MB in annotation sources`}{" "}
            · {plural(all.length, "draft")}
          </p>
          <Button pending={exporting} onClick={() => void exportData()}>
            Export device data
          </Button>
          {saved && (
            <p className="settings-saved">
              <CircleCheck aria-hidden />
              <span>
                Saved to <span className="settings-saved__path">{saved}</span>
              </span>
            </p>
          )}
          <p className="settings-meta">
            Saves Chat and annotation draft text, source metadata and voice
            preferences to this device’s Downloads. Image and video bytes and
            access credentials are excluded.
          </p>
          <Button
            disabled={!sent.length}
            variant="stop-outline"
            pending={busy && !clear}
            onClick={() => warn.ask(() => setClear(true), clearSent)}
          >
            Clear sent drafts
          </Button>
          {error && <Notice tone="stop">{error}</Notice>}
        </div>
      </Section>
      <Sheet
        open={clear}
        onClose={() => {
          if (!busy) setClear(false);
        }}
        title={`Clear ${plural(sent.length, "sent draft")}?`}
        footer={
          <Button
            pending={busy}
            variant="stop-outline"
            onClick={() => { warn.agreed(); clearSent(); }}
          >
            Clear {plural(sent.length, "sent draft")}
          </Button>
        }
      >
        <p>
          This removes {plural(sent.length, "unchanged sent draft")} and {plural(sent.reduce((n, d) => n + d.sources.length, 0), "local source")} from
          this device. Pending revisions and unsent drafts stay. The computer’s
          copies stay. This cannot be undone.
        </p>
        {warn.check}
        {error && <Notice tone="stop">{error}</Notice>}
      </Sheet>
    </>
  );
}
