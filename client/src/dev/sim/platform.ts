// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { parseHostVariant } from "./variants";
//
// Development-only Platform backed by SimHost. It reproduces link behavior
// that matters to the interface — authentication, offline periods, lost
// receipts, journal replay — and exposes `window.__sim` so tests and people
// can inject failures while using the app.

import type {
  ErrorCode,
  ClientFrame,
  Grants,
  MediaClientFrame,
  PairingOffer,
  ProtocolError,
  Query,
  ServerFrame,
  SpeechClientFrame,
  StreamParams,
  TransferClientFrame,
} from "../../protocol/types";
import { buildTileFrame } from "../../protocol/binary";
import { base64UrlEncode, parsePairingLink } from "../../protocol/pairing";
import { PAIRING_SCHEME } from "../../brand";
import type {
  ControlLink,
  HostProfile,
  LinkEvent,
  LinkState,
  MediaEvent,
  MediaLink,
  PairingResult,
  Platform,
  SendOutbox,
  SpeechEvent,
  SpeechLink,
  TransferEvent,
  TransferLink,
  TerminalLink,
} from "../../host/link";
import { SimHost, allGrants } from "./host";
import { fixtureHandoff } from "./contribution";
import { handoffs } from "../../screens/contribute/handoffs";
import * as fx from "./scenario";
import { controlDrills, SimControlTerminal, simulatedPreview } from "./control";
import { refusedSide, type SideKind, type SideRefusal } from "./refuseSide";
import { reportPhone } from "./reportPhone";
import { simulatedDevice } from "./device";
import { paintDesktop } from "./annotate";

import { simulatedLifecycle, secondProfile } from "./lifecycle";

type Sendable = Exclude<ClientFrame, { type: "hello" } | { type: "authenticate" }>;

const PROFILE: HostProfile = {
  profileId: "sim-studio",
  hostId: fx.ids.host,
  name: "studio",
  addresses: ["studio.tail1234.ts.net:47100"],
  certSha256: "5f1c9a0e7d3b2a4c6e8f0a1b3c5d7e9f2a4b6c8d0e1f3a5b7c9d1e3f5a7b9c0d",
  pairedAt: Date.now() - 12 * 24 * 3600_000,
  deviceId: fx.ids.device,
};

/** Controls exposed on `window.__sim` for tests and hands-on failure drills. */
export interface SimControls {
  reports: typeof reportPhone;
  lifecycle: ReturnType<typeof simulatedLifecycle>["controls"];
  device: ReturnType<typeof simulatedDevice>["controls"];
  work: SimHost["work"];
  host: SimHost;
  library: SimHost["library"];
  transfers: { chunkDelay(ms: number): void; interruptPublication(): void };
  control: ReturnType<typeof controlDrills>;
  media: { freeze(value?: boolean): void };
  /** Drop the connection and keep it down until `online()`. */
  offline(reason?: string): void;
  /** Another device answers at the computer's address until `online()`. */
  impostor(): void;
  online(): void;
  /** Re-authenticate with restricted phone grants for access-state checks. */
  permissions(grants: Partial<Grants>): void;
  /** Execute the next command on the host but lose the connection before its record or receipt arrives. */
  dropNextReceipt(): void;
  /** Delay every receipt and query answer by this many milliseconds. */
  latency(ms: number): void;
  /** Revoke this phone (terminal failure state). */
  revoke(): void;
  /** Speed up (<1) or slow down (>1) scripted agent behavior. */
  pace(multiplier: number): void;
  /** Explicitly replace the simulated computer and its journal. */
  restart(): void;
  /** Refuse the next side connection of this kind with a fatal protocol reason. */
  refuseSide(kind: SideKind, code: ErrorCode, reason?: string): void;
  stops: { ignore(taskId: string, activity?: boolean, ends?: boolean): void; restart(taskId: string): void };
  /** The computer's owner. Unless held, they confirm each request at once. */
  pairing: { hold(): void; confirm(): void; refuse(): void; expire(): void };
  /** A fixture handoff from Clankerhub, as a transport would deliver it; its id. */
  handoff(over?: Parameters<typeof fixtureHandoff>[1]): string | null;
}

declare global {
  interface Window {
    __sim?: SimControls;
  }
}

class SimControlLink implements ControlLink {
  #listeners = new Set<(e: LinkEvent) => void>();
  #state: LinkState = { state: "idle" };
  #detach: (() => void) | null = null;
  #timers: ReturnType<typeof setTimeout>[] = [];
  #attempt = 0;

  constructor(private readonly env: SimEnv) {
    env.links.add(this);
  }

