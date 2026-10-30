// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import Voice from "./Voice";

const voice = vi.fn();
vi.mock("../../app/context", () => ({
  useApp: () => ({ session: { command: vi.fn() }, profile: { profileId: "phone", deviceId: "0192f7a1-0000-7000-8000-00000000d001", name: "studio" }, ui: { push: vi.fn() } }),
  // The computer reported this device's grants: it may watch but not control.
  useHost: <T,>(select: (s: HostState) => T) => select({ ...emptyHostState, grants: { view: true, control_input: false, admin: false, tasks: true, terminal: false, computer_use: false, approvals: false, library_read: true, library_write: false, transfers: false, accounts: false } }),
  useLink: () => ({ state: "online" }), describeError: () => "Unavailable",
}));
// crates/domain/src/grants.rs refuses Query::Voice without view and control_input;
// crates/services/src/hub.rs words that refusal as below.
vi.mock("./VoiceState", () => ({ useVoice: (...args: unknown[]) => { voice(...args); return { view: null, error: "This device or chat wasn’t given permission for that", reload: vi.fn() }; }, useVoiceVisible: () => true }));
vi.mock("../../shell/Screen", () => ({ Screen: ({ title, children }: { title: string; children: ReactNode }) => <main><h1>{title}</h1>{children}</main> }));
afterEach(cleanup);

it("tells a device without Control input why voice control is unavailable and the exact command, not a refusal", () => {
  render(<Voice route={{ screen: "voice" }} />);
  expect(screen.getByText("This device may not use voice control")).toBeTruthy();
  expect(screen.getByText("ophio devices allow 0192f7a1-0000-7000-8000-00000000d001 control", { exact: false })).toBeTruthy();
  expect(screen.queryByText(/given permission for that/)).toBeNull();
  expect(screen.queryByRole("button", { name: "Check again" })).toBeNull();
});
