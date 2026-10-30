// SPDX-License-Identifier: Apache-2.0
//
// A project's checklist: what the person has to do themselves, as agents
// put it there. Every item is a fold of its own, opened in place, with all
// it takes inside it: each step, the page it opens, the text to copy and
// the file to get. The next item is open when the list opens. Folds open
// and close on their own, and the one tapped stays where it was on the
// screen, so the person never loses their place in the list. One ticked off
// goes down to Done, and the item after it takes its place on the screen.
// An agent waiting on an item starts again the moment it's ticked, and a
// question asked under an item goes to the agent that wrote it.

import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import {
  ArrowRight, Bot, Check, ChevronDown, Clock, Copy, ExternalLink, Eye, FileDown, FolderDown, ListChecks, MessageCircleQuestion, OctagonAlert, RotateCcw, Send, Smartphone, Trash2,
} from "lucide-react";
import {
  allItems, arranged, CHECKLIST_UPDATE, deliveryWords, DELIVER_WAIT_MS, DONE_SECTION, luminance, nextItem, ready, unanswered, waitingOnPerson, useChecklist, useFileDelivery, type Pending,
} from "../../app/checklist";
import { describeError, useApp, useHost } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { TextArea } from "../../design/Field";
import { Empty, ListRow } from "../../design/ListRow";
import { Markdown } from "../../design/Markdown";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import { download } from "../../host/transfer";
import type { Checklist, ChecklistFile, ChecklistItem, ChecklistNote, ChecklistStep, ChecklistTheme, Command, CommandResult, FileDeliveryView, Project } from "../../protocol/types";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import { bytes } from "../library/model";
import { saveBlob } from "../settings/helpers";
import type { RouteProps } from "../types";
import { RequestChecklist } from "./RequestChecklist";
import "./checklist.css";

export type SendFn = (command: Command, label: string, change?: Pending, waitMs?: number) => Promise<CommandResult | null>;

export default function ChecklistScreen({ route }: RouteProps<"checklist">) {
  const { profile } = useApp();
  const project = useHost((s) => s.projects[route.projectId] ?? null);
  const allowed = useMay("tasks");
  const { list, error, reload, send, supported, online } = useChecklist(route.projectId);
  const done = list ? allItems(list).filter((i) => i.owner !== "agent" && i.done_at !== undefined).length : 0;
  const total = list ? allItems(list).filter((i) => i.owner !== "agent").length : 0;
  const subtitle = [project?.name ?? profile.name, list && total > 0 && `${done} of ${total} done`].filter(Boolean).join(" · ");
  return (
    <Screen title={list?.title ?? "Checklist"} subtitle={subtitle} className="checklist-screen">
      {!project ? <Notice tone="unknown">This project isn’t on {profile.name} any more.</Notice>
        : !allowed ? <NotAllowed what="see and tick off checklists" permission="tasks" />
          : !supported ? <Notice tone="warn" title="Update the computer">{CHECKLIST_UPDATE}</Notice>
            : <>
              {error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>}
              {list === undefined && !error && (online
                ? <div className="screen__loading" aria-busy="true" aria-label="Reading the checklist" />
                : <Notice tone="unknown">Connect to {profile.name} to read the checklist.</Notice>)}
              {list === null && (
                <Empty icon={ListChecks} title="No checklist yet" action={<RequestChecklist project={project} />}>
                  When an agent needs you to do something yourself, such as sign in, make a key or choose, it goes here with every step. Ask an agent for one now.
                </Empty>
              )}
              {list && <ChecklistBody list={list} project={project} send={send} focus={route.itemId} />}
            </>}
    </Screen>
  );
}

const FONTS: Record<NonNullable<ChecklistTheme["font"]>, string> = {
  sans: "var(--font-ui)",
  serif: "\"Iowan Old Style\", \"Palatino Linotype\", Palatino, Georgia, serif",
  rounded: "ui-rounded, \"SF Pro Rounded\", Nunito, \"Varela Round\", var(--font-ui)",
  mono: "var(--font-mono)",
};

/** The project's look, laid over Øphio's: the accent fills what is the
 * person's to tap, with text on it in whichever of white or near-black
 * reads, and the surface only behind the heading, in the scheme it suits. */
export function themeStyle(theme: ChecklistTheme | undefined): { style?: CSSProperties; className: string } {
  const accent = luminance(theme?.accent);
  if (!theme || accent === null) return { className: "checklist" };
  const style: Record<string, string> = {
    "--cl-accent": theme.accent,
    // White on the accent when it reads at 4.5:1, else near-black.
    "--cl-on-accent": 1.05 / (accent + 0.05) >= 4.5 ? "#ffffff" : "#14100f",
  };
  if (theme.font) style["--cl-font"] = FONTS[theme.font];
  const surface = luminance(theme.surface);
  let scheme = "";
  if (surface !== null && theme.surface) {
    style["--cl-surface"] = theme.surface;
    scheme = surface > 0.5 ? " checklist--light-surface" : " checklist--dark-surface";
  }
  return { style: style as CSSProperties, className: `checklist checklist--branded${scheme}` };
}

