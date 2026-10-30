// SPDX-License-Identifier: Apache-2.0
import type { SimHost } from "./host";
import {
  ids,
  targets as initialTargets,
  terminals as initialTerminals,
} from "./scenario";
import type {
  Command,
  InputEnvelope,
  InputAck,
  TerminalClientFrame,
  TerminalInfo,
} from "../../protocol/types";
import type { TerminalEvent, TerminalLink } from "../../host/link";
import { terminalFrame } from "../../protocol/binary";

interface Transcript {
  bytes: Uint8Array;
  base: number;
  line: string;
  decoder: TextDecoder;
}
export class ControlDrills {
  previewDelay = 250;
  previewError = false;
  readonly previewClosers = new Set<(reason: string) => void>();
  closePreviews(reason: string): void { for (const close of this.previewClosers) close(reason); }
  frozen = false;
  rejectNext = false;
  /** The next human action may or may not reach the screen, and the release after it is unconfirmed too. */
  uncertainNext = false;
  refuseText: { characters: string; paste_available: boolean } | null = null;
  readonly inputs: InputEnvelope[] = [];
  readonly transcripts = new Map<string, Transcript>();
  readonly terminalLinks = new Set<SimControlTerminal>();
  #timers = new Map<string, ReturnType<typeof setTimeout>>();
  #leases = new Map<string, string>();
  #detach: () => void;
  constructor(readonly host: SimHost) {
    this.#detach = host.attach((frame) => {
      if (frame.type !== "event" || frame.event.type !== "control_changed")
        return;
      const view = frame.event.view;
      const identity =
        view.lease?.state === "active" &&
        (view.input_state === "human" || view.input_state === "agent")
          ? `${view.lease.lease_id}:${view.lease.generation}`
          : null;
      if (identity && this.#leases.get(view.target_id) === identity) return;
      const old = this.#timers.get(view.target_id);
      if (old) clearTimeout(old);
      this.#timers.delete(view.target_id);
      this.#leases.delete(view.target_id);
      if (
        view.lease?.state === "active" &&
        (view.input_state === "human" || view.input_state === "agent")
      )
        this.renew(view.lease.lease_id, view.lease.generation);
    });
  }
  lock(value = true, targetId = ids.tgHdmi): void {
    this.host.control = this.host.control.map(v => {
      if (v.target_id !== targetId) return v;
      const view = { ...v, desktop_lock: value ? "locked" as const : "unlocked" as const, input_state: "paused" as const,
        model_observation: v.model_observation ? { ...v.model_observation, state: "withdrawn" as const } : null,
        lease: v.lease ? { ...v.lease, state: "revoked" as const } : null };
      this.host.emit({ type: "control_changed", view });
      return view;
    });
  }
  refusal(env: InputEnvelope): InputAck | null {
    const view = this.host.control.find(v => v.lease?.lease_id === env.lease_id);
    if (view?.desktop_lock === "locked") return { lease_id: env.lease_id, seq: env.seq, outcome: "failed", refusal: { kind: "locked" } };
    if (env.action.action === "text" && this.refuseText) {
      const refusal = { kind: "text_unavailable" as const, ...this.refuseText };
      this.refuseText = null;
      return { lease_id: env.lease_id, seq: env.seq, outcome: "failed", refusal };
    }
    return null;
  }
  reopen(targetId: string): void {
    this.host.targets = this.host.targets.map(target =>
      target.target_id === targetId && target.capture.state === "temporarily_unavailable"
        && target.capture.reason.includes("Display changed")
        ? { ...target, capture: { state: "available" }, input: { state: "available" } }
        : target);
    this.expire(targetId);
    this.host.emit({ type: "targets_changed", targets: this.host.targets });
  }
  captureAllowed(targetId: string): boolean {
    // Like the Linux companion: an unknown lock also reports input unavailable.
    const lock = this.host.control.find(v => v.target_id === targetId)?.desktop_lock;
    return lock !== "locked" && (lock !== "unknown" || this.host.host.capabilities.input.state === "available");
  }
  admitInput(targetId: string): void {
    const caps = this.host.host.capabilities;
    const target = this.host.targets.find(t => t.target_id === targetId);
    for (const a of [caps.input, caps.capture, target?.input, target?.capture]) {
      if (a?.state !== "available") throw { code: "temporarily_unavailable", message: a && "reason" in a ? a.reason : a && "remedy" in a ? a.remedy : "Desktop input is unavailable", detail: null };
    }
    if (!this.captureAllowed(targetId)) throw { code: "temporarily_unavailable", message: this.host.control.find(v => v.target_id === targetId)?.desktop_lock === "locked" ? "Computer is locked" : "Desktop lock state is unknown", detail: null };
  }
  before(command: Command): void {
    if (["request_human_lease", "take_over", "return_to_agent"].includes(command.command) && "target_id" in command && command.target_id)
      this.admitInput(command.target_id);
    if (command.command === "create_task" && command.kind === "computer_use" && command.target_id) this.admitInput(command.target_id);
    if (command.command === "resume_task") {
      const task = this.host.tasks.find(t => t.task_id === command.task_id);
      if (task?.kind === "computer_use" && task.target_id) this.admitInput(task.target_id);
    }
    if (command.command === "capture_target" && !this.captureAllowed(command.target_id))
      throw { code: "temporarily_unavailable", message: "Computer is locked", detail: null };
    if (
      this.rejectNext &&
      (command.command === "request_human_lease" ||
        command.command === "take_over")
    ) {
      this.rejectNext = false;
      throw {
        code: "permission_denied",
        message:
          "Input permission was withdrawn on the computer. Review its input settings.",
      };
    }
  }
  renew(leaseId: string, generation: number): void {
    const view = this.host.control.find(
      (v) =>
        v.lease?.lease_id === leaseId &&
        v.lease.generation === generation &&
        v.lease.state === "active",
    );
    if (!view?.lease) return;
    this.#leases.set(view.target_id, `${leaseId}:${generation}`);
    const old = this.#timers.get(view.target_id);
    if (old) clearTimeout(old);
    this.#timers.set(
      view.target_id,
      setTimeout(
        () => this.expire(view.target_id),
        view.lease.renewal_window_ms,
      ),
    );
  }
  expire(targetId = ids.tgHdmi): void {
    const current = this.host.control.find((v) => v.target_id === targetId);
    if (!current?.lease || current.lease.state !== "active") return;
    const view = {
      ...current,
      input_state: "paused" as const,
      lease: { ...current.lease, state: "expired" as const },
    };
    this.host.control = this.host.control.map((v) =>
      v.target_id === targetId ? view : v,
    );
    this.host.emit({ type: "control_changed", view });
  }
  geometry(targetId = ids.tgHdmi): void {
    this.host.targets = this.host.targets.map((t) =>
      t.target_id === targetId
        ? {
            ...t,
            geometry: {
              ...t.geometry,
              width: t.geometry.height,
              height: t.geometry.width,
              revision: t.geometry.revision + 1,
            },
          }
        : t,
    );
    this.host.emit({ type: "targets_changed", targets: this.host.targets });
  }
  missing(targetId = ids.tgHdmi): void {
    this.host.targets = this.host.targets.map((t) =>
      t.target_id === targetId
        ? {
            ...t,
            capture: {
              state: "permission_missing",
              remedy:
                "On the computer, open Screen Sharing and approve this display in the desktop portal.",
            },
          }
        : t,
    );
    this.host.emit({ type: "targets_changed", targets: this.host.targets });
  }
  empty(): void {
    this.host.targets = [];
    this.host.terminals = [];
    this.host.emit({ type: "targets_changed", targets: [] });
    this.host.emit({ type: "terminals_changed", terminals: [] });
  }
  restore(): void {
    this.host.targets = initialTargets();
    this.host.terminals = initialTerminals(Date.now());
    this.host.emit({ type: "targets_changed", targets: this.host.targets });
    this.host.emit({
      type: "terminals_changed",
      terminals: this.host.terminals,
    });
  }
  dropTerminal(ms = 1200): void {
    for (const link of this.terminalLinks) {
      link.lose("Terminal connection interrupted");
      setTimeout(() => link.connect(), ms);
    }
  }
  dispose(): void {
    this.closePreviews("lost_connection");
    this.#detach();
    for (const t of this.#timers.values()) clearTimeout(t);
    this.#timers.clear();
    this.#leases.clear();
  }
}