  connect(): void {
    if (this.#state.state === "online" || this.#state.state === "connecting" || this.#state.state === "authenticating") return;
    if (this.env.revoked) {
      this.#setState({ state: "failed", reason: "revoked", detail: "This device was removed on the computer." });
      return;
    }
    if (this.env.incompatible) {
      this.#setState({ state: "failed", reason: "incompatible_version", detail: `Update the app. ${PRODUCT_NAME} on the computer requires protocol 2.0.` });
      return;
    }
    this.#attempt++;
    this.#setState({ state: "connecting", attempt: this.#attempt });
    this.#later(this.env.forcedOffline ? 900 : 350, () => {
      if (this.env.forcedOffline) {
        this.#setState({
          state: "offline",
          since: Date.now(),
          reason: this.env.mismatch ? SIM_MISMATCH : this.env.offlineReason,
          retryAt: Date.now() + 4000,
          ...(this.env.mismatch && { mismatch: true }),
        });
        this.#later(4000, () => this.connect());
        return;
      }
      this.#setState({ state: "authenticating" });
      this.#later(250, () => {
        this.#attempt = 0;
        const host = this.env.host;
        // Online before announcing, so the session's first query can be sent.
        this.#state = { state: "online", since: Date.now() };
        this.#emit({
          type: "authenticated",
          challenge: {
            selected: { major: 1, minor: 0 },
            host_id: host.host.host_id,
            boot_epoch: host.bootEpoch,
            server_nonce: "c2ltdWxhdGVk",
          },
          auth: {
            host_id: host.host.host_id,
            device_id: fx.ids.device,
            grants: { ...this.env.grants },
            capabilities: host.host.capabilities,
            companion_version: "0.1.0-sim",
            journal_head: host.seq,
          },
        });
      });
    });
  }

  disconnect(): void {
    this.#drop({ state: "idle" });
  }

  /** Called by SimEnv when the simulated network goes away. */
  lose(reason: string, retryMs = 4000): void {
    if (this.#state.state === "idle" || this.#state.state === "failed") return;
    this.#drop({ state: "offline", since: Date.now(), reason, retryAt: Date.now() + retryMs, ...(this.env.mismatch && { mismatch: true }) });
    this.#later(retryMs, () => this.connect());
  }

  revoke(): void {
    this.#drop({ state: "failed", reason: "revoked", detail: "This device was removed on the computer." });
  }

  #drop(next: LinkState): void {
    this.#detach?.();
    this.#detach = null;
    for (const t of this.#timers) clearTimeout(t);
    this.#timers = [];
    this.#setState(next);
  }

  send(frame: Sendable): boolean {
    if (this.#state.state !== "online") return false;
    const host = this.env.host;
    switch (frame.type) {
      case "ping":
        this.#later(40, () => this.#frame({ type: "pong", nonce: frame.nonce }));
        return true;
      case "subscribe":
        this.#detach?.();
        host.replayAfter(frame.after_seq, (f) => this.#frame(f));
        this.#detach = host.attach((f) => this.#frame(f));
        void host.work.pictures();
        return true;
      case "query": {
        const { query_id, query } = frame;
        // The computer refuses a query this device wasn't granted.
        const refused = queryAllowed(this.env.grants, query)
          ? null
          : Promise.reject({ code: "permission_denied", message: "This device or chat wasn’t given permission for that" });
        void (refused ?? host.query(query)).then(
          (data) => this.#later(this.env.latencyMs + 60, () => this.#frame({ type: "query_result", query_id, result: { Ok: data } })),
          (e) => this.#later(this.env.latencyMs + 60, () => this.#frame({ type: "query_result", query_id, result: { Err: e } })),
        );
        return true;
      }
      case "command": {
        const { type: _t, ...env } = frame;
        void _t;
        const drop = this.env.dropReceipt;
        this.env.dropReceipt = false;
        // The host has it; the phone never hears back, not even its record.
        if (drop) {
          this.#detach?.();
          this.#detach = null;
        }
        const body = frame.body;
        const refusal = ["set_project_stage", "add_project", "remove_project", "update_project_defaults", "promote_one_off", "delete_one_off"].includes(body.command) && !this.env.grants.admin
          ? "Project settings permission is required."
          : ["review_contribution", "prepare_contribution", "cancel_contribution"].includes(body.command) && !this.env.grants.admin
            ? "contribution_manage_required"
          : ["create_agent", "copy_setup_skill", "choose_setup_skill_version", "set_disabled_in_ophio"].includes(body.command) && !this.env.grants.library_write
            ? "This device or chat wasn’t given permission for that"
            : body.command === "account" && (!this.env.grants.accounts || (body.action.action === "add_service" && body.action.service.service.kind === "cli" && !this.env.grants.terminal))
            ? "This device or chat wasn’t given permission for that"
            : body.command === "agent_login" && !(body.action.action === "answer_code" ? this.env.grants.approvals : this.env.grants.accounts)
            ? "This device or chat wasn’t given permission for that"
            : body.command === "github" && !(["mark_pr_ready", "merge_pr", "merge_together", "stop_together", "file_issue", "run_issues", "set_group", "close_issue"].includes(body.action.action) ? this.env.grants.tasks : this.env.grants.admin)
            ? "This device or chat wasn’t given permission for that"
            : body.command === "unlock_desktop" && !(this.env.grants.control_input && this.env.grants.admin)
            ? "This device or chat wasn’t given permission for that"
            : (["checklist_tick", "checklist_answer", "checklist_ask", "checklist_deliver", "remove_checklist"].includes(body.command) && !this.env.grants.tasks)
              || (body.command === "set_file_delivery" && !this.env.grants.admin)
            ? "This device or chat wasn’t given permission for that"
            : null;
        const result = refusal
          ? Promise.resolve<import("../../protocol/types").CommandReceipt>({ command_id: frame.command_id, status: "rejected", seq: null, result: null, error: { code: "permission_denied", message: refusal } })
          : host.command(env, () => this.env.grants);
        void result.then((receipt) => {
          if (drop) {
            this.env.loseAll("Connection lost while sending");
            return;
          }
          this.#later(this.env.latencyMs + 120, () => {
            this.#frame({ type: "receipt", ...receipt });
            if (frame.body.command === "revoke_device" && frame.body.device_id === PROFILE.deviceId && receipt.status !== "rejected") {
              // As on a real computer: the connection closes first, and the
              // refused reconnect is what says this device was revoked.
              this.env.revoked = true;
              for (const link of this.env.links) link.lose("Connection closed by the computer", 600);
            }
            if ((frame.body.command === "set_device_grants" || frame.body.command === "change_device_grants") && frame.body.device_id === PROFILE.deviceId && receipt.status !== "rejected") {
              // The computer closes this device's connections so it reconnects
              // under the new grants.
              this.env.grants = { ...host.devices.find((d) => d.device_id === PROFILE.deviceId)!.grants };
              this.env.loseAll("device grants changed; reconnect");
            }
          });
        });
        return true;
      }
      case "input": {
        const { type: _t, ...env } = frame;
        void _t;
        const ack = host.handleInput(env);
        this.#later(30, () => this.#frame({ type: "input_ack", ...ack }));
        return true;
      }
      case "renew_lease":
        host.controlSim.renew(frame.lease_id, frame.generation);
        return true;
    }
  }

  listen(listener: (e: LinkEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #frame(frame: ServerFrame): void {
    if (this.#state.state !== "online") return;
    this.#emit({ type: "frame", frame });
  }

  #setState(state: LinkState): void {
    this.#state = state;
    this.#emit({ type: "state", state });
  }

  #emit(e: LinkEvent): void {
    for (const l of this.#listeners) l(e);
  }

  #later(ms: number, fn: () => void): void {
    this.#timers.push(setTimeout(fn, ms));
  }
}