/** One project's whole list, arranged here: open items by urgency under their
 * sections, then the ones done in the last three days. Counts go by all of it. */
export function ChecklistBody({ list, project, send, focus, beforeSections, afterSections }: { list: Checklist; project: Project; send: SendFn; focus?: string; beforeSections?: ReactNode; afterSections?: ReactNode }) {
  const items = allItems(list);
  const next = nextItem(list);
  const [open, setOpen] = useState<ReadonlySet<string>>(() => {
    const first = focus && items.some((i) => i.item_id === focus) ? focus : next?.item_id;
    return new Set(first ? [first] : []);
  });
  // Opened by link or Go to it, an item shows however long ago it was done.
  const [kept, setKept] = useState<ReadonlySet<string>>(() => new Set(focus ? [focus] : []));
  const shown = arranged(list, undefined, { keep: focus && !kept.has(focus) ? new Set(kept).add(focus) : kept });
  const order = allItems(shown);
  const split = shown.sections.filter((s) => s.section_id !== DONE_SECTION).length > 1;
  const heads = useRef(new Map<string, HTMLElement>());
  const scrollTo = useRef<string | null>(focus ?? null);
  /** The heading of the fold just opened or closed, and where it sat. */
  const pinned = useRef<{ id: string; top: number } | null>(null);
  const delivery = useFileDelivery(order.some((i) => i.steps.some((s) => s.file)));
  const [removing, setRemoving] = useState(false);
  const theme = themeStyle(list.theme);
  const personItems = items.filter((i) => i.owner !== "agent");
  const done = personItems.filter((i) => i.done_at !== undefined).length;
  const agentsOpen = items.filter((i) => i.owner === "agent" && i.done_at === undefined).length;
  const waitedOn = new Set(waitingOnPerson(list).map((i) => i.item_id));

  // A fold opened or closed keeps its heading where it was on the screen,
  // or brings it back to the top if the person had scrolled past it, so
  // nothing moves under them. An item opened from elsewhere, or by Next,
  // comes to the top of the screen with its steps below it. Every render,
  // since a tick moves an item without opening or closing a fold.
  useLayoutEffect(() => {
    const pin = pinned.current;
    pinned.current = null;
    const head = pin && heads.current.get(pin.id);
    const scroller = head?.closest<HTMLElement>(".screen__body");
    if (pin && head && scroller) {
      const top = Math.max(pin.top, scroller.getBoundingClientRect().top);
      scroller.scrollTop += head.getBoundingClientRect().top - top;
    }
    const id = scrollTo.current;
    if (!id) return;
    scrollTo.current = null;
    const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
    heads.current.get(id)?.scrollIntoView?.({ block: "start", behavior: reduce ? "auto" : "smooth" });
  });

  // A linked item may arrive in a newer checklist after the cached list.
  const focusHere = !!focus && items.some((i) => i.item_id === focus);
  useEffect(() => {
    if (focus && focusHere) go(focus);
  }, [focus, focusHere, list.project_id]);

  const fold = (id: string, to: boolean) => {
    if (open.has(id) === to) return;
    const head = heads.current.get(id);
    if (head) pinned.current = { id, top: head.getBoundingClientRect().top };
    setOpen((o) => {
      const n = new Set(o);
      if (to) n.add(id); else n.delete(id);
      return n;
    });
  };
  const go = (id: string) => {
    scrollTo.current = id;
    setOpen((o) => new Set(o).add(id));
    setKept((k) => k.has(id) ? k : new Set(k).add(id));
  };
  /** Ticked off: it folds and goes down to Done, nothing else opens, and the
   * open item after it comes up into its place on the screen. */
  const finished = (item: ChecklistItem) => {
    announceFinished(list, item);
    const head = heads.current.get(item.item_id);
    const at = order.findIndex((i) => i.item_id === item.item_id);
    const after = order.slice(at + 1).find((i) => i.done_at === undefined);
    if (head) pinned.current = { id: after?.item_id ?? item.item_id, top: head.getBoundingClientRect().top };
    if (open.has(item.item_id)) setOpen((o) => { const n = new Set(o); n.delete(item.item_id); return n; });
  };
  /** Opened again: it goes back to its section and stays under the finger. */
  const reopened = (item: ChecklistItem) => {
    const head = heads.current.get(item.item_id);
    if (head) pinned.current = { id: item.item_id, top: head.getBoundingClientRect().top };
  };

  return (
    <div className={theme.className} style={theme.style}>
      <header className="cl-hero">
        {list.theme?.logo && <Logo projectId={project.project_id} file={list.theme.logo} name={list.theme.name ?? project.name} />}
        <div className="cl-hero__text">
          <h2 className="cl-hero__title">{list.title}</h2>
          {list.summary && <p className="cl-hero__summary">{list.summary}</p>}
          <div className="cl-progress" role="progressbar" aria-label="Done" aria-valuemin={0} aria-valuemax={personItems.length} aria-valuenow={done}>
            <span className="cl-progress__fill" style={{ transform: `scaleX(${personItems.length ? done / personItems.length : 0})` }} />
          </div>
          <p className="cl-hero__meta">
            {done === personItems.length ? "Everything for you is done." : `${personItems.length - done} for you to do`}
            {agentsOpen > 0 && <span>{agentsOpen} for agent</span>}
            {next && !open.has(next.item_id) && (
              <button type="button" className="cl-jump" onClick={() => go(next.item_id)}>
                Next: {next.title} <ArrowRight aria-hidden size={16} />
              </button>
            )}
          </p>
        </div>
      </header>

      {beforeSections}
      {/* Keep every item a sibling under one list: reprioritizing across
          sections moves its keyed fold without discarding an open AskField. */}
      <ol className="cl-items cl-sectioned-items">
        {shown.sections.flatMap((section, index) => [
          ...((split || section.title) ? [
            <li key={`heading-${index}-${section.section_id}`} className="cl-section__divider" role="presentation">
              <h2 className="cl-section__title">{section.title}</h2>
            </li>,
          ] : []),
          ...section.items.map((item) => (
            <ChecklistItemFold key={item.item_id} item={item} list={list} project={project} send={send} delivery={delivery.view}
              waitedOn={waitedOn.has(item.item_id)} open={open.has(item.item_id)} onToggle={() => fold(item.item_id, !open.has(item.item_id))} onFinished={() => finished(item)}
              onReopened={() => reopened(item)} onGo={go} headRef={(el) => { if (el) heads.current.set(item.item_id, el); else heads.current.delete(item.item_id); }} />
          )),
        ])}
      </ol>

      {afterSections}
      <section className="cl-section cl-about" aria-label="About the whole checklist">
        <h2 className="cl-section__title">Questions</h2>
        <Notes notes={list.notes} />
        <AskField projectId={project.project_id} send={send} label="Ask about the checklist" />
      </section>

      <div className="group cl-doors">
        {delivery.view && (
          <SettingsDoor view={delivery.view} />
        )}
        <ListRow icon={Trash2} title="Remove the checklist" subtitle="Clears it for this project. An agent can make a new one." onClick={() => setRemoving(true)} />
      </div>
      {removing && <RemoveSheet list={list} send={send} onClose={() => setRemoving(false)} />}
    </div>
  );
}