interface Environment {
  host: SimHost;
  forcedOffline: boolean;
  revoked: boolean;
  side: Set<{ lose(reason: string): void; connect(): void }>;
}
const encoder = new TextEncoder();
export class SimControlTerminal implements TerminalLink {
  #listeners = new Set<(e: TerminalEvent) => void>();
  #terminal: TerminalInfo | null = null;
  #offline = false;
  #closed = false;
  #epoch = 0;
  #detachHost: () => void;
  constructor(private readonly env: Environment) {
    env.side.add(this);
    env.host.controlSim.terminalLinks.add(this);
    this.#detachHost = env.host.attach((event) => {
      if (
        event.type !== "event" ||
        event.event.type !== "terminals_changed" ||
        !this.#terminal
      )
        return;
      const terminal = event.event.terminals.find(
        (t) => t.terminal_id === this.#terminal?.terminal_id,
      );
      if (terminal) this.#terminal = terminal;
      if (terminal?.state.state === "exited")
        this.#emit({
          type: "text",
          frame: { type: "exited", code: terminal.state.code },
        });
    });
  }
  lose(reason: string): void {
    this.#offline = true;
    this.#epoch++;
    this.#emit({
      type: "state",
      state: { state: "offline", since: Date.now(), reason, retryAt: null },
    });
  }
  connect(): void {
    if (this.#closed || this.env.forcedOffline || this.env.revoked) return;
    this.#offline = false;
    this.#emit({
      type: "state",
      state: { state: "online", since: Date.now() },
    });
  }
  send(frame: TerminalClientFrame): boolean {
    if (
      this.#closed ||
      this.#terminal?.state.state === "exited" ||
      this.#offline ||
      this.env.forcedOffline ||
      this.env.revoked
    )
      return false;
    if (frame.type === "detach") {
      this.#terminal = null;
      this.#epoch++;
      return true;
    }
    if (frame.type === "resize") {
      this.#emit({
        type: "text",
        frame: { type: "resized", cols: frame.cols, rows: frame.rows },
      });
      return true;
    }
    const generation = ++this.#epoch;
    setTimeout(() => {
      if (this.#closed || generation !== this.#epoch) return;
      const terminal = this.env.host.terminals.find(
        (t) => t.terminal_id === frame.terminal_id,
      );
      if (!terminal) {
        this.#emit({
          type: "state",
          state: { state: "offline", since: Date.now(), code: "not_found",
            reason: "Terminal not found", retryAt: null },
        });
        return;
      }
      this.#terminal = terminal;
      const buffer = this.#buffer(terminal);
      const requested = frame.from_offset ?? buffer.base;
      const from = Math.min(
        buffer.base + buffer.bytes.length,
        Math.max(buffer.base, requested),
      );
      this.#emit({
        type: "text",
        frame: {
          type: "attached",
          terminal,
          replay_from: from,
          replay_truncated: requested < buffer.base,
        },
      });
      this.#emit({
        type: "output",
        data: terminalFrame(from, buffer.bytes.subarray(from - buffer.base)),
      });
    }, 120);
    return true;
  }
  #buffer(terminal: TerminalInfo): Transcript {
    let buffer = this.env.host.controlSim.transcripts.get(terminal.terminal_id);
    if (!buffer) {
      buffer = {
        bytes: encoder.encode(
          `\x1b[?1049l\x1b[?25hConnected to ${terminal.title}\r\n${terminal.cwd}\r\n\r\n${this.#prompt(terminal)}`,
        ),
        base: 0,
        line: "",
        decoder: new TextDecoder(),
      };
      this.env.host.controlSim.transcripts.set(terminal.terminal_id, buffer);
    }
    return buffer;
  }
  #prompt(terminal: TerminalInfo): string {
    return `${terminal.cwd.split("/").at(-1) ?? "shell"} ❯ `;
  }
  input(bytes: Uint8Array): boolean {
    const term = this.#terminal;
    if (
      !term ||
      this.#closed ||
      this.#terminal?.state.state === "exited" ||
      this.#offline ||
      this.env.forcedOffline ||
      this.env.revoked ||
      term.state.state !== "running"
    )
      return false;
    const buffer = this.#buffer(term);
    const text = buffer.decoder
      .decode(bytes, { stream: true })
      .replaceAll("\x1b[200~", "")
      .replaceAll("\x1b[201~", "");
    for (const ch of text) {
      if (ch === "\r" || ch === "\n") {
        const command = buffer.line.trim();
        buffer.line = "";
        this.#append(term, "\r\n");
        this.#run(term, command);
      } else if (ch === "\x03") {
        buffer.line = "";
        this.#append(term, `^C\r\n${this.#prompt(term)}`);
      } else if (ch === "\x7f") {
        buffer.line = buffer.line.slice(0, -1);
        this.#append(term, "\b \b");
      } else if (ch >= " ") {
        buffer.line += ch;
        this.#append(term, ch);
      }
    }
    return true;
  }
  #run(term: TerminalInfo, command: string): void {
    if (/^exit(?: \d+)?$/.test(command)) {
      this.#append(term, "exit\r\n");
      term.state = { state: "exited", code: Number(command.split(" ")[1] ?? 0) };
      this.env.host.emit({ type: "terminals_changed", terminals: this.env.host.terminals });
      return;
    }
    const output =
      command === "pwd"
        ? term.cwd
        : command === "ls"
          ? "README.md  src  tests  package.json"
          : command === "git status"
            ? "On branch fix/retry-clock\r\nChanges not staged for commit:\r\n  modified: src/retry.rs"
            : command.startsWith("echo ")
              ? command.slice(5)
              : command.startsWith("cargo test")
                ? "running 64 tests\r\ntest result: ok. 64 passed; 0 failed"
                : command
                  ? `shell: command not found: ${command.split(" ")[0]}`
                  : "";
    setTimeout(
      () =>
        this.#append(
          term,
          `${output}${output ? "\r\n" : ""}${this.#prompt(term)}`,
        ),
      100,
    );
  }
  #append(terminal: TerminalInfo, text: string): void {
    if (
      this.env.host.terminals.find(
        (t) => t.terminal_id === terminal.terminal_id,
      )?.state.state === "exited"
    )
      return;
    const buffer = this.#buffer(terminal),
      bytes = encoder.encode(text),
      at = buffer.base + buffer.bytes.length;
    const next = new Uint8Array(buffer.bytes.length + bytes.length);
    next.set(buffer.bytes);
    next.set(bytes, buffer.bytes.length);
    buffer.bytes = next;
    if (next.length > 256 * 1024) {
      const cut = next.length - 256 * 1024;
      buffer.base += cut;
      buffer.bytes = next.slice(cut);
    }
    terminal.last_output_at = Date.now();
    terminal.output_offset = buffer.base + buffer.bytes.length;
    for (const link of this.env.host.controlSim.terminalLinks)
      if (
        link.#terminal?.terminal_id === terminal.terminal_id &&
        !link.#offline
      )
        link.#emit({ type: "output", data: terminalFrame(at, bytes) });
  }
  close(): void {
    this.#detachHost();
    this.#closed = true;
    this.#epoch++;
    this.#terminal = null;
    this.#listeners.clear();
    this.env.side.delete(this);
    this.env.host.controlSim.terminalLinks.delete(this);
  }
  listen(listener: (e: TerminalEvent) => void): () => void {
    this.#listeners.add(listener);
    queueMicrotask(() => this.connect());
    return () => this.#listeners.delete(listener);
  }
  #emit(event: TerminalEvent): void {
    if (
      (!this.#closed && !this.env.forcedOffline && !this.env.revoked) ||
      event.type === "state"
    )
      for (const listener of this.#listeners) listener(event);
  }
}

