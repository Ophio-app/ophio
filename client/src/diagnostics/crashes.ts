// SPDX-License-Identifier: Apache-2.0
import { Component, createElement, lazy, Suspense, type ErrorInfo, type ReactNode } from "react";
import type { CrashKind, CrashRecord, StackFrame } from "../protocol/types";
import type { StorageLike } from "../state/savedJson";
import { openReport } from "../reports/request";
import { Button } from "../design/Button";
import { MAX_FRAMES, MAX_REPORT_CODE, MAX_TRAIL_EVENTS } from "./constants";
import { scrub } from "./scrub";
import { SavedDiagnostics } from "./storage";
import { trail } from "./trail";

const identifier = (value: string | undefined) => value && /^[A-Za-z_$][\w$.<>]*(?: \[as [\w$]+\])?$/.test(value) ? value.slice(0, MAX_REPORT_CODE) : undefined;
function crashCode(code: string, kind: CrashKind): string {
  if (/^js:(?:[A-Za-z]+Error|Error)@[A-Za-z_$][\w$.<>]*(?: \[as [\w$]+\])?$/.test(code)) return code.slice(0, MAX_REPORT_CODE);
  if (/^(?:exit|metric):[A-Za-z0-9_-]+$/.test(code)) return code.slice(0, MAX_REPORT_CODE);
  const panic = code.match(/^panic:(.+):(\d+)$/);
  if (panic) { const file = panic[1]!.split(/[\\/]/).at(-1)!; if (/^[\w.-]+$/.test(file)) return `panic:${file}:${panic[2]}`.slice(0, MAX_REPORT_CODE); }
  return kind === "system" ? "exit:unknown" : kind === "panic" ? "panic:unknown:0" : "js:Error@unknown";
}
const errorNames = new Set(["Error", "TypeError", "ReferenceError", "SyntaxError", "RangeError", "URIError", "EvalError", "AggregateError"]);

export function ownFrames(stack: string | undefined, origin = location.origin): StackFrame[] {
  const frames: StackFrame[] = [];
  for (const line of (stack ?? "").split("\n")) {
    // Chromium and WebKit spell stacks differently. Neither's absolute URL leaves here.
    const match = line.trim().replace(/^(.*?)@(?=(?:https?|tauri|asset):)/, "at $1 (").match(/^(?:at\s+)?(?:(.*?)\s*\()?((?:https?|tauri|asset):\/\/[^\s()]+?):(\d+):(\d+)\)?$/);
    if (!match) continue;
    try {
      const url = new URL(match[2]!);
      if (url.origin !== origin || url.search || url.hash) continue;
      if (!/^\/assets\/[A-Za-z0-9_-]+\.js$/.test(url.pathname)
        && !(import.meta.env.DEV && /^\/src\/[\w/-]+\.[jt]sx?$/.test(url.pathname))) continue;
      const fn = identifier(match[1]?.replace(/@$/, "").trim());
      frames.push({ ...(fn ? { function: fn } : {}), file: url.pathname.split("/").at(-1)!, line: Number(match[3]), column: Number(match[4]) });
      if (frames.length === MAX_FRAMES) break;
    } catch { /* A malformed frame is not an app frame. */ }
  }
  return frames;
}