function SettingsDoor({ view }: { view: FileDeliveryView }) {
  const { ui } = useApp();
  return <ListRow icon={FolderDown} title="Where files go" subtitle={deliveryWords(view)} chevron onClick={() => ui.push({ screen: "settings", section: "settings-files" })} />;
}

/** Keep the same immediate acknowledgment in one project and in All. */
function announceFinished(list: Checklist, item: ChecklistItem) {
  const updated = { ...list, sections: list.sections.map((s) => ({ ...s, items: s.items.map((i) =>
    i.item_id === item.item_id ? { ...i, done_at: Date.now() } : i) })) };
  const after = nextItem(updated);
  const remaining = allItems(updated).some((candidate) => candidate.owner !== "agent" && candidate.done_at === undefined);
  const awaited = !!item.waiting || waitingOnPerson(list).some((candidate) => candidate.item_id === item.item_id);
  const words = awaited ? "Done. The agent waiting on it carries on now." : "Done.";
  announce(after ? `${words} Next: ${after.title}`
    : remaining ? `${words} The remaining items are waiting on other to-dos.`
      : `${words} Everything for you is done.`);
  if (awaited) toast("Done. The agent waiting on it carries on now.");
}

/** A checklist item as one fold for the combined All list. Its theme is scoped
 * to this entry; it owns its file-delivery lookup and survives focus changes. */