export function controlDrills(getHost: () => SimHost) {
  return {
    freeze: (value = true) => {
      getHost().controlSim.frozen = value;
    },
    geometry: (target?: string) => getHost().controlSim.geometry(target),
    rejectLease: () => {
      getHost().controlSim.rejectNext = true;
    },
    uncertainInput: () => {
      getHost().controlSim.uncertainNext = true;
    },
    expire: (target?: string) => getHost().controlSim.expire(target),
    missing: (target?: string) => getHost().controlSim.missing(target),
    empty: () => getHost().controlSim.empty(),
    restore: () => getHost().controlSim.restore(),
    dropTerminal: (ms?: number) => getHost().controlSim.dropTerminal(ms),
    inputs: () => getHost().controlSim.inputs,
    lock: (value = true, target?: string) => getHost().controlSim.lock(value, target),
    refuseText: (characters = "éΕληνικά日本語", paste_available = true) => { getHost().controlSim.refuseText = { characters, paste_available }; },
  };
}

/** Preview side-link drill: never opens a real browser or accesses a local service. */
export async function simulatedPreview(env: Environment, previewId: string, changed: (state: import("../../host/preview").PreviewState) => void): Promise<import("../../host/preview").PreviewHandle> {
  if (env.forcedOffline || env.revoked) throw new Error("The computer is not connected.");
  // Like the computer, open only a preview its latest check reached.
  if (env.host.previews.find(p => p.preview_id === previewId)?.reachable !== true)
    throw Object.assign(new Error("The computer hasn’t confirmed this preview is reachable. Check it again."), { code: "conflict" });
  if (env.host.controlSim.previewError) throw new Error("The computer could not reach this preview server.");
  await new Promise(resolve => setTimeout(resolve, env.host.controlSim.previewDelay));
  if (env.forcedOffline || env.revoked) throw new Error("The preview connection was lost.");
  let closed = false;
  const close = (reason: string) => {
    if (closed) return;
    closed = true;
    env.side.delete(side);
    env.host.controlSim.previewClosers.delete(close);
    changed({ state: "closed", reason });
  };
  const side = { lose: () => close(env.revoked ? "device_revoked" : "lost_connection"), connect() {} };
  env.side.add(side);
  env.host.controlSim.previewClosers.add(close);
  const style = getComputedStyle(document.documentElement);
  const ink = style.getPropertyValue("--ink"), bg = style.getPropertyValue("--surface"), line = style.getPropertyValue("--line");
  changed({ state: "open", connected_address: "[::1]:5173" });
  return {
    document: `<!doctype html><html lang="en"><meta name="viewport" content="width=device-width"><style>body{font:16px system-ui;color:${ink};background:${bg};padding:24px;margin:0}h1{font-size:28px}article{border-top:1px solid ${line};padding:20px 0}small{font-size:14px}</style><small>SIMULATED PREVIEW</small><h1>Garden</h1><p>A quiet place for the work ahead.</p><article><h2>Today</h2><p>Finish the retry clock.</p></article><article><h2>Next</h2><p>Review the latest changes.</p></article></html>`,
    async show() { return null; },
    async close() { close("closed"); },
    async picture() {
      const canvas = document.createElement("canvas"); canvas.width = 600; canvas.height = 400;
      const context = canvas.getContext("2d");
      if (!context) throw new Error("Picture capture unavailable.");
      context.fillStyle = bg; context.fillRect(0, 0, 600, 400); context.fillStyle = ink;
      context.font = "32px sans-serif"; context.fillText("Garden", 32, 60);
      context.font = "20px sans-serif"; context.fillText("Today · Finish the retry clock", 32, 130);
      const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob(b => b ? resolve(b) : reject(new Error("Picture capture failed.")), "image/png"));
      return { blob, async acknowledge() {} };
    },
  };
}