// ---- Media -------------------------------------------------------------------

class SimMediaLink implements MediaLink {
  #listeners = new Set<(e: MediaEvent) => void>();
  #timer: ReturnType<typeof setInterval> | null = null;
  #seq = 0;
  #params: StreamParams | null = null;
  #targetId: string | null = null;
  #canvas: HTMLCanvasElement | null = null;
  #t0 = performance.now();
  #busy = false;
  #closed = false;
  #revision = -1;

  constructor(private readonly env: SimEnv) { env.side.add(this); }

  lose(reason: string): void { this.#emit({ type: "state", state: { state: "offline", since: Date.now(), reason, retryAt: null } }); }
  connect(): void { if (!this.#closed) this.#emit({ type: "state", state: { state: "online", since: Date.now() } }); }

  send(frame: MediaClientFrame): boolean {
    if (this.env.forcedOffline || this.env.revoked || this.#closed) return false;
    switch (frame.type) {
      case "open": {
        if (!this.env.host.controlSim.captureAllowed(frame.target_id)) {
          this.#later(() => this.#emit({ type: "text", frame: { type: "ended", reason: "Computer is locked" } }));
          return true;
        }
        if (frame.params.reopen) this.env.host.controlSim.reopen(frame.target_id);
        const target = this.env.host.targets.find((t) => t.target_id === frame.target_id);
        if (!target || target.capture.state !== "available") {
          this.#later(() => this.#emit({ type: "text", frame: { type: "ended", reason: "This display can't be viewed" } }));
          return true;
        }
        this.#targetId = frame.target_id;
        this.#params = frame.params;
        this.#later(() => {
          if (this.#closed) return;
          this.#revision = target.geometry.revision;
          this.#emit({ type: "state", state: { state: "online", since: Date.now() } });
          this.#emit({
            type: "text",
            frame: { type: "opened", session_id: "sim-media", target_id: frame.target_id, geometry: target.geometry, tile: 4096 },
          });
          this.#start();
        });
        return true;
      }
      case "adjust":
        this.#params = frame.params;
        this.#start();
        return true;
      case "ack":
      case "request_keyframe":
        return true;
      case "close":
        this.close();
        return true;
    }
  }

  close(): void {
    this.#closed = true;
    this.env.side.delete(this);
    if (this.#timer) clearInterval(this.#timer);
    this.#timer = null;
  }

  listen(listener: (e: MediaEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #start(): void {
    if (this.#timer) clearInterval(this.#timer);
    const fps = Math.max(1, Math.min(this.#params?.max_fps ?? 8, 10));
    this.#timer = setInterval(() => void this.#frame(), 1000 / fps);
  }

  async #frame(): Promise<void> {
    if (this.#busy || this.#closed || this.env.revoked || this.env.host.controlSim.frozen || !this.#params || !this.#targetId) return;
    if (this.env.forcedOffline) return;
    if (!this.env.host.controlSim.captureAllowed(this.#targetId)) return;
    const target = this.env.host.targets.find((t) => t.target_id === this.#targetId);
    if (!target) return;
    this.#busy = true;
    try {
      const g = target.geometry;
      if (this.#revision !== g.revision) { this.#revision = g.revision; this.#emit({ type: "text", frame: { type: "geometry_changed", geometry: g } }); }
      // Like the computer, crop to the region asked for before scaling.
      const r = this.#params.region;
      const region = r
        ? (() => {
            const x = Math.min(g.width - 1, Math.floor(r.x * g.width));
            const y = Math.min(g.height - 1, Math.floor(r.y * g.height));
            return {
              x,
              y,
              width: Math.max(1, Math.min(g.width, Math.ceil((r.x + r.width) * g.width)) - x),
              height: Math.max(1, Math.min(g.height, Math.ceil((r.y + r.height) * g.height)) - y),
            };
          })()
        : null;
      const area = region ?? { x: 0, y: 0, width: g.width, height: g.height };
      const scale = Math.min(1, this.#params.max_width / area.width, this.#params.max_height / area.height);
      const w = Math.max(1, Math.round(area.width * scale));
      const h = Math.max(1, Math.round(area.height * scale));
      const canvas = (this.#canvas ??= document.createElement("canvas"));
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      const t = (performance.now() - this.#t0) / 1000;
      const view = this.env.host.control.find((c) => c.target_id === this.#targetId);
      ctx.setTransform(1, 0, 0, 1, -area.x * scale, -area.y * scale);
      const painted = paintDesktop(ctx, g.width * scale, g.height * scale, t, target.label, view?.input_state === "agent");
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      const cx = Math.round(painted.x - area.x * scale), cy = Math.round(painted.y - area.y * scale);
      const cursor = cx >= 0 && cy >= 0 && cx < w && cy < h ? { x: cx, y: cy } : null;
      const blob = await new Promise<Blob | null>((r) => canvas.toBlob(r, "image/jpeg", (this.#params?.quality ?? 70) / 100));
      if (!blob) return;
      const jpeg = new Uint8Array(await blob.arrayBuffer());
      const data = buildTileFrame(
        {
          keyframe: true,
          cursor,
          tile: Math.max(w, h),
          seq: ++this.#seq,
          geometryRevision: g.revision,
          encWidth: w,
          encHeight: h,
          srcWidth: g.width,
          srcHeight: g.height,
          region,
        },
        [{ col: 0, row: 0, jpeg }],
      );
      this.#emit({ type: "binary", data });
    } finally {
      this.#busy = false;
    }
  }

  #emit(e: MediaEvent): void {
    for (const l of this.#listeners) l(e);
  }

  #later(fn: () => void): void {
    setTimeout(fn, 180);
  }
}

// ---- Terminal ------------------------------------------------------------------

// ---- Speech ----------------------------------------------------------------------

class SimSpeechLink implements SpeechLink {
  #listeners = new Set<(e: SpeechEvent) => void>();
  #timers: ReturnType<typeof setTimeout>[] = [];
  #session: string | null = null;

  constructor(private readonly env: SimEnv) {
    this.#at(0, () => this.#emit({ type: "state", state: env.forcedOffline ? { state: "offline", since: Date.now(), reason: "Offline", retryAt: null } : { state: "online", since: Date.now() } }));
  }

  send(frame: SpeechClientFrame): boolean {
    if (this.env.forcedOffline) return false;
    switch (frame.type) {
      case "start": {
        this.#session = frame.session_id;
        const words = "Give these more breathing room".split(" ");
        this.#at(100, () => this.#emit({ type: "capturing" }));
        this.#at(150, () => this.#emit({ type: "text", frame: { type: "ready", session_id: frame.session_id, engine: "whisper small.en" } }));
        let text = "";
        words.forEach((w, i) => {
          this.#at(400 + i * 260, () => {
            text += (i ? " " : "") + w;
            this.#emit({ type: "level", rms: 0.2 + 0.5 * Math.abs(Math.sin(i)) });
            this.#emit({
              type: "text",
              frame: {
                type: "segment",
                segment: { segment_id: "s1", text, start_ms: 0, end_ms: 400 + i * 260, revision: i + 1, is_final: false },
              },
            });
          });
        });
        this.#at(7000, () => this.#emit({ type: "text", frame: { type: "segment", segment: {
          segment_id: "s1", text: "Give these more breathing room.", start_ms: 0, end_ms: 4200, revision: 40, is_final: true,
        } } }));
        this.#at(11000, () => this.#emit({ type: "text", frame: { type: "segment", segment: {
          segment_id: "s1", text: "Give the heading and cards more breathing room.", start_ms: 0, end_ms: 4200, revision: 41, is_final: true,
        } } }));
        return true;
      }
      case "finish": {
        const id = this.#session;
        this.#clear();
        this.#at(1200, () => {
          this.#emit({
            type: "text",
            frame: {
              type: "segment",
              segment: {
                segment_id: "s1",
                text: "Give the heading and cards more breathing room.",
                start_ms: 0,
                end_ms: 4200,
                revision: 99,
                is_final: true,
              },
            },
          });
          if (id) this.#emit({ type: "text", frame: { type: "finished", session_id: id, audio_artifact: null } });
        });
        return true;
      }
      case "cancel":
        this.#clear();
        return true;
    }
  }

  close(): void {
    this.#clear();
  }

  listen(listener: (e: SpeechEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #at(ms: number, fn: () => void): void {
    this.#timers.push(setTimeout(fn, ms));
  }

  #clear(): void {
    for (const t of this.#timers) clearTimeout(t);
    this.#timers = [];
  }

  #emit(e: SpeechEvent): void {
    for (const l of this.#listeners) l(e);
  }
}