export function ChecklistEntry({ list, project, item, send, focus, projectLabel, onGo }: {
  list: Checklist; project: Project; item: ChecklistItem; send: SendFn;
  focus?: boolean; projectLabel?: ReactNode; onGo?: (id: string) => void;
}) {
  const [open, setOpen] = useState(!!focus);
  const head = useRef<HTMLElement | null>(null);
  const scrollTo = useRef(!!focus);
  const pinned = useRef<number | null>(null);
  const delivery = useFileDelivery(item.steps.some((step) => !!step.file));
  const theme = themeStyle(list.theme);
  useEffect(() => {
    if (focus) { scrollTo.current = true; setOpen(true); }
  }, [focus]);
  useLayoutEffect(() => {
    const el = head.current;
    if (!el) return;
    if (scrollTo.current) {
      scrollTo.current = false;
      const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
      el.scrollIntoView?.({ block: "start", behavior: reduce ? "auto" : "smooth" });
    } else if (pinned.current !== null) {
      const scroller = el.closest<HTMLElement>(".screen__body");
      if (scroller) scroller.scrollTop += el.getBoundingClientRect().top - Math.max(pinned.current, scroller.getBoundingClientRect().top);
    }
    pinned.current = null;
  }, [open, focus]);
  const toggle = () => {
    pinned.current = head.current?.getBoundingClientRect().top ?? null;
    setOpen((value) => !value);
  };
  return <ChecklistItemFold item={item} list={list} project={project} send={send} delivery={delivery.view}
    waitedOn={waitingOnPerson(list).some((i) => i.item_id === item.item_id)} open={open} onToggle={toggle} onFinished={() => { announceFinished(list, item); setOpen(false); }} onGo={onGo ?? (() => {})}
    headRef={(el) => { head.current = el; }} projectLabel={projectLabel} theme={theme} canGo={!!onGo} />;
}

export function ChecklistItemFold({ item, list, project, send, delivery, open, onToggle, onFinished, onReopened, onGo, headRef, projectLabel, theme, waitedOn = false, canGo = true }: {
  item: ChecklistItem; list: Checklist; project: Project; send: SendFn; delivery: FileDeliveryView | null;
  open: boolean; onToggle: () => void; onFinished: () => void; onReopened?: () => void; onGo: (id: string) => void; headRef: (el: HTMLElement | null) => void; projectLabel?: ReactNode; theme?: ReturnType<typeof themeStyle>; waitedOn?: boolean; canGo?: boolean;
}) {
  const pid = project.project_id;
  const agent = item.owner === "agent";
  const writer = useHost((s) => {
    const waitingWriter = !agent && waitedOn ? allItems(list).find((candidate) =>
      candidate.owner === "agent" && candidate.done_at === undefined && candidate.task_id && candidate.after?.includes(item.item_id))?.task_id : undefined;
    const id = waitingWriter ?? item.task_id;
    return id ? s.tasks[id] : undefined;
  });
  const done = item.done_at !== undefined;
  const blocked = !done && !ready(item, list);
  const asking = !agent && !!item.ask && !item.ask.answer;
  const questions = unanswered(item.notes);
  const bodyId = `cl-i-${project.project_id}-${item.item_id}`;
  const prerequisites = blocked ? allItems(list).filter((i) => item.after?.includes(i.item_id) && i.done_at === undefined) : [];
  const personBefore = prerequisites.find((i) => i.owner !== "agent");
  const firstBefore = agent ? personBefore ?? prerequisites[0] : prerequisites[0];

  const tick = (done: boolean, stepId?: string) => {
    const change: Pending = { kind: "tick", itemId: item.item_id, done, ...(stepId ? { stepId } : {}) };
    const lastStep = stepId !== undefined && done && !asking
      && item.steps.every((s) => s.step_id === stepId || s.done_at !== undefined);
    if ((stepId === undefined && done) || lastStep) onFinished();
    else if (!done && item.done_at !== undefined) onReopened?.();
    send({ command: "checklist_tick", project_id: pid, item_id: item.item_id, ...(stepId ? { step_id: stepId } : {}), done }, done ? `Tick off ${item.title}` : `Open ${item.title} again`, change)
      .catch((e: Error) => toast(e.message, { tone: "stop" }));
  };

  const meta = [
    <Pill key="owner" tone={agent ? "accent" : undefined}>{agent ? "For agent" : "For you"}</Pill>,
    !done && item.priority === "blocker" && <Pill key="p" tone="stop" icon={OctagonAlert}>Blocker</Pill>,
    !done && item.priority === "high" && <Pill key="p" tone="warn">High priority</Pill>,
    !done && item.priority === "low" && <span key="p" className="cl-meta">Low priority</span>,
    item.minutes && <span key="m" className="cl-meta"><Clock aria-hidden size={14} />About {item.minutes} min</span>,
    item.due && !done && <span key="d" className="cl-meta">Aim for {dueWords(item.due)}</span>,
    !agent && waitedOn && !done && <Pill key="w" tone="accent" icon={Bot}>An agent waits on it</Pill>,
    asking && !done && <Pill key="a" tone="warn">Asks you</Pill>,
    questions > 0 && <Pill key="q">Question sent</Pill>,
    blocked && firstBefore && (!agent || !personBefore) && <span key="b" className="cl-meta">{!agent && firstBefore.owner === "agent" ? "Waiting for the agent" : "After"} “{firstBefore.title}”</span>,
    agent && done && <span key="status" className="cl-meta">Done by the agent</span>,
    agent && !done && !blocked && (writer?.state === "working" || writer?.state === "dispatching") && <span key="status" className="cl-meta">An agent is on it</span>,
    agent && !done && !blocked && (writer?.state !== "working" && writer?.state !== "dispatching") && <span key="status" className="cl-meta">No agent is on this now</span>,
  ].filter(Boolean);

  return (
    <li style={theme?.style} className={`${theme?.className ?? ""}${theme ? " cl-entry" : ""} cl-item${open ? " is-open" : ""}${done ? " is-done" : ""}${blocked ? " is-blocked" : ""}`}>
      {projectLabel && <div className="cl-item__project">{projectLabel}</div>}
      <div className="cl-item__head" ref={headRef}>
        {!agent && <button type="button" role="checkbox" aria-checked={done} className="cl-tick"
          aria-label={done ? `Open “${item.title}” again` : asking ? `Answer “${item.title}” first` : `Tick off “${item.title}”`}
          onClick={() => (asking && !done ? (open ? undefined : onToggle()) : tick(!done))}>
          {done && <Check aria-hidden size={18} strokeWidth={3} />}
        </button>}
        <button type="button" className={`cl-item__toggle${agent ? " cl-item__toggle--agent" : ""}`} aria-expanded={open} aria-controls={bodyId} onClick={onToggle}>
          <span className="cl-item__title">{item.title}</span>
          {meta.length > 0 && <span className="cl-item__meta">{meta}</span>}
          <ChevronDown aria-hidden className="cl-item__chevron" size={20} />
        </button>
      </div>
      {agent && personBefore && (canGo
        ? <button className="cl-item__prerequisite" type="button" onClick={() => onGo(personBefore.item_id)}>Waiting on you: {personBefore.title} <ArrowRight aria-hidden size={16} /></button>
        : <span className="cl-item__prerequisite">Waiting on you: {personBefore.title}</span>)}
      {open && (
        <div className="cl-item__body" id={bodyId}>
          {item.why && <p className="cl-why">{item.why}</p>}
          {blocked && firstBefore && (
            <Notice tone="info" action={canGo ? <Button size="sm" onClick={() => onGo(firstBefore.item_id)}>Go to it</Button> : undefined}>
              {agent ? (personBefore ? `Waiting on you to finish “${personBefore.title}”.` : `Waiting on “${firstBefore.title}”.`) : firstBefore.owner === "agent" ? `The agent must finish “${firstBefore.title}” first.` : `Do “${firstBefore.title}” first.`}
            </Notice>
          )}
          {!agent && item.waiting && !done && (
            <p className="cl-waiting"><Bot aria-hidden size={16} />Once it’s done, the agent goes on to {lowerFirst(item.waiting)}</p>
          )}
          {!agent && !item.waiting && waitedOn && !done && writer?.title && (
            <p className="cl-waiting"><Bot aria-hidden size={16} />Once it’s done, the agent on “{writer.title}” carries on.</p>
          )}
          {item.steps.length > 0 && (
            <ol className="cl-steps">
              {item.steps.map((step, n) => (
                <Step key={step.step_id} step={step} n={n + 1} projectId={pid} delivery={delivery}
                  onTick={(d) => tick(d, step.step_id)} readOnly={agent} />
              ))}
            </ol>
          )}
          {!agent && item.ask && <Ask item={item} projectId={pid} send={send} onFinished={onFinished} />}
          <Notes notes={item.notes} />
          <AskField projectId={pid} itemId={item.item_id} send={send} label={agent ? "Send feedback" : "Ask about this"} />
          {!agent && <div className="cl-item__actions">
            {done
              ? <Button variant="quiet" size="sm" icon={RotateCcw} onClick={() => tick(false)}>Open it again</Button>
              : <Button variant="primary" className="cl-primary" icon={Check} disabled={asking}
                onClick={() => tick(true)}>{asking ? "Answer above to finish" : "Done"}</Button>}
          </div>}
        </div>
      )}
    </li>
  );
}

