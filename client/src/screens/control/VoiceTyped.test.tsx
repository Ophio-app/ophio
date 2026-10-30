// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import type { VoiceSessionView, VoiceView } from "../../protocol/types";
import VoiceSession from "./VoiceSession";

let view: VoiceView;
const command = vi.fn();
const result = vi.fn();
vi.mock("../../app/context", () => ({
  useApp: () => ({ session: { command, renewLease: vi.fn() }, platform: {}, profile: { profileId: "phone", name: "Test computer" }, ui: { push: vi.fn() } }),
  useHost: <T,>(select: (s: HostState) => T) => select({ ...emptyHostState, grants: { view: true, control_input: true, admin: true, tasks: true, terminal: false, computer_use: true, approvals: false, library_read: true, library_write: false, transfers: false, accounts: false } }),
  useLink: () => ({ state: "online" }), describeError: (e: Error) => e.message,
}));
vi.mock("./VoiceState", () => ({ useVoice: () => ({ view, error: null, reload: vi.fn() }), useVoiceVisible: () => true }));
vi.mock("../../shell/Screen", () => ({ Screen: ({ title, children, footer }: { title: string; children: ReactNode; footer?: ReactNode }) => <main><h1>{title}</h1>{children}{footer}</main> }));
vi.mock("../../host/commandResult", () => ({ commandResult: () => result() }));
vi.mock("../../design/back", () => ({ useBackLayer() {} }));
function sessionView(): VoiceSessionView { return { session_id: "session", target: "browser", target_id: "target", lease: { lease_id: "lease", generation: 1, boot_epoch: "boot", target_id: "target", owner: { kind: "human", device_id: "phone" }, state: "active", granted_at: 1, renewal_window_ms: 3000 }, dry_run: false, phase: "ready", transcript: "", understood: "Ready for a command", prompt_id: 3, choices: [], steps: [], ended: null, picture: null }; }
beforeEach(() => {
  localStorage.clear(); command.mockReset(); result.mockReset();
  command.mockResolvedValue({ status: "accepted" });
  view = { settings: { browser: true, desktop: true, revision: 7 }, voice_browser: { state: "available" }, voice_desktop: { state: "available" }, speech: { state: "permission_missing", remedy: "Install a local speech model" }, session: sessionView() };
});
afterEach(cleanup);

const box = () => screen.getByLabelText("Typed command") as HTMLTextAreaElement;

it("a typed command the computer carried out is not left behind as an unsent draft", async () => {
  result.mockResolvedValue({ kind: "voice_session", session: { ...sessionView(), understood: "Click “Sign in”" } });
  const first = render(<VoiceSession sessionId="session" />);
  fireEvent.change(box(), { target: { value: "click sign in" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send command" })); });
  await waitFor(() => expect(command).toHaveBeenCalledTimes(1));
  expect(box().value).toBe("");
  // Reopening the session must not offer the same command again as a draft.
  first.unmount();
  render(<VoiceSession sessionId="session" />);
  expect(box().value).toBe("");
  expect((screen.getByRole("button", { name: "Send command" }) as HTMLButtonElement).disabled).toBe(true);
});

it("keeps a typed command the computer refused, so it can be corrected and sent again", async () => {
  result.mockRejectedValue(new Error("Voice control ended"));
  render(<VoiceSession sessionId="session" />);
  fireEvent.change(box(), { target: { value: "click sign in" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send command" })); });
  await screen.findByText("Voice control ended");
  expect(box().value).toBe("click sign in");
});