// ---- Environment -------------------------------------------------------------------

// ---- Transfers -----------------------------------------------------------------

async function sha256Hex(parts: Uint8Array[]): Promise<string> {
  const size = parts.reduce((n, p) => n + p.byteLength, 0);
  const all = new Uint8Array(size);
  let o = 0;
  for (const p of parts) {
    all.set(p, o);
    o += p.byteLength;
  }
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", all));
  return [...d].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Receives upload bytes into memory and hands the result to the simulated host. */
class SimTransferLink implements TransferLink {
  #listeners = new Set<(e: TransferEvent) => void>();
  #uploading: { id: string; parts: Uint8Array[]; received: number } | null = null;
  /** Bumped to stop a download stream (cancel, drop, close). */
  #stream = 0;

  constructor(private readonly env: SimEnv) {
    env.side.add(this);
  }

  lose(reason: string): void {
    this.#stream++;
    this.#uploading = null;
    this.#emit({ type: "state", state: { state: "offline", since: Date.now(), reason, retryAt: null } });
  }

  connect(): void {
    this.#emit({ type: "state", state: { state: "online", since: Date.now() } });
  }

  send(frame: TransferClientFrame): boolean {
    if (this.env.forcedOffline) return false;
    const host = this.env.host;
    switch (frame.type) {
      case "upload": {
        const tr = host.transfers.find((t) => t.transfer_id === frame.transfer_id);
        if (!tr) {
          this.#later(() => this.#emit({ type: "text", frame: { type: "failed", transfer_id: frame.transfer_id, reason: "No such transfer" } }));
          return true;
        }
        if (tr.state.state === "cancelled" || tr.state.state === "completed") {
          this.#later(() => this.#emit({ type: "text", frame: { type: "failed", transfer_id: frame.transfer_id, reason: "This transfer is already closed" } }));
          return true;
        }
        const stage = host.uploadStaging.open(frame.transfer_id);
        this.#uploading = stage;
        this.#later(() => this.#emit({ type: "text", frame: { type: "resume_at", transfer_id: frame.transfer_id, offset: stage.received } }));
        return true;
      }
      case "finish": {
        const u = this.#uploading;
        if (!u || u.id !== frame.transfer_id) return false;
        this.#uploading = null;
        const tr = host.transfers.find((t) => t.transfer_id === u.id);
        void sha256Hex(u.parts).then(async (sha) => {
          const r = await host.finishUpload(u.id, sha, mediaTypeFor(tr?.name ?? ""), concat(u.parts));
          this.#emit({
            type: "text",
            frame: r.ok
              ? { type: "completed", transfer_id: u.id, sha256: sha, artifact_id: r.artifactId }
              : { type: "failed", transfer_id: u.id, reason: r.reason },
          });
        });
        return true;
      }
      case "cancel":
        if (this.#uploading?.id === frame.transfer_id) this.#uploading = null;
        this.#stream++;
        return true;
      case "download": {
        const id = frame.transfer_id;
        const d = host.downloadBytes(id);
        if (!d) {
          this.#later(() => this.#emit({ type: "text", frame: { type: "failed", transfer_id: id, reason: "No such transfer" } }));
          return true;
        }
        // The simulated computer keeps the bytes, so a resumed download
        // really continues from the phone's offset.
        const offset = Math.min(Number(frame.offset), d.bytes.length);
        const token = ++this.#stream;
        this.#later(() => {
          this.#emit({ type: "text", frame: { type: "sending", transfer_id: id, offset, size: d.bytes.length, sha256: d.sha256 } });
          void this.#send(token, id, d.bytes, d.sha256, offset);
        });
        return true;
      }
    }
  }

  async #send(token: number, id: string, bytes: Uint8Array, sha: string, offset: number): Promise<void> {
    for (let at = offset; at < bytes.length; ) {
      await new Promise((r) => setTimeout(r, this.env.host.uploadStaging.delay));
      if (token !== this.#stream || this.env.forcedOffline) return;
      const end = Math.min(bytes.length, at + 256 * 1024);
      this.#emit({ type: "chunk", data: bytes.slice(at, end) });
      this.env.host.transferProgress(id, end);
      at = end;
    }
    if (token !== this.#stream) return;
    this.env.host.finishDownload(id);
    this.#emit({ type: "text", frame: { type: "completed", transfer_id: id, sha256: sha, artifact_id: null } });
  }

  async chunk(bytes: Uint8Array): Promise<boolean> {
    const u = this.#uploading;
    if (this.env.forcedOffline || !u) return false;
    u.parts.push(bytes.slice());
    u.received += bytes.byteLength;
    this.env.host.transferProgress(u.id, u.received);
    await new Promise((r) => setTimeout(r, this.env.host.uploadStaging.delay));
    return true;
  }

  close(): void {
    this.#uploading = null;
    this.#stream++;
    this.#listeners.clear();
    this.env.side.delete(this);
  }

  listen(listener: (e: TransferEvent) => void): () => void {
    this.#listeners.add(listener);
    this.#later(() => listener({ type: "state", state: this.env.forcedOffline ? { state: "offline", since: Date.now(), reason: this.env.offlineReason, retryAt: null } : { state: "online", since: Date.now() } }));
    return () => this.#listeners.delete(listener);
  }

  #later(fn: () => void): void {
    setTimeout(fn, 60 + this.env.latencyMs);
  }

  #emit(e: TransferEvent): void {
    for (const l of this.#listeners) l(e);
  }
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.byteLength, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.byteLength;
  }
  return out;
}