type CrashSource = () => Promise<CrashRecord[]>;
export class CrashRegistry {
  #saved: SavedDiagnostics<CrashRecord[]>;
  #pending: CrashRecord[];
  #sources = new Set<CrashSource>();
  #collecting: Promise<CrashRecord[]> | undefined;
  #since = 0;
  build = "unknown";
  constructor(storage?: StorageLike | null, readonly now: () => number = Date.now) {
    this.#saved = new SavedDiagnostics("diagnostics.crashes.v1", storage);
    const saved = this.#saved.load([]);
    this.#pending = Array.isArray(saved) ? saved.filter((c) => c && c.side === "phone" && typeof c.code === "string" && Array.isArray(c.frames)).slice(0, MAX_TRAIL_EVENTS) : [];
  }
  pending(): CrashRecord[] { return structuredClone(this.#pending); }
  addCrashSource(source: CrashSource): () => void { this.#sources.add(source); return () => this.#sources.delete(source); }
  async collect(): Promise<CrashRecord[]> {
    if (this.#collecting) return this.#collecting;
    this.#collecting = (async () => {
      const results = await Promise.allSettled([...this.#sources].map((source) => source()));
      for (const result of results) if (result.status === "fulfilled") for (const crash of result.value) this.keep(crash);
      return this.pending();
    })();
    try { return await this.#collecting; } finally { this.#collecting = undefined; }
  }
  /** Drops everything from before `at`, when this app was installed or
   *  updated. An update stops the app, and what came before it belongs to a
   *  build that is gone. */
  forgetBefore(at: number): void {
    if (!(at > this.#since)) return;
    this.#since = at;
    const kept = this.#pending.filter((c) => c.at > at);
    if (kept.length === this.#pending.length) return;
    this.#pending = kept;
    this.#saved.save(kept);
  }
  keep(crash: CrashRecord): void {
    if (!(crash.at > this.#since)) return;
    try {
      const safe: CrashRecord = { at: crash.at, side: "phone", kind: crash.kind, code: crashCode(crash.code, crash.kind), build: crash.build.slice(0, MAX_REPORT_CODE), message: scrub(crash.message ?? ""), frames: crash.frames.filter((f) => /^[\w-]+\.[\w]+$/.test(f.file)).slice(0, MAX_FRAMES).map(f => ({ file: f.file, line: Math.min(0xffffffff, Math.max(0, Math.trunc(f.line))), ...(f.column !== undefined ? { column: Math.min(0xffffffff, Math.max(0, Math.trunc(f.column))) } : {}), function: identifier(f.function) })) };
      const key = JSON.stringify(safe);
      if (!this.#pending.some((c) => JSON.stringify(c) === key)) this.#pending.unshift(safe);
      this.#pending = this.#pending.slice(0, MAX_TRAIL_EVENTS);
      this.#saved.save(this.#pending);
    } catch { /* A broken crash source cannot break reporting. */ }
  }
  acknowledge(sent: CrashRecord[]): void {
    const keys = new Set(sent.map((c) => JSON.stringify(c)));
    this.#pending = this.#pending.filter((c) => !keys.has(JSON.stringify(c)));
    this.#saved.save(this.#pending);
  }
  capture(error: unknown, kind: CrashKind = "script", fallback?: string): void {
    try {
      const e = error instanceof Error ? error : undefined;
      const frames = ownFrames(e?.stack);
      const name = e && errorNames.has(e.name) ? e.name : "Error";
      const code = `js:${name}@${frames[0]?.function ?? "unknown"}`;
      this.keep({ at: this.now(), side: "phone", kind, code, build: this.build, message: scrub(e?.message ?? (typeof error === "string" ? error : fallback ?? "Unhandled rejection")), frames });
      trail.record({ type: "crash", code });
      trail.flush();
    } catch { /* Capturing a crash must not cause a second one. */ }
  }
  install(): () => void {
    const onerror: OnErrorEventHandler = (message, _source, _line, _column, error) => { this.capture(error, "script", typeof message === "string" ? message : undefined); return false; };
    const previous = window.onerror;
    const handler: OnErrorEventHandler = (...args) => { onerror(...args); return previous?.apply(window, args) ?? false; };
    const rejection = (event: PromiseRejectionEvent) => this.capture(event.reason);
    window.onerror = handler;
    window.addEventListener("unhandledrejection", rejection);
    return () => { if (window.onerror === handler) window.onerror = previous; window.removeEventListener("unhandledrejection", rejection); };
  }
}
export const crashes = new CrashRegistry();
export const addCrashSource = (source: CrashSource) => crashes.addCrashSource(source);

const CrashReportOverlay = lazy(() => import("../reports/ReportsOverlay").then(m => ({ default: m.ReportsOverlay })));

export class DiagnosticsBoundary extends Component<{ children: ReactNode; registry?: CrashRegistry; reload?: () => void }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  componentDidCatch(error: unknown, _info: ErrorInfo) { (this.props.registry ?? crashes).capture(error, "render"); }
  render() {
    if (!this.state.failed) return this.props.children;
    return createElement("div", { className: "boot boot--error", role: "alert" },
      createElement("h1", { className: "boot__title" }, "Something went wrong"),
      createElement(Button, { variant: "primary", onClick: this.props.reload ?? (() => location.reload()) }, "Reload"),
      createElement(Button, { variant: "quiet", onClick: () => openReport({ type: "error" }, { screen: "/startup", error: { code: "render_error" } }) }, "Report"),
      createElement(Suspense, { fallback: null }, createElement(CrashReportOverlay)));
  }
}
