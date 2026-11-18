// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useRef, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type { AutoStart, Project } from "../../protocol/types";
import "./autoStartSetting.css";

const TITLE = "Start what the agent proposes without asking";
const DEFAULT: AutoStart = { per_reply: 4, at_once: 4 };
const same = (a: AutoStart | null | undefined, b: AutoStart | null) =>
  (!a && !b) || (!!a && !!b && a.per_reply === b.per_reply && a.at_once === b.at_once);

export function AutoStartSettingRow({ project }: { project: Project }) {
  const shown = useHost((s) => s.capabilities?.features.includes("chat_work_sets_v1") === true && s.grants?.tasks === true && s.grants?.approvals === true);
  const current = useHost((s) => s.projects[project.project_id]);
  const [open, setOpen] = useState(false);
  if (!shown || !current) return null;
  return <>
    <ListRow title={TITLE} subtitle={current.auto_start ? `On, up to ${current.auto_start.at_once} at once` : "Off"} chevron onClick={() => setOpen(true)} />
    {open && <AutoStartSetting key={project.project_id} project={current} onClose={() => setOpen(false)} />}
  </>;
}

/** `before`: the computer's value when this phone asked. Once the computer
 * has accepted, any newer value of its own ends the wait: this phone's, or
 * another device's that came after it. */
type Pending = { target: AutoStart | null; before: AutoStart | null; optimistic: boolean; confirmed: boolean; label: string };

function AutoStartSetting({ project, onClose }: { project: Project; onClose: () => void }) {
  const { session, profile } = useApp();
  const current = useHost((s) => s.projects[project.project_id]);
  const online = useLink().state === "online";
  const allowed = useHost((s) => s.capabilities?.features.includes("chat_work_sets_v1") === true && s.grants?.tasks === true && s.grants?.approvals === true);
  const [pending, setPending] = useState<Pending | null>(null);
  const busy = useRef(false);
  const labelId = useId();
  const actual = current?.auto_start ?? null;
  const visible = pending?.optimistic ? pending.target : actual;
  // Off and on are changes too, not only the limits.
  const actualKey = actual ? `${actual.per_reply}/${actual.at_once}` : "off";

  useEffect(() => {
    if (pending?.confirmed && (same(actual, pending.target) || !same(actual, pending.before))) {
      busy.current = false;
      setPending(null);
    }
    // actualKey stands for actual, which is a new object on every update.
  }, [actualKey, pending]);

  const change = async (target: AutoStart | null) => {
    if (!online || !allowed || busy.current || same(actual, target)) return;
    busy.current = true;
    // Switching on or raising either limit waits for the computer's project update.
    const optimistic = !target || (!!actual && target.per_reply <= actual.per_reply && target.at_once <= actual.at_once);
    const label = target && !actual ? "Turning on…" : "Saving…";
    const before = actual;
    setPending({ target, before, optimistic, confirmed: false, label });
    try {
      const receipt = await session.command({ command: "set_auto_start", project_id: project.project_id, auto_start: target }, { label: "Change automatic starts" });
      await commandResult(session, receipt);
      const now = session.state.get().projects[project.project_id]?.auto_start ?? null;
      if (same(now, target) || !same(now, before)) {
        busy.current = false;
        setPending(null);
      } else {
        setPending({ target, before, optimistic, confirmed: true, label });
      }
    } catch (e) {
      busy.current = false;
      setPending(null);
      toast(describeError(e, profile.name), { tone: "stop" });
    }
  };

  return <Sheet open onClose={onClose} title={TITLE} subtitle={project.name}>
    <div className="auto-start-setting">
      <label className="auto-start-setting__switch">
        <input type="checkbox" role="switch" checked={!!visible} disabled={!online || !allowed || !!pending} aria-labelledby={labelId}
          onChange={(e) => void change(e.target.checked ? DEFAULT : null)} />
        <span id={labelId}>Allow in this project</span>
      </label>
      <p className="auto-start-setting__explain">Work the agent proposes while answering your message starts without your tap. Only from Work chats with full access, and never while Next up is paused. You can stop it from its card, and it always stops at its pull request.</p>
      {pending && <p role="status" className="auto-start-setting__pending">{pending.label}</p>}
      {visible && <div className="auto-start-setting__limits">
        <Limit label="Tasks one reply may start" value={visible.per_reply} disabled={!online || !allowed || !!pending}
          onChange={(n) => void change({ ...visible, per_reply: n })} />
        <Limit label="Running at once" value={visible.at_once} disabled={!online || !allowed || !!pending}
          onChange={(n) => void change({ ...visible, at_once: n })} />
      </div>}
    </div>
  </Sheet>;
}

function Limit({ label, value, disabled, onChange }: { label: string; value: number; disabled: boolean; onChange: (n: number) => void }) {
  return <div className="auto-start-setting__limit">
    <span>{label}</span>
    <div className="auto-start-setting__stepper" role="group" aria-label={label}>
      <button type="button" aria-label={`Fewer ${label.toLowerCase()}`} disabled={disabled || value <= 1} onClick={() => onChange(value - 1)}>−</button>
      <output aria-live="polite">{value}</output>
      <button type="button" aria-label={`More ${label.toLowerCase()}`} disabled={disabled || value >= 8} onClick={() => onChange(value + 1)}>+</button>
    </div>
  </div>;
}