function mediaTypeFor(name: string): string {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const known: Record<string, string> = {
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    webp: "image/webp",
    gif: "image/gif",
    mp4: "video/mp4",
    webm: "video/webm",
    md: "text/markdown",
    txt: "text/plain",
    pdf: "application/pdf",
  };
  return known[ext] ?? "application/octet-stream";
}

class SimEnv {
  refusals = new Map<SideKind, SideRefusal>();
  openSide<T extends TerminalLink | MediaLink | SpeechLink | TransferLink>(kind: SideKind, create: () => T): T {
    const refusal = this.refusals.get(kind);
    if (!refusal) return create();
    this.refusals.delete(kind);
    return refusedSide(refusal) as T;
  }
  incompatible = false;
  grants: Grants = { ...allGrants };
  host = new SimHost({ variant: parseHostVariant(new URLSearchParams(location.search).get("host")), indicator: new URLSearchParams(location.search).get("indicator") !== "missing" });
  links = new Set<SimControlLink>();
  forcedOffline = false;
  offlineReason = "Can't reach studio";
  mismatch = false;
  dropReceipt = false;
  latencyMs = 0;
  revoked = false;
  /** Side links (transfers) that see the connection drop and return too. */
  side = new Set<{ lose(reason: string): void; connect(): void }>();