function lowerFirst(s: string): string {
  const t = s.trim().replace(/\.$/, "");
  return `${/^[A-Z][a-z]/.test(t) ? t[0]!.toLowerCase() + t.slice(1) : t}.`;
}

function dueWords(due: string): string {
  const d = new Date(`${due}T12:00:00`);
  if (Number.isNaN(d.getTime())) return due;
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function Step({ step, n, projectId, delivery, onTick, readOnly = false }: { step: ChecklistStep; n: number; projectId: string; delivery: FileDeliveryView | null; onTick: (done: boolean) => void; readOnly?: boolean }) {
  const { platform } = useApp();
  const done = step.done_at !== undefined;
  const open = (url: string) => void platform.openExternal(url).catch(() => toast("That page didn’t open. Copy it instead.", { tone: "stop" }));
  return (
    <li className={`cl-step${done ? " is-done" : ""}`}>
      {readOnly ? <span className="cl-step__marker" aria-label={`Step ${n}${done ? ", done" : ""}`}>
        {done ? <Check aria-hidden size={14} strokeWidth={3} /> : n}
      </span> : <button type="button" role="checkbox" aria-checked={done} className="cl-tick cl-tick--step"
        aria-label={`Step ${n}${done ? ", done" : ""}`} onClick={() => onTick(!done)}>
        {done ? <Check aria-hidden size={14} strokeWidth={3} /> : <span aria-hidden>{n}</span>}
      </button>}
      <div className="cl-step__body">
        <Markdown className="cl-step__text" text={step.text} onLink={open} />
        {step.url && (
          <Button size="sm" icon={ExternalLink} className="cl-step__link" onClick={() => open(step.url!)}>
            Open {hostOf(step.url)}
          </Button>
        )}
        {step.copy && <CopyBox text={step.copy} />}
        {step.file && <FileBox file={step.file} projectId={projectId} delivery={delivery} />}
      </div>
    </li>
  );
}

/** Where a link goes, as a person reads it: the site's name, or the address. */
export function hostOf(url: string): string {
  try {
    const u = new URL(url);
    if (u.protocol === "mailto:") return `email to ${u.pathname}`;
    if (u.protocol === "tel:") return `call ${u.pathname}`;
    return u.hostname.replace(/^www\./, "");
  } catch {
    return "the page";
  }
}

function CopyBox({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(t);
  }, [copied]);
  const copy = () => void navigator.clipboard.writeText(text).then(
    () => { setCopied(true); announce("Copied"); },
    () => toast("Copy is unavailable. Select the text to copy it.", { tone: "stop" }),
  );
  return (
    <div className="cl-copy">
      <code className="cl-copy__text">{text}</code>
      <Button size="sm" icon={copied ? Check : Copy} onClick={copy} aria-label={`Copy ${text.length > 40 ? "it" : text}`}>{copied ? "Copied" : "Copy"}</Button>
    </div>
  );
}

