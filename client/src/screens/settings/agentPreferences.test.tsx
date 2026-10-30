// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { capabilities, harnesses } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import type { AgentPreferences as Preferences } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import { AGENT_PREFERENCES_FEATURE, AgentPreferences } from "./AgentPreferences";

let kept: Preferences;
let state: HostState;
const query = vi.fn(async () => ({ kind: "agent_preferences", view: { ...kept } }));
const command = vi.fn(async (c: { questions: boolean }, o: { expectedRevision: number }) => {
  if (o.expectedRevision !== kept.revision) throw new Error("stale");
  kept = { questions: c.questions, revision: kept.revision + 1 };
  return { status: "accepted" };
});
const session = { query, command };
vi.mock("../../app/context", () => ({
  useApp: () => ({ session, profile: { name: "Test computer" } }),
  useHost: <T,>(select: (s: HostState) => T) => select(state),
  useLink: () => ({ state: "online" }),
  describeError: (e: { message?: string }) => e.message ?? "Request failed",
}));
const grants = { view: true, tasks: true, admin: true, control_input: false, terminal: false, approvals: true, computer_use: false, library_read: true, library_write: false, transfers: false, accounts: false };
beforeEach(() => {
  kept = { questions: true, revision: 0 };
  state = { ...emptyHostState, capabilities: capabilities(), harnesses, grants };
  query.mockClear(); command.mockClear();
});
afterEach(cleanup);

const toggle = () => screen.getByRole("checkbox", { name: "Agents can ask me questions" }) as HTMLInputElement;

it("turns questions off and on, with the revision it was shown", async () => {
  render(<AgentPreferences />);
  await waitFor(() => expect(toggle().disabled).toBe(false));
  expect(toggle().checked).toBe(true);
  fireEvent.click(toggle());
  await waitFor(() => expect(toggle().checked).toBe(false));
  expect(command).toHaveBeenLastCalledWith({ command: "set_agent_preferences", questions: false }, expect.objectContaining({ expectedRevision: 0 }));
  expect(screen.getByText(/make the best choice they can and say what they chose/)).toBeTruthy();
  fireEvent.click(toggle());
  await waitFor(() => expect(command).toHaveBeenLastCalledWith({ command: "set_agent_preferences", questions: true }, expect.objectContaining({ expectedRevision: 1 })));
  expect(toggle().checked).toBe(true);
});

it("shows the choice on the tap and stays usable while the computer answers; a refusal puts it back", async () => {
  render(<><AgentPreferences /><Toasts /></>);
  await waitFor(() => expect(toggle().disabled).toBe(false));
  let refuse!: (e: Error) => void;
  command.mockImplementationOnce(() => new Promise((_, reject) => { refuse = reject; }));
  fireEvent.click(toggle());
  expect(toggle().checked).toBe(false);
  expect(toggle().disabled).toBe(false);
  expect(screen.getByText(/make the best choice they can/)).toBeTruthy();
  await act(async () => { refuse(new Error("It changed on the computer.")); });
  expect(toggle().checked).toBe(true);
  expect(screen.getByText("Couldn’t change whether agents ask you questions. It changed on the computer.")).toBeTruthy();
});

it("isn't offered by a computer that can't tell its agents", () => {
  const { features, ...rest } = capabilities();
  state = { ...state, capabilities: { ...rest, features: features.filter((f) => f !== AGENT_PREFERENCES_FEATURE) } };
  const { container } = render(<AgentPreferences />);
  expect(container.textContent).toBe("");
  expect(query).not.toHaveBeenCalled();
});
