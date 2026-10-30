// SPDX-License-Identifier: Apache-2.0
import type { Availability, CommandResult, VoiceElement, VoiceOperation, VoiceSessionView, VoiceSettings, VoiceView } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import type { SimHost } from "./host";
import { ids } from "./scenario";
const available: Availability = { state: "available" };
export class VoiceSim {
  settings: VoiceSettings = { revision: 0, browser: true, desktop: true };
  desktopAvailable = false;
  fail = false;
  sessions = new Map<string, VoiceSessionView>();
  link(host: SimHost, connected = true) { host.jev = { connection: connected ? "connected" : "not_connected", checked_at: Date.now() }; }
  view(host: SimHost, id: string | null): VoiceView {
    if (this.fail) throw { code: "temporarily_unavailable", message: "The computer could not check voice control." };
    const connection = host.jev.connection;
    const gate = (enabled: boolean, native: Availability): Availability => connection !== "connected" ? { state: "temporarily_unavailable", reason: connection === "last_check_failed" ? "The last Jev check failed. Check Jev access on the computer before using voice control" : "Link Jev on the computer to use voice control: ophio jev connect" } : !enabled ? { state: "temporarily_unavailable", reason: "Voice control is turned off in Settings" } : native;
    return structuredClone({ settings: this.settings, voice_browser: gate(this.settings.browser, available), voice_desktop: gate(this.settings.desktop, this.desktopAvailable ? available : { state: "temporarily_unavailable", reason: "No app on the computer that exposes its controls has focus. Bring one to the front; many terminals don't expose theirs." }), speech: host.host.capabilities.speech, session: id ? this.sessions.get(id) ?? null : null });
  }
  set(browser: boolean, desktop: boolean, revision: number | null) {
    if (revision !== this.settings.revision) throw { code: "stale_revision", message: "Voice settings changed. Check again." };
    this.settings = { browser, desktop, revision: this.settings.revision + 1 };
  }
  command(host: SimHost, op: VoiceOperation): CommandResult | null {
    if (op.operation === "start") {
      const caps = this.view(host, null);
      if ((op.target === "browser" ? caps.voice_browser : caps.voice_desktop).state !== "available" || !op.disclosure_accepted) throw { code: "permission_denied", message: "Voice control is unavailable" };
      const target = host.targets.find(t => t.target_id === op.target_id);
      if (!target || target.geometry.revision !== op.geometry_revision) throw { code: "stale_revision", message: "The target changed" };
      const value: VoiceSessionView = { session_id: uuidv7(Date.now()), target: op.target, target_id: op.target_id, lease: { lease_id: uuidv7(Date.now()), target_id: op.target_id, owner: { kind: "human", device_id: ids.device }, generation: 100, boot_epoch: host.bootEpoch, state: "active", granted_at: Date.now(), renewal_window_ms: 3000 }, dry_run: op.dry_run, phase: "ready", transcript: "", understood: "Ready for a command", choices: [], prompt_id: 0, picture: picture(), steps: [], ended: null };
      this.sessions.set(value.session_id, value);
      const control = { target_id: op.target_id, lease: value.lease, input_state: "human" as const, model_observation: null, private_step: null };
      host.control = [...host.control.filter(c => c.target_id !== op.target_id), control]; host.emit({ type: "control_changed", view: structuredClone(control) });
      return { kind: "voice_session", session: structuredClone(value) };
    }
    const s = this.sessions.get(op.session_id);
    if (!s) throw { code: "not_found", message: "Voice session not found" };
    if (op.operation === "stop") {
      s.ended = "stop"; s.phase = "stopped"; s.understood = "Voice control ended"; s.lease.state = "revoked";
      const control = host.control.find(c => c.target_id === s.target_id);
      if (control) { control.lease = s.lease; control.input_state = "paused"; host.emit({ type: "control_changed", view: structuredClone(control) }); }
      return { kind: "voice_session", session: structuredClone(s) };
    }
    if (s.ended) throw { code: "expired", message: "Voice control ended" };
    if (op.operation !== "transcript" && op.prompt_id !== s.prompt_id) throw { code: "stale_revision", message: "Review the current prompt" };
    if (op.operation === "cancel") { s.phase = "ready"; s.choices = []; s.understood = "Command cancelled"; }
    else if (op.operation === "choose") { const e = s.choices.find(e => e.id === op.element_id); if (!e) throw { code: "invalid_request", message: "Choose a listed control" }; this.applied(s, `Click “${e.name}”`); }
    else if (op.operation === "confirm") this.applied(s, s.understood);
    else if (op.operation === "transcript") {
      s.transcript = op.text;
      if (!op.final_segment) { s.phase = "assessing"; s.understood = "Listening to the command…"; return { kind: "voice_session", session: structuredClone(s) }; }
      s.choices = []; const words = op.text.toLowerCase();
      if (words === "confirm" && s.phase === "confirm") this.applied(s, s.understood);
      else if (words.includes("password")) { s.phase = "refused"; s.understood = "Voice typing is unavailable for password or sensitive fields. Use the remote keyboard"; }
      else if (words.includes("delete") || words.includes("buy") || words.includes("send")) { s.phase = "confirm"; s.prompt_id++; s.understood = "Click “Delete draft”"; }
      else if (words.includes("link") || words.includes("which")) { s.phase = "disambiguation"; s.prompt_id++; s.understood = "Say or tap 1–3"; s.choices = ["Documentation", "Project notes", "Release notes"].map((name, i): VoiceElement => ({ id: `e${i}`, role: "link", name, bounds: { x: 10, y: 20 + i * 50, width: 100, height: 40 }, enabled: true, focused: false, visible: true, sensitive: false, destructive: false })); }
      else this.applied(s, words.startsWith("go to ") ? `Open ${op.text.slice(6)}` : words.startsWith("type ") ? "Type into “Search”" : "Click “Sign in”");
    }
    return { kind: "voice_session", session: structuredClone(s) };
  }
  private applied(s: VoiceSessionView, understood: string) {
    s.phase = "ready"; s.choices = []; s.understood = understood; s.picture = picture(understood);
    s.steps.push({ utterance_id: uuidv7(Date.now()), transcript: s.transcript, at: Date.now(), answers: null, action: null, outcome: s.dry_run ? "dry_run" : "applied", detail: s.dry_run ? "Preview only. No input sent" : "The computer confirmed the input", jev_usage: { input_tokens: 240, output_tokens: 44 } });
  }
}
function picture(status = "Project notes") {
  if (typeof document === "undefined") return null;
  const c = document.createElement("canvas"); c.width = 900; c.height = 480;
  const x = c.getContext("2d"); if (!x) return null;
  x.fillStyle = "#faf9f5"; x.fillRect(0, 0, 900, 480); x.fillStyle = "#e9e6df"; x.fillRect(0, 0, 900, 52); x.fillStyle = "#303238"; x.font = "18px sans-serif"; x.fillText("Separate browser · Local fixture", 30, 33); x.font = "34px sans-serif"; x.fillText(status.slice(0, 45), 60, 130); x.font = "22px sans-serif"; x.fillText("Search project notes", 60, 220); x.strokeStyle = "#999a9f"; x.strokeRect(60, 240, 540, 55); x.fillStyle = "#3549be"; x.fillRect(60, 350, 170, 55); x.fillStyle = "#fff"; x.fillText("Sign in", 100, 384);
  return { png_base64: c.toDataURL("image/png").split(",")[1] ?? "", at: Date.now() };
}