  loseAll(reason: string): void {
    for (const l of this.links) l.lose(reason);
    for (const l of this.side) l.lose(reason);
  }
}

const PAIRED_KEY = "sim.paired";
const NAMES_KEY = "sim.profile-names";

/** Names given on this device, kept across reloads as the app's profile store keeps them. */
function localNames(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(NAMES_KEY) ?? "{}") as Record<string, string>;
  } catch {
    return {};
  }
}

export function createSimPlatform(): Platform {
  const env = new SimEnv();
  const device = simulatedDevice();
  const params = new URLSearchParams(location.search);
  reportPhone.detection = params.has("report-phone");
  reportPhone.capture = params.has("report-picture");
  if (params.has("report-crash")) env.host.reports.crash();
  if (params.has("unpaired")) localStorage.removeItem(PAIRED_KEY);
  else if (localStorage.getItem(PAIRED_KEY) === null) localStorage.setItem(PAIRED_KEY, "1");
  if (params.has("offline")) env.forcedOffline = true;
  env.incompatible = params.has("incompatible");
  const lifecycle = simulatedLifecycle(params.has("lock-test"));
  // `same-name`: an older pairing of a reinstalled computer, still called the
  // same, at another port and certificate.
  const spare = new SimEnv(), otherProfile = params.has("same-name")
    ? { ...secondProfile(PROFILE), name: PROFILE.name, addresses: ["studio.tail1234.ts.net:47101"],
      certSha256: "c29e04b7a1f35d6e8c0b2a4f6d8e1c3b5a7f9e0d2c4b6a8f1e3d5c7b9a0f2e4d", pairedAt: PROFILE.pairedAt - 90 * 24 * 3600_000 }
    : secondProfile(PROFILE);
  spare.host.host = { ...spare.host.host, host_id: otherProfile.hostId, name: otherProfile.name };
  spare.host.projects = [];
  spare.host.tasks = []; spare.host.runs = []; spare.host.approvals = []; spare.host.attention = [];
  spare.host.control = []; spare.host.targets = [];
  const environment = (profileId: string) => profileId === otherProfile.profileId ? spare : env;

  const drills = controlDrills(() => env.host);
  const owner = simulatedOwner();
  const controls: SimControls = {
    reports: reportPhone,
    pairing: owner.controls,
    stops: {
      ignore(taskId, activity, ends) { env.host.stopSafety.ignore(taskId, activity, ends); },
      restart(taskId) { controls.restart(); env.host.stopSafety.reportRunning(taskId); controls.online(); },
    },
    lifecycle: lifecycle.controls,
    device: device.controls,
    host: env.host,
    library: env.host.library,
    transfers: {
      chunkDelay(ms) { env.host.uploadStaging.delay = Math.max(0, ms); },
      interruptPublication() { env.host.interruptPublication = true; },
    },
    work: env.host.work,
    control: drills,
    media: { freeze: drills.freeze },
    offline(reason) {
      env.forcedOffline = true;
      env.mismatch = false;
      if (reason) env.offlineReason = reason;
      env.loseAll(env.offlineReason);
    },
    impostor() {
      env.forcedOffline = true;
      env.mismatch = true;
      env.loseAll(SIM_MISMATCH);
    },
    online() {
      env.forcedOffline = false;
      env.mismatch = false;
      for (const l of env.links) l.connect();
      for (const l of env.side) l.connect();
    },
    permissions(grants) {
      env.grants = { ...env.grants, ...grants };
      env.loseAll("Access grants changed");
      for (const l of env.links) l.connect();
    },
    dropNextReceipt() {
      env.dropReceipt = true;
    },
    latency(ms) {
      env.latencyMs = ms;
    },
    revoke() {
      env.revoked = true;
      for (const l of env.links) l.revoke();
      for (const l of env.side) l.lose("This device was removed on the computer.");
    },
    pace(m) {
      env.host.setPace(m);
    },
    refuseSide(kind, code, reason) {
      env.refusals.set(kind, { code, reason: reason ?? (code === "not_found"
        ? `The ${kind} is no longer on the computer.`
        : `The computer refused the ${kind} connection (${code}). Check its settings.`) });
    },
    handoff(over) {
      const { packetJson, handoffJson } = fixtureHandoff(Date.now(), over);
      return handoffs().receive(packetJson, handoffJson)?.id ?? null;
    },
    restart() {
      const next = new SimHost({ variant: env.host.variant, indicator: env.host.indicator });
      next.terminals = [];
      env.host.dispose();
      env.host = next;
      controls.host = next;
      controls.library = next.library;
      controls.work = next.work;
      env.loseAll("Simulated computer restarted");
    },
  };
  window.__sim = controls;
  // `handoff`: one fixture handoff waits under Tasks, as if just delivered.
  if (params.has("handoff")) controls.handoff();

  return {
    kind: "simulated",
    preview: (id, preview, _path, changed) => simulatedPreview(environment(id), preview, changed),
    ...device.capabilities,
    ...lifecycle.capabilities,
    async appInfo() { return { ...await device.capabilities.appInfo!(), protocol: { major: 1, minor: 0 } }; },
    async profiles() {
      const names = localNames();
      const paired = params.has("two-computers") || params.has("same-name") ? [PROFILE, otherProfile] : [PROFILE];
      return localStorage.getItem(PAIRED_KEY) === "1" ? paired.map((p) => ({ ...p, name: names[p.profileId] ?? p.name })) : [];
    },
    parsePairingLink,
    async pair(offer: PairingOffer, _deviceName: string, replace: boolean, waiting: (code: string) => void): Promise<PairingResult> {
      if (!replace && (await this.profiles()).some((p) => p.hostId === offer.host_id))
        throw new Error("This phone is already paired with that computer. Choose to replace it to pair again.");
      await new Promise((r) => setTimeout(r, 400));
      waiting(SIM_PAIRING_CODE);
      await owner.answer();
      localStorage.setItem(PAIRED_KEY, "1");
      return {
        profile: { ...PROFILE, name: offer.host_name, hostId: offer.host_id, certSha256: offer.cert_sha256, pairedAt: Date.now() },
        response: { device_id: fx.ids.device, host_id: offer.host_id, host_name: offer.host_name, grants: allGrants },
      };
    },
    async cancelPairing() {
      owner.withdraw();
    },
    async renameProfile(profileId: string, name: string) {
      const profile = (await this.profiles()).find((p) => p.profileId === profileId);
      if (!profile) throw new Error("This computer is no longer paired with this device.");
      const trimmed = name.trim();
      if (!trimmed || trimmed.length > 64) throw new Error("Give the computer a name of up to 64 characters.");
      localStorage.setItem(NAMES_KEY, JSON.stringify({ ...localNames(), [profileId]: trimmed }));
      return { ...profile, name: trimmed };
    },
    async forget() {
      localStorage.setItem(PAIRED_KEY, "0");
      localStorage.removeItem(NAMES_KEY);
      localStorage.removeItem(SIM_OUTBOX_KEY);
    },
    outbox: simOutbox(),
    control: (id) => new SimControlLink(environment(id)),
    media: (id) => environment(id).openSide("media", () => new SimMediaLink(environment(id))),
    terminal: (id) => environment(id).openSide("terminal", () => new SimControlTerminal(environment(id))),
    speech: (id) => environment(id).openSide("speech", () => new SimSpeechLink(environment(id))),
    transfer: (id) => environment(id).openSide("transfer", () => new SimTransferLink(environment(id))),
    async scanPairingCode() {
      await new Promise((r) => setTimeout(r, 1200));
      return simPairingLink();
    },
    async openExternal(url: string) {
      window.open(url, "_blank", "noopener");
    },
  };
}

