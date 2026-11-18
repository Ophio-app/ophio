// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { capabilities, harnesses } from "../../dev/sim/scenario";
import { AgentUpdatesSim } from "../../dev/sim/agentUpdates";
import { emptyHostState, type HostState } from "../../state/projection";
import type { Command } from "../../protocol/types";
import { Toasts } from "../../design/Toast";
import { AgentUpdates } from "./AgentUpdates";
import { AgentModelUpdate } from "./AgentModelUpdate";
let sim: AgentUpdatesSim;
let state: HostState;
let online: boolean;
const push = vi.fn();
const query = vi.fn(async () => ({ kind: "agent_updates", view: structuredClone(sim.view) }));
const command = vi.fn(async (c: Command, o?: { expectedRevision?: number }) => {
  if (c.command === "set_agent_update_mode") sim.setMode(c.mode, o?.expectedRevision ?? null);
  if (c.command === "update_agent") sim.update(c.harness);
  if (c.command === "check_agent_updates") sim.check();
  return { status: "accepted" };
});
let session = { query, command };
vi.mock("../../app/context", () => ({
  useApp: () => ({ session, profile: { name: "Test computer" }, ui: { push } }),
  useHost: <T,>(select: (s: HostState) => T) => select(state),
  useLink: () => ({ state: online ? "online" : "offline" }),
  describeError: (e: { message?: string }) => e.message ?? "Request failed",
}));
beforeEach(() => {
  sim = new AgentUpdatesSim(); online = true;
  session = { query, command };
  state = { ...emptyHostState, capabilities: capabilities(), harnesses, grants: { view: true, tasks: true, admin: true, control_input: false, terminal: false, approvals: true, computer_use: false, library_read: true, library_write: false, transfers: false, accounts: false } };
  query.mockClear(); command.mockClear(); push.mockClear();
});
afterEach(cleanup);
const radio = (name: string | RegExp) => screen.getByRole("radio", { name }) as HTMLInputElement;
const ready = async () => waitFor(() => expect(radio(/Tell me when/).closest("fieldset")!.disabled).toBe(false));

