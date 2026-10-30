// SPDX-License-Identifier: Apache-2.0
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { SpeechEvent } from "../../host/link";
import type { ReactNode } from "react";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import type { VoiceSessionView, VoiceView } from "../../protocol/types";
import Voice from "./Voice";
import VoiceSession, { VoicePrompt } from "./VoiceSession";
import { VoiceSettings } from "./VoiceSettings";
let online = true;
let view: VoiceView;
const command = vi.fn().mockResolvedValue({ status: "accepted" });
const renewLease = vi.fn();
const push = vi.fn();
const reload = vi.fn();
let speechListener: ((event: SpeechEvent) => void) | undefined;
const speechSend = vi.fn().mockReturnValue(true);
const speechClose = vi.fn();
const platform = { speech: () => ({ send: speechSend, close: speechClose, listen: (callback: (event: SpeechEvent) => void) => { speechListener = callback; return () => { speechListener = undefined; }; } }) };
vi.mock("../../app/context", () => ({
 useApp: () => ({ session: { command, renewLease }, platform, profile: { profileId: "phone", name: "Test computer" }, ui: { push } }),
 useHost: <T,>(select: (s: HostState) => T) => select({ ...emptyHostState, grants: { view: true, control_input: true, admin: true, tasks: true, terminal: false, computer_use: true, approvals: false, library_read: true, library_write: false, transfers: false, accounts: false } }),
 useLink: () => ({ state: online ? "online" : "offline" }), describeError: () => "Unavailable",
}));
vi.mock("./VoiceState", () => ({ useVoice: () => ({ view, error: null, reload }), useVoiceVisible: () => true }));
vi.mock("../../shell/Screen", () => ({ Screen: ({ title, children, footer }: { title: string; children: ReactNode; footer?: ReactNode }) => <main><h1>{title}</h1>{children}{footer}</main> }));
vi.mock("../../host/commandResult", () => ({ commandResult: async () => ({ kind: "voice_session", session: view.session }) }));
vi.mock("../../design/back", () => ({ useBackLayer() {} }));
function sessionView(): VoiceSessionView { return { session_id: "session", target: "browser", target_id: "target", lease: { lease_id: "lease", generation: 1, boot_epoch: "boot", target_id: "target", owner: { kind: "human", device_id: "phone" }, state: "active", granted_at: 1, renewal_window_ms: 3000 }, dry_run: false, phase: "ready", transcript: "", understood: "Ready for a command", prompt_id: 3, choices: [], steps: [], ended: null, picture: null }; }
beforeEach(() => { localStorage.clear(); online = true; command.mockClear(); renewLease.mockClear(); view = { settings: { browser: true, desktop: true, revision: 7 }, voice_browser: { state: "available" }, voice_desktop: { state: "temporarily_unavailable", reason: "Enable accessibility on the computer" }, speech: { state: "permission_missing", remedy: "Install a local speech model" }, session: sessionView() }; });
afterEach(cleanup);
it("shows Jev gating and speech installation without an implicit start", () => { view.voice_browser = { state: "temporarily_unavailable", reason: "Link Jev on the computer to use voice control: ophio jev connect" }; render(<Voice route={{ screen: "voice" }} />); expect(screen.getByText(/Link Jev/)).toBeTruthy(); expect(screen.getByText(/Enable accessibility/)).toBeTruthy(); fireEvent.click(screen.getByRole("button", { name: "Set up local speech" })); expect(push).toHaveBeenCalledWith({ screen: "settings", section: "settings-voice" }); expect(command).not.toHaveBeenCalled(); });
it("numbers disambiguation choices, focuses the prompt and binds a tap to its revision", () => { const value = sessionView(); value.phase = "disambiguation"; value.understood = "Say or tap 1–3"; value.choices = ["First", "Second", "Third"].map((name, i) => ({ id: `e${i}`, name, role: "link", bounds: { x: 0, y: 0, width: 20, height: 20 }, enabled: true, visible: true, focused: false, sensitive: false, destructive: false })); const send = vi.fn(); render(<VoicePrompt value={value} disabled={false} send={send} />); expect(document.activeElement?.getAttribute("aria-label")).toBe("Understood command"); fireEvent.click(screen.getByRole("button", { name: "2. Second · link" })); expect(send).toHaveBeenCalledWith({ operation: "choose", session_id: "session", prompt_id: 3, element_id: "e1" }); });
it("requires explicit Confirm and does not send on rendering", () => { const value = { ...sessionView(), phase: "confirm" as const, understood: "Delete draft" }; const send = vi.fn(); render(<VoicePrompt value={value} disabled={false} send={send} />); expect(send).not.toHaveBeenCalled(); fireEvent.click(screen.getByRole("button", { name: "Confirm action" })); expect(send).toHaveBeenCalledWith({ operation: "confirm", session_id: "session", prompt_id: 3 }); });
it("keeps the typed draft on remount and offers the keyboard after sensitive-field refusal", () => { view.session = { ...sessionView(), phase: "refused", understood: "Use the remote keyboard for sensitive fields" }; const mounted = render(<VoiceSession sessionId="session" />); fireEvent.change(screen.getByLabelText("Typed command"), { target: { value: "unfinished command" } }); fireEvent.click(screen.getByRole("button", { name: "Open remote keyboard" })); expect(push).toHaveBeenCalledWith({ screen: "desktop", targetId: "target" }); mounted.unmount(); render(<VoiceSession sessionId="session" />); expect((screen.getByLabelText("Typed command") as HTMLTextAreaElement).value).toBe("unfinished command"); });
it("keeps the last picture and disables all voice actions offline", () => { online = false; view.session = { ...sessionView(), picture: { png_base64: "iVBOR", at: Date.now() - 5000 }, phase: "confirm", understood: "Delete draft" }; render(<VoiceSession sessionId="session" />); expect(screen.getByText(/Last picture/)).toBeTruthy(); expect(screen.getByRole("img").getAttribute("alt")).toContain("Last picture"); for (const name of ["Confirm action", "Cancel command", "Send command", "Stop voice control", "Hold to talk"]) expect((screen.getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(true); expect(renewLease).not.toHaveBeenCalled(); });
it("revision-checks voice feature settings and shows what is in force until the computer has a change", async () => {
  let answer!: () => void;
  command.mockImplementationOnce(() => new Promise(r => { answer = () => r({ status: "accepted" }); }));
  render(<VoiceSettings />);
  const browser = screen.getByRole("checkbox", { name: "Browser voice control" }) as HTMLInputElement;
  const desktop = screen.getByRole("checkbox", { name: "Desktop voice control" }) as HTMLInputElement;
  fireEvent.click(browser);
  // Voice control lets Jev work the computer: it stays on, and usable, until the computer has it off.
  expect(browser.checked).toBe(true); expect(browser.disabled).toBe(false);
  expect(command).toHaveBeenCalledWith({ command: "set_voice", browser: false, desktop: true }, { label: "Turn off browser voice control", expectedRevision: 7 });
  await waitFor(() => expect(screen.getByText(/turning browser voice control off/)).toBeTruthy());
  // A second tap while the first is on its way keeps the first, and goes against the revision it leaves.
  fireEvent.click(desktop);
  expect(command).toHaveBeenCalledTimes(1);
  answer();
  await waitFor(() => expect(command).toHaveBeenLastCalledWith({ command: "set_voice", browser: false, desktop: false }, { label: "Turn off desktop voice control", expectedRevision: 8 }));
});
it("says once why voice control can't be used when Jev isn't linked, and offers no Turn off", () => {
  const unlinked = { state: "temporarily_unavailable", reason: "Link Jev on the computer to use voice control: ophio jev connect" } as const;
  view.voice_browser = unlinked; view.voice_desktop = unlinked;
  render(<VoiceSettings />);
  expect(screen.getAllByText(/Link Jev/)).toHaveLength(1);
  expect(screen.getByText(/^Voice control: /)).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Turn off/ })).toBeNull();
  expect((screen.getByRole("checkbox", { name: "Desktop voice control" }) as HTMLInputElement).checked).toBe(true);
});

it("cancels the microphone when the app enters the background", () => {
  view.speech = { state: "available" }; speechSend.mockClear(); speechClose.mockClear();
  render(<VoiceSession sessionId="session" />);
  fireEvent.click(screen.getByRole("button", { name: "Hold to talk" }));
  speechListener?.({ type: "state", state: { state: "online", since: 1 } });
  expect(speechSend).toHaveBeenCalledWith(expect.objectContaining({ type: "start", keep_audio: false }));
  const original = Object.getOwnPropertyDescriptor(document, "hidden");
  Object.defineProperty(document, "hidden", { configurable: true, value: true });
  fireEvent(document, new Event("visibilitychange"));
  expect(speechSend).toHaveBeenLastCalledWith({ type: "cancel" }); expect(speechClose).toHaveBeenCalledOnce();
  if (original) Object.defineProperty(document, "hidden", original); else Reflect.deleteProperty(document, "hidden");
});