const SIM_PAIRING_CODE = "482913";
/** What the native link says when no address presents the pinned certificate. */
const SIM_MISMATCH = "Another device answered at the computer’s address with a different certificate, so nothing was sent to it.";

/** The owner answering pairing requests at the simulated computer. */
function simulatedOwner() {
  let held = false;
  let pending: { resolve: () => void; reject: (e: unknown) => void } | null = null;
  const settle = (outcome: "confirm" | ProtocolError) => {
    const request = pending;
    pending = null;
    if (outcome === "confirm") request?.resolve();
    else request?.reject(outcome);
  };
  return {
    answer: () => new Promise<void>((resolve, reject) => {
      pending = { resolve, reject };
      if (!held) setTimeout(() => settle("confirm"), 1200);
    }),
    withdraw: () => settle({ code: "temporarily_unavailable", message: "The connection closed before the computer confirmed the operation." }),
    controls: {
      hold() { held = true; },
      confirm() { settle("confirm"); },
      refuse() { settle({ code: "permission_denied", message: "The computer's owner refused this device" }); },
      expire() { settle({ code: "expired", message: "The pairing code expired before it was confirmed on the computer" }); },
    },
  };
}

const SIM_OUTBOX_KEY = "sim.outbox";

/** The simulated phone's outbox: localStorage stands in for the synced file
 * the app writes, so reloading mid-send exercises the same recovery. */