/** The person's way of getting a file, as they chose in Settings. */
function getWords(view: FileDeliveryView | null): string {
  const d = view?.settings.delivery;
  switch (d?.kind) {
    case "taildrop": return `Send to ${d.device}`;
    case "folder": return `Put in ${view!.folders.find((f) => f.path === d.path)?.label ?? "the folder"}`;
    case "drive": return `Copy to ${d.remote}`;
    default: return "Save to this phone";
  }
}

/** What Open can show inside Ophio. Other files can only be saved or sent. */
function viewer(file: ChecklistFile): "image" | "video" | "text" | null {
  if (file.media_type.startsWith("image/")) return "image";
  if (file.media_type.startsWith("video/")) return "video";
  if ((file.media_type.startsWith("text/") || file.media_type === "application/json") && file.size <= 512 * 1024) return "text";
  return null;
}

function FileBox({ file, projectId, delivery }: { file: ChecklistFile; projectId: string; delivery: FileDeliveryView | null }) {
  const { platform, profile, session } = useApp();
  const [busy, setBusy] = useState<"open" | "here" | "there" | null>(null);
  const [got, setGot] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState<{ url: string; text?: string } | null>(null);
  const [broken, setBroken] = useState(false);
  const [actual, setActual] = useState(false);
  // Open and Save share one download.
  const fetched = useRef<Blob | null>(null);
  const transfers = useMay("transfers");
  const elsewhere = !!delivery && delivery.settings.delivery.kind !== "phone";
  const kind = viewer(file);
  useEffect(() => shown ? () => URL.revokeObjectURL(shown.url) : undefined, [shown]);
  const take = async () => {
    if (!fetched.current) {
      const { blob } = await download({ platform, profileId: profile.profileId, session }, { source: { kind: "checklist_file", project_id: projectId, sha256: file.sha256 }, label: `Get ${file.name}` });
      fetched.current = new Blob([blob], { type: file.media_type });
    }
    return fetched.current;
  };
  const open = async () => {
    setBusy("open"); setError(null);
    try {
      const blob = await take();
      const text = kind === "text" ? await blob.text() : undefined;
      setBroken(false); setActual(false);
      setShown({ url: URL.createObjectURL(blob), text });
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally { setBusy(null); }
  };
  const here = async () => {
    setBusy("here"); setError(null);
    try {
      const blob = await take();
      if (platform.saveToPhone) {
        const { savedAs } = await platform.saveToPhone(blob, file.name);
        setGot(`Saved to ${savedAs}`);
      } else {
        saveBlob(blob, file.name);
        setGot("Downloaded");
      }
      announce(`${file.name} saved`);
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally { setBusy(null); }
  };
  const there = async () => {
    setBusy("there"); setError(null);
    try {
      const receipt = await session.command({ command: "checklist_deliver", project_id: projectId, sha256: file.sha256 }, { label: `Send ${file.name}` });
      const r = await commandResult(session, receipt, { waitMs: DELIVER_WAIT_MS });
      const to = r?.kind === "delivered" ? r.to : null;
      setGot(to ? `Sent to ${to}` : "Sent");
      announce(`${file.name} sent`);
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally { setBusy(null); }
  };
  return (
    <div className="cl-file">
      <FileDown aria-hidden className="cl-file__icon" size={22} />
      <div className="cl-file__text">
        <span className="cl-file__name">{file.name}</span>
        <span className="cl-file__meta">{got ?? bytes(file.size)}</span>
      </div>
      <div className="cl-file__actions">
        {elsewhere && <Button size="sm" pending={busy === "there"} disabled={!!busy} onClick={() => void there()}>{getWords(delivery)}</Button>}
        {kind && <Button size="sm" variant={elsewhere ? "quiet" : "secondary"} icon={Eye} pending={busy === "open"} disabled={!!busy || !transfers}
          onClick={() => void open()}>Open</Button>}
        <Button size="sm" variant={elsewhere ? "quiet" : "secondary"} icon={Smartphone} pending={busy === "here"} disabled={!!busy || !transfers}
          onClick={() => void here()}>{elsewhere ? "To this phone" : "Save"}</Button>
      </div>
      {!transfers && !elsewhere && <p className="cl-file__note">This phone may not take files. Allow it Transfers in Settings › Connections, or choose another place in Where files go.</p>}
      {error && !shown && <Notice tone="stop" className="cl-file__error">{error}</Notice>}
      <Sheet open={!!shown} onClose={() => setShown(null)} title={file.name} subtitle={bytes(file.size)} size="full" className="cl-viewer"
        footer={<div className="cl-viewer__foot">
          {error && <Notice tone="stop">{error}</Notice>}
          {got && <p className="fine" role="status">{got}</p>}
          <Button variant="primary" icon={Smartphone} pending={busy === "here"} disabled={!!busy || !transfers} onClick={() => void here()}>Save to this phone</Button>
        </div>}>
        {shown && (broken ? <Notice tone="warn">Ophio can’t show this file. Save it to this phone to open it in another app.</Notice>
          : kind === "text" ? <pre className="cl-viewer__text">{shown.text}</pre>
          : kind === "video" ? <video className="cl-viewer__media" src={shown.url} controls playsInline onError={() => setBroken(true)} />
          : <div className={`cl-viewer__frame${actual ? " is-actual" : ""}`}>
              <button type="button" className="cl-viewer__zoom" aria-label={actual ? "Fit to the screen" : "Show at full size"} onClick={() => setActual((a) => !a)}>
                <img className="cl-viewer__media" src={shown.url} alt={file.name} onError={() => setBroken(true)} />
              </button>
            </div>)}
      </Sheet>
    </div>
  );
}

function Ask({ item, projectId, send, onFinished }: { item: ChecklistItem; projectId: string; send: SendFn; onFinished: () => void }) {
  const ask = item.ask!;
  const options = ask.options ?? [];
  const [changing, setChanging] = useState(false);
  const [chosen, setChosen] = useState<string | null>(null);
  const [own, setOwn] = useState("");
  const [busy, setBusy] = useState(false);
  const answer = chosen === "" ? own.trim() : chosen ?? (options.length ? "" : own.trim());
  if (ask.answer && !changing) {
    return (
      <div className="cl-ask is-answered">
        <p className="cl-ask__question">{ask.question}</p>
        <p className="cl-ask__answer"><Check aria-hidden size={16} />You answered: {ask.answer.text}</p>
        <Button variant="quiet" size="sm" onClick={() => { setChanging(true); setChosen(null); setOwn(""); }}>Change the answer</Button>
      </div>
    );
  }
  const submit = () => {
    if (!answer) return;
    setBusy(true);
    const lastOne = item.done_at === undefined && item.steps.every((s) => s.done_at !== undefined);
    if (lastOne) onFinished();
    send({ command: "checklist_answer", project_id: projectId, item_id: item.item_id, answer }, `Answer ${item.title}`, { kind: "answer", itemId: item.item_id, answer })
      .then(() => setChanging(false), (e: Error) => toast(e.message, { tone: "stop" }))
      .finally(() => setBusy(false));
  };
  return (
    <fieldset className="cl-ask">
      <legend className="cl-ask__question">{ask.question}</legend>
      {options.length > 0 && (
        <div className="group" role="radiogroup" aria-label={ask.question}>
          {options.map((o) => (
            <ListRow key={o.label} title={o.label} subtitle={o.detail} checked={chosen === o.label} onClick={() => setChosen(o.label)} />
          ))}
          <ListRow title="Something else" checked={chosen === ""} onClick={() => setChosen("")} />
        </div>
      )}
      {(options.length === 0 || chosen === "") && (
        <TextArea label={options.length ? "Your answer" : "Answer"} value={own} onChange={setOwn} maxRows={6} />
      )}
      <div className="cl-ask__actions">
        <Button variant="primary" className="cl-primary" icon={Send} pending={busy} disabled={!answer} onClick={submit}>Send answer</Button>
        {changing && <Button variant="quiet" onClick={() => setChanging(false)}>Keep it</Button>}
      </div>
    </fieldset>
  );
}

function Notes({ notes }: { notes?: ChecklistNote[] }) {
  if (!notes?.length) return null;
  const waiting = unanswered(notes) > 0;
  return (
    <ol className="cl-notes" aria-label="Questions and answers">
      {notes.map((n) => (
        <li key={n.note_id} className={`cl-note cl-note--${n.from}`}>
          <span className="cl-note__who">{n.from === "person" ? "You asked" : "The agent"}</span>
          <Markdown className="cl-note__text" text={n.text} />
        </li>
      ))}
      {waiting && <li className="cl-note cl-note--wait">The agent answers here. You can go on with the checklist meanwhile.</li>}
    </ol>
  );
}

function AskField({ projectId, itemId, send, label }: { projectId: string; itemId?: string; send: SendFn; label: string }) {
  const [text, setText] = useState("");
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  if (!open) {
    return <Button variant="quiet" size="sm" icon={MessageCircleQuestion} className="cl-askfield__open" onClick={() => setOpen(true)}>{label}</Button>;
  }
  const submit = () => {
    const t = text.trim();
    if (!t) return;
    setBusy(true);
    send({ command: "checklist_ask", project_id: projectId, ...(itemId ? { item_id: itemId } : {}), text: t }, label)
      .then(() => { setText(""); setOpen(false); toast(label === "Send feedback" ? "Feedback sent. The answer comes here." : "Asked. The answer comes here."); }, (e: Error) => toast(e.message, { tone: "stop" }))
      .finally(() => setBusy(false));
  };
  return (
    <div className="cl-askfield">
      <TextArea label={label} value={text} onChange={setText} maxRows={6} autoFocus
        hint="The agent that wrote it answers. If a step was wrong or unclear, it fixes the step too." />
      <div className="cl-ask__actions">
        <Button variant="primary" size="sm" icon={Send} pending={busy} disabled={!text.trim()} onClick={submit}>{label === "Send feedback" ? "Send feedback" : "Ask"}</Button>
        <Button variant="quiet" size="sm" onClick={() => { setOpen(false); setText(""); }}>Cancel</Button>
      </div>
    </div>
  );
}

const logos = new Map<string, Promise<string>>();

function Logo({ projectId, file }: { projectId: string; file: ChecklistFile; name: string }) {
  const { platform, profile, session } = useApp();
  const transfers = useMay("transfers");
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const image = file.media_type.startsWith("image/") && file.size <= 2 * 1024 * 1024;
  useEffect(() => {
    setUrl(null);
    setFailed(false);
    if (!image || !transfers) return;
    let alive = true;
    let read = logos.get(file.sha256);
    if (!read) {
      read = download({ platform, profileId: profile.profileId, session }, { source: { kind: "checklist_file", project_id: projectId, sha256: file.sha256 }, label: "Show the checklist’s logo" })
        .then(({ blob }) => URL.createObjectURL(blob));
      read.catch(() => logos.delete(file.sha256));
      logos.set(file.sha256, read);
    }
    read.then((u) => { if (alive) setUrl(u); }, () => {});
    return () => { alive = false; };
  }, [image, transfers, file.sha256, projectId, platform, profile.profileId, session]);
  if (!url || failed) return null;
  return <img className="cl-hero__logo" src={url} alt="" onError={() => setFailed(true)} />;
}

function RemoveSheet({ list, send, onClose }: { list: Checklist; send: SendFn; onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  const open = allItems(list).filter((i) => i.done_at === undefined);
  const agentOpen = open.filter((i) => i.owner === "agent");
  const waiting = open.filter((i) => i.waiting).length;
  const remove = () => {
    setBusy(true);
    send({ command: "remove_checklist", project_id: list.project_id }, "Remove the checklist")
      .then(() => { onClose(); toast("Checklist removed"); }, (e: Error) => { setBusy(false); toast(e.message, { tone: "stop" }); });
  };
  return (
    <Sheet open onClose={onClose} title="Remove the checklist?" subtitle={list.title}
      footer={<Button variant="stop" pending={busy} onClick={remove}>Remove</Button>}>
      <p className="settings-meta">
        {open.length === 0 ? "Everything on it is done." : `${open.length} ${open.length === 1 ? "item is" : "items are"} still to do.`}
        {waiting > 0 && ` ${waiting === 1 ? "An agent waits" : "Agents wait"} on ${waiting === 1 ? "one of them" : `${waiting} of them`}; removing it tells ${waiting === 1 ? "it" : "them"} it won’t be done here.`}
      </p>
      {agentOpen.length > 0 && <div className="settings-meta">
        <p>Removing the checklist also removes these open agent to-dos:</p>
        <ul>{agentOpen.map((item) => <li key={item.item_id}>{item.title}</li>)}</ul>
      </div>}
    </Sheet>
  );
}
