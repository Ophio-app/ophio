// SPDX-License-Identifier: Apache-2.0
// Adding and removing previews, which Control alone manages.
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useWarning } from "../../app/warnings";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { commandResult } from "../../host/commandResult";
import type { Command, PreviewEndpoint } from "../../protocol/types";
import { classifyPreviewUrl } from "../settings/helpers";

interface Draft { project: string; label: string; url: string }
const EMPTY: Draft = { project: "", label: "", url: "" };

/** A half-typed preview is kept on this device until it is added or cleared.
 * Older versions kept it with the Connections forms. */
function readDraft(profileId: string): Draft {
  for (const key of [`preview-draft.${profileId}`, `connections-form.${profileId}`]) {
    try {
      const saved = localStorage.getItem(key);
      if (saved) {
        const { project = "", label = "", url = "" } = JSON.parse(saved) as Partial<Draft>;
        return { project, label, url };
      }
    } catch { /* Unreadable: start empty. */ }
  }
  return EMPTY;
}

/** Runs one preview command to its result, with what went wrong if it didn't. */
function usePreviewCommand() {
  const { session, profile } = useApp();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (body: Command, label: string): Promise<boolean> => {
    setPending(true);
    setError(null);
    try {
      await commandResult(session, await session.command(body, { label }));
      return true;
    } catch (e) {
      setError(describeError(e, profile.name));
      return false;
    } finally {
      setPending(false);
    }
  };
  return { run, pending, error, setError };
}

/** `projectId`, the project Control shows, is chosen when the sheet opens
 * unless a kept draft has a label or address already. */
export function AddPreviewSheet({ open, onClose, projectId }: { open: boolean; onClose(): void; projectId?: string | null }) {
  const { profile } = useApp();
  const online = useLink().state === "online";
  const projects = useHost((s) => s.projects);
  const admin = useHost((s) => !!s.grants?.admin);
  const { run, pending, error } = usePreviewCommand();
  const [form, setForm] = useState<Draft>(() => readDraft(profile.profileId));
  const [warning, setWarning] = useState<string | null>(null);
  useEffect(() => {
    if (open && projectId) setForm((f) => (f.label || f.url ? f : { ...f, project: projectId }));
  }, [open, projectId]);
  const keep = (next: Draft) => {
    setForm(next);
    try {
      localStorage.setItem(`preview-draft.${profile.profileId}`, JSON.stringify(next));
    } catch {
      setWarning("This draft isn’t saved on this device. Keep this sheet open until you finish.");
    }
  };
  const patch = (value: Partial<Draft>) => keep({ ...form, ...value });
  // A kept draft can name a project the computer no longer has; the list
  // would show the first project while nothing is chosen.
  const project = projects[form.project] ? form.project : Object.keys(projects)[0] ?? "";
  const url = classifyPreviewUrl(form.url);
  const close = () => { if (!pending) onClose(); };
  return (
    <div onPointerDownCapture={(e) => {
      // A tap outside while typing only puts the keyboard away.
      if (!(e.target instanceof HTMLDialogElement)) return;
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && e.target.contains(focused) && focused.matches("input, textarea, select")) e.preventDefault();
    }}>
      <Sheet
        open={open}
        onClose={close}
        title="Add preview"
        footer={<>
          <Button
            variant="primary"
            pending={pending}
            disabled={!online || !admin || !form.label.trim() || !url.valid || !project}
            onClick={async () => {
              if (!url.valid) return;
              const added = await run({ command: "register_preview", project_id: project, label: form.label.trim(), url: url.url }, `Add preview ${form.label}`);
              if (added) { keep(EMPTY); onClose(); }
            }}
          >
            Add preview
          </Button>
          <Button variant="quiet" disabled={pending} onClick={() => { keep(EMPTY); onClose(); }}>Cancel and clear draft</Button>
        </>}
      >
        <div className="settings-stack">
          {!online && <Notice>Connect to add this preview. Your draft stays on this device.</Notice>}
          {online && !admin && <Notice>This device doesn’t have permission to add previews.</Notice>}
          <label className="field">
            <span className="field__label">Project</span>
            <select className="field__input" value={project} disabled={pending} onChange={(e) => patch({ project: e.target.value })}>
              {Object.values(projects).map((p) => (
                <option key={p.project_id} value={p.project_id}>{p.name}</option>
              ))}
            </select>
          </label>
          <TextField disabled={pending} label="Preview label" value={form.label} onChange={(label) => patch({ label })} />
          <TextField
            disabled={pending}
            label="Preview URL"
            value={form.url}
            onChange={(url) => patch({ url })}
            inputMode="url"
            error={form.url && !url.valid ? url.reason : null}
          />
          {url.valid && !url.privateHost && (
            <Notice tone="warn" title="This looks like a public URL">
              It isn’t a private-network, tailnet or localhost address.
              Add it only if you intend to open this site. The computer
              may refuse it under its preview policy.
            </Notice>
          )}
          <p className="fine">
            A localhost address opens privately in this app, reaching the
            computer’s own localhost. Other addresses open in your device’s
            browser. Closing keeps this draft on this device; Cancel clears it.
          </p>
          {warning && <Notice tone="warn">{warning}</Notice>}
          {error && <Notice tone="stop">{error}</Notice>}
        </div>
      </Sheet>
    </div>
  );
}

/** Remove, with the warning unless it was turned off. */
export function useRemovePreview() {
  const warn = useWarning("preview.remove");
  const admin = useHost((s) => !!s.grants?.admin);
  const online = useLink().state === "online";
  const { run, pending, error, setError } = usePreviewCommand();
  const [asking, setAsking] = useState<PreviewEndpoint | null>(null);
  const remove = async (p: PreviewEndpoint) => {
    if (await run({ command: "remove_preview", preview_id: p.preview_id }, `Remove ${p.label}`)) setAsking(null);
  };
  return {
    allowed: online && admin,
    ask: (p: PreviewEndpoint) => warn.ask(() => { setError(null); setAsking(p); }, () => void remove(p)),
    /** A refusal from removing without the sheet. */
    error: asking ? null : error,
    sheet: (
      <Sheet
        open={!!asking}
        onClose={() => { if (!pending) setAsking(null); }}
        title={asking ? `Remove ${asking.label}?` : "Remove preview"}
        footer={asking && (
          <Button variant="stop" pending={pending} onClick={() => { warn.agreed(); void remove(asking); }}>
            Remove {asking.label}
          </Button>
        )}
      >
        {asking && <p>
          Remove {asking.label} (<span className="mono">{asking.url}</span>) from
          the list. Its server keeps running.{" "}
          {asking.automatic ? "It stays off the list while this server runs, and is listed again the next time it starts." : "You can add its address again."}
        </p>}
        {warn.check}
        {error && <Notice tone="stop">{error}</Notice>}
      </Sheet>
    ),
  };
}