it("offers one three-way setting, actual and latest versions, and keeps it during a one-shot update", async () => {
  render(<AgentUpdates />); await ready();
  expect(screen.getAllByRole("radio")).toHaveLength(3);
  expect(radio(/Tell me when/).checked).toBe(true);
  expect(screen.getByText(/2.4.2 is out/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Update Claude Code now" }));
  await waitFor(() => expect(screen.getAllByText(/up to date/)[0]).toBeTruthy());
  expect(command).toHaveBeenCalledWith({ command: "update_agent", harness: "claude_code" }, expect.anything());
  expect(sim.view.mode).toBe("notify");
  expect(command.mock.calls.some(([c]) => c.command === "set_agent_update_mode")).toBe(false);
});
it("changes instantly, quotes revisions, and restores the setting after a refusal", async () => {
  render(<><AgentUpdates /><Toasts /></>); await ready();
  fireEvent.click(radio(/Don’t check/));
  expect(radio(/Don’t check/).checked).toBe(true);
  await waitFor(() => expect(sim.view.mode).toBe("never"));
  expect(command).toHaveBeenLastCalledWith({ command: "set_agent_update_mode", mode: "never" }, expect.objectContaining({ expectedRevision: 1 }));
  let reject!: (e: Error) => void;
  command.mockImplementationOnce(() => new Promise((_, no) => { reject = no; }));
  fireEvent.click(radio(/Update automatically/));
  expect(radio(/Update automatically/).checked).toBe(true);
  await act(async () => reject(new Error("The computer refused.")));
  expect(radio(/Don’t check/).checked).toBe(true);
  expect(screen.getByText(/Couldn’t change the agent update setting/)).toBeTruthy();
});
it("lets Never check explicitly and update once without changing mode", async () => {
  sim.view.mode = "never";
  render(<AgentUpdates />); await ready();
  fireEvent.click(screen.getByRole("button", { name: "Check now" }));
  await waitFor(() => expect(screen.getByText("Check requested.")).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Update Claude Code now" }));
  await waitFor(() => expect(sim.view.agents[0]!.installed_version).toBe("2.4.2"));
  expect(sim.view.mode).toBe("never");
});
it("shows not checked yet rather than unsupported before the first saved check", async () => {
  sim.view.agents = sim.view.agents.map(a => ({ ...a, installed_version: null, latest_version: null, checked_at: null, update_supported: false, problem: null }));
  render(<AgentUpdates />); await ready();
  expect(screen.getAllByText(/not checked yet/)).toHaveLength(2);
  expect(screen.queryByText(/can’t update this agent/)).toBeNull();
  expect(screen.queryByText(/usual installer/)).toBeNull();
  expect(screen.queryByText(/up to date/)).toBeNull();
  expect((screen.getByRole("button", { name: "Check now" }) as HTMLButtonElement).disabled).toBe(false);
});
it.each(["This agent is pinned to a fixed version.", "This computer’s system cannot install this agent."])("keeps an explicit problem visible before a saved check: %s", async problem => {
  sim.view.agents = [{ ...sim.view.agents[0]!, latest_version: null, checked_at: null, update_supported: false, problem }];
  render(<AgentUpdates />); await ready();
  expect(screen.getByText(problem)).toBeTruthy();
  expect(screen.queryByText(/not checked yet/)).toBeNull();
  expect(screen.queryByText(/usual installer/)).toBeNull();
});
it.each(["2.4.0", "2.4.1", "2.4.2"])("labels cached latest %s as known rather than current after a failed check", async latest_version => {
  const problem = "The latest check failed because the update site is unreachable.";
  sim.view.agents = [{ ...sim.view.agents[0]!, installed_version: "2.4.1", latest_version, problem }];
  render(<AgentUpdates />); await ready();
  expect(screen.getByText(`· latest known ${latest_version}`)).toBeTruthy();
  expect(screen.getByText(problem)).toBeTruthy();
  expect(screen.queryByText(/up to date/)).toBeNull();
  expect(screen.queryByText(/is out/)).toBeNull();
});
it("keeps checking available on an unsupported installation platform", async () => {
  sim.view.supported = false;
  sim.view.agents = sim.view.agents.map(a => ({ ...a, update_supported: false, installed_version: null }));
  render(<AgentUpdates />); await ready();
  expect(screen.queryByRole("button", { name: /Update .* now/ })).toBeNull();
  expect((screen.getByRole("button", { name: "Check now" }) as HTMLButtonElement).disabled).toBe(false);
  expect(screen.getAllByText(/usual installer/)).toHaveLength(1);
  expect(screen.getByText("Claude Code · version unknown")).toBeTruthy();
});
it("disables changes and actions offline or without Manage access", async () => {
  state.grants = { ...state.grants!, admin: false };
  const first = render(<AgentUpdates />);
  await waitFor(() => expect(screen.getByText("Claude Code 2.4.1")).toBeTruthy());
  for (const button of screen.getAllByRole("button")) expect((button as HTMLButtonElement).disabled).toBe(true);
  expect(radio(/Tell me when/).closest("fieldset")!.disabled).toBe(true);
  first.unmount(); online = false;
  render(<AgentUpdates />);
  expect(screen.getByText("Offline. Connect to check or update agents.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Check now" }));
  expect(command).not.toHaveBeenCalled();
});
it("shows announced models only as a hint and opens update settings without changing the model", async () => {
  const leave = vi.fn();
  render(<AgentModelUpdate harness={harnesses[0]!} leave={leave} />);
  await waitFor(() => expect(screen.getByText(/Claude Fable 5.2 may need a newer Claude Code/)).toBeTruthy());
  expect(screen.queryByRole("button", { name: "Claude Fable 5.2" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Review agent update" }));
  expect(leave).toHaveBeenCalledOnce();
  expect(push).toHaveBeenCalledWith({ screen: "settings", section: "settings-agents" });
  expect(command).not.toHaveBeenCalled();
});
it("does not query an older host for model update hints", () => {
  state.capabilities = { ...capabilities(), features: ["agent_updates_v1"] };
  const { container } = render(<AgentModelUpdate harness={harnesses[0]!} />);
  expect(container.textContent).toBe(""); expect(query).not.toHaveBeenCalled();
});
it("does not suggest updates for models already available or an unknown installed version", async () => {
  sim.view.agents[0]!.released_models = harnesses[0]!.models.map(m => m.id);
  const first = render(<AgentModelUpdate harness={harnesses[0]!} />);
  await waitFor(() => expect(query).toHaveBeenCalled());
  expect(screen.queryByText(/may need a newer/)).toBeNull(); first.unmount();
  sim.view.agents[0]!.installed_version = null;
  sim.view.agents[0]!.released_models = ["claude-fable-5-2"];
  render(<AgentModelUpdate harness={harnesses[0]!} />);
  await waitFor(() => expect(query).toHaveBeenCalledTimes(2));
  expect(screen.queryByText(/may need a newer/)).toBeNull();
});