function simOutbox(): SendOutbox {
  const read = (): Record<string, string> => {
    try {
      return JSON.parse(localStorage.getItem(SIM_OUTBOX_KEY) ?? "{}") as Record<string, string>;
    } catch {
      return {};
    }
  };
  const write = (records: Record<string, string>) =>
    localStorage.setItem(SIM_OUTBOX_KEY, JSON.stringify(records));
  return {
    async put(profileId, commandId, record) {
      write({ ...read(), [`${profileId}/${commandId}`]: record });
    },
    async list(profileId) {
      return Object.entries(read())
        .filter(([key]) => key.startsWith(`${profileId}/`))
        .map(([, record]) => record);
    },
    async remove(profileId, commandId) {
      const records = read();
      delete records[`${profileId}/${commandId}`];
      write(records);
    },
  };
}

/** A well-formed pairing link for the simulated computer. */
export function simPairingLink(): string {
  const offer: PairingOffer = {
    host_id: fx.ids.host,
    host_name: "studio",
    addresses: PROFILE.addresses,
    cert_sha256: PROFILE.certSha256,
    pairing_secret: "c2ltdWxhdGVkLXBhaXJpbmctc2VjcmV0LTMyYnl0ZXM",
    expires_at: Date.now() + 5 * 60_000,
    protocol: { major: 1, minor: 0 },
  };
  return `${PAIRING_SCHEME}://pair?d=${base64UrlEncode(JSON.stringify(offer))}`;
}

/** The computer's query grants (`domain::grants::query`). */
function queryAllowed(g: Grants, q: Query): boolean {
  switch (q.kind) {
    case "project_stages":
      return g.tasks || g.admin;
    case "snapshot":
    case "command_status":
      return true;
    case "task_timeline":
    case "task_history":
    case "task":
    case "run":
    case "harnesses":
    case "previews":
    case "project_branches":
    case "findings":
    case "queue":
    case "observed_sessions":
    case "catalog":
    case "github_project":
    case "github_items":
    case "github_pr":
      return g.tasks;
    case "control_targets":
    case "windows":
    case "system_stats":
    case "agent_updates":
    case "agent_preferences":
    case "notification_settings":
    case "reports":
    case "report":
    case "report_preview":
      return g.view;
    case "inbox_report_code":
      return g.admin;
    case "voice":
      return g.view && g.control_input;
    case "library_list":
    case "library_read":
    case "library_search":
    case "instructions":
    case "library_trash":
    case "library_recovery_list":
    case "library_recovery_read":
    case "library_trash_read":
    case "agent_files":
    case "disabled_in_ophio":
      return g.library_read;
    case "terminals":
      return g.terminal;
    case "accounts":
    case "agent_logins":
    case "agent_login_password":
      return g.accounts;
    case "checklists":
    case "checklist":
      return g.tasks;
    case "file_delivery":
      return g.tasks || g.admin;
    case "readiness":
    case "contribution_delivery":
    case "contribution_capabilities":
    case "contribution_policy":
    case "contribution_operation":
    case "devices":
    case "channel_link":
    case "project_folders":
      return g.admin;
    case "approval":
      return g.approvals;
    case "task_artifacts":
    case "project_shown":
      return g.tasks && g.transfers;
    case "transfers":
    case "artifact":
      return g.transfers;
    case "reusable_formats":
    case "reusable_draft":
      return g.tasks && g.library_read;
    case "upload_destination":
      return (
        g.transfers &&
        (q.destination.kind !== "project_dir" || g.tasks)
      );
  }
}
