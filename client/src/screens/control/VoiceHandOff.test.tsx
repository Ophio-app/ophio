// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import type { Grants, VoiceSessionView } from "../../protocol/types";
import { capabilities, harnesses, projects, targets } from "../../dev/sim/scenario";
import { VoiceEnded, VoicePrompt } from "./VoiceSession";
import { handOffs } from "./VoiceHandOff";
import { goalKey, restorePreviousGoal } from "./computerReview";
import { CommandError, OutcomeUnknownError } from "../../host/session";

const command = vi.fn();
const push = vi.fn();
const result = vi.fn();
const refreshHarnesses = vi.fn().mockResolvedValue(undefined);
const query = vi.fn();
let grants: Grants;
let computerUse = true;
const project = projects(Date.now())[0]!;
const display = targets()[0]!;
vi.mock("../../app/context", () => ({
  useApp: () => ({ session: { command, refreshHarnesses, query }, profile: { profileId: "phone", name: "Test computer" }, ui: { push } }),
  useHost: <T,>(select: (s: HostState) => T) => select({
    ...emptyHostState, grants, harnesses,
    capabilities: { ...capabilities(), computer_use: computerUse ? { state: "available" } : { state: "unsupported", reason: "Install computer-mcp beside Øphio" } },
    projects: { [project.project_id]: project }, targets: [display],
  }),
  useUi: <T,>(select: (s: { projectId: string | null }) => T) => select({ projectId: project.project_id }),
  useLink: () => ({ state: "online" }), describeError: (e: Error) => e.message,
}));
vi.mock("../../host/commandResult", () => ({ commandResult: () => result() }));
const allowed: Grants = { view: true, control_input: true, admin: false, tasks: true, terminal: false, computer_use: true, approvals: false, library_read: false, library_write: false, transfers: false, accounts: false };
function offered(): VoiceSessionView {
  return { session_id: "voice", target: "desktop", target_id: display.target_id, lease: { lease_id: "lease", generation: 1, boot_epoch: "boot", target_id: display.target_id, owner: { kind: "human", device_id: "phone" }, state: "active", granted_at: 1, renewal_window_ms: 3000 }, dry_run: false, phase: "refused", transcript: "Open Firefox and then search for cats.", understood: "This needs several steps. Say one step at a time, such as “open the browser”", prompt_id: 4, choices: [], steps: [], ended: null, picture: null, offer: { instruction: "Open Firefox and then search for cats." } };
}
beforeEach(() => {
  grants = { ...allowed }; computerUse = true; localStorage.clear(); handOffs.set(() => ({}));
  command.mockReset().mockResolvedValue({ status: "accepted" }); push.mockReset(); result.mockReset(); query.mockReset();
});
afterEach(cleanup);

it("shows the exact words, the agent and the display, and starts nothing until tapped", () => {
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  expect(screen.getByLabelText("Instruction for the agent").textContent).toBe("Open Firefox and then search for cats.");
  const agent = harnesses.find((h) => h.kind === "claude_code")!;
  expect(screen.getByText(new RegExp(`^${agent.display_name}`))).toBeTruthy();
  expect(screen.getByText(display.label)).toBeTruthy();
  expect(command).not.toHaveBeenCalled();
});

it("one tap stops voice control, then starts computer use on that display and opens it", async () => {
  result.mockResolvedValueOnce(undefined).mockResolvedValueOnce({ kind: "task_created", task_id: "task-1" });
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await waitFor(() => expect(push).toHaveBeenCalledWith({ screen: "computer_use", taskId: "task-1" }));
  expect(command.mock.calls[0]?.[0]).toEqual({ command: "voice", operation: { operation: "stop", session_id: "voice" } });
  expect(command.mock.calls[1]?.[0]).toMatchObject({
    command: "create_task", kind: "computer_use", mode: "work", project_id: project.project_id,
    prompt: "Open Firefox and then search for cats.", target_id: display.target_id, placement: { mode: "start_now" },
  });
});

it("says so, and keeps the plain refusal, when this device or the computer can't start computer use", () => {
  grants = { ...allowed, computer_use: false };
  const { unmount } = render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  expect(screen.getByText("This device doesn’t have permission to start computer use.")).toBeTruthy();
  expect((screen.getByRole("button", { name: /^Hand to / }) as HTMLButtonElement).disabled).toBe(true);
  unmount();
  grants = { ...allowed }; computerUse = false;
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  expect(screen.getByText(/^Computer use is unavailable on the computer/)).toBeTruthy();
  expect(screen.getByText(/This needs several steps/)).toBeTruthy();
});

it("a failed start after voice stopped says where to start it instead", async () => {
  result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("Agent signed out."));
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await screen.findByText(/^Voice control stopped, but the agent didn’t start\. Agent signed out\./);
  expect(push).not.toHaveBeenCalled();
});

const ended = (): VoiceSessionView => ({ ...offered(), phase: "stopped", ended: "stop", offer: undefined, understood: "Voice control ended" });

it("shows and sends the same words: the instruction without stray spaces", async () => {
  const value = { ...offered(), offer: { instruction: "  Open Firefox and then search for cats.  " } };
  result.mockResolvedValueOnce(undefined).mockResolvedValueOnce({ kind: "task_created", task_id: "task-1" });
  render(<VoicePrompt value={value} disabled={false} send={vi.fn()} />);
  expect(screen.getByLabelText("Instruction for the agent").textContent).toBe("Open Firefox and then search for cats.");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await waitFor(() => expect(push).toHaveBeenCalled());
  expect(command.mock.calls[1]?.[0]).toMatchObject({ prompt: "Open Firefox and then search for cats." });
});

it("keeps showing the hand-off after voice control has ended, until the start settles", async () => {
  let fail: (e: Error) => void = () => {};
  result.mockResolvedValueOnce(undefined).mockReturnValueOnce(new Promise((_, reject) => { fail = reject; }));
  const view = render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  // Stopping voice ends its view; the phone now shows the ended session.
  view.rerender(<VoiceEnded value={ended()} />);
  expect(screen.getByLabelText("Instruction for the agent").textContent).toBe("Open Firefox and then search for cats.");
  await act(async () => { fail(new Error("Agent signed out.")); });
  expect(await screen.findByText(/^Voice control stopped, but the agent didn’t start\. Agent signed out\./)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Start computer use" })).toBeTruthy();
});

it("an unanswered start is checked under its own identity, never started again", async () => {
  result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await screen.findByText(/hasn’t confirmed the agent started/);
  expect(screen.queryByRole("button", { name: /^Hand to / })).toBeNull();
  // Start computer use holds the very request that was sent, to check it.
  const sent = command.mock.calls[1]?.[1] as { commandId: string };
  const draft = JSON.parse(localStorage.getItem(goalKey("phone", project.project_id)) ?? "null") as { goal: string; attempt: { id: string; body: { prompt: string } } };
  expect(draft.attempt.id).toBe(sent.commandId);
  expect(draft.attempt.body.prompt).toBe(draft.goal);
  // Checking asks the computer about that request; one it never received
  // can be sent again, under the same identity, and only then.
  query.mockResolvedValueOnce({ receipt: null });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Check the start" })); });
  expect(query).toHaveBeenCalledWith({ kind: "command_status", command_id: sent.commandId });
  expect(command).toHaveBeenCalledTimes(2);
  result.mockResolvedValueOnce({ kind: "task_created", task_id: "task-2" });
  await act(async () => { fireEvent.click(await screen.findByRole("button", { name: "Send the same request again" })); });
  await waitFor(() => expect(push).toHaveBeenCalledWith({ screen: "computer_use", taskId: "task-2" }));
  expect((command.mock.calls[2]?.[1] as { commandId: string }).commandId).toBe(sent.commandId);
});

it("checks an unanswered start under its own identity even when this phone can't save it", async () => {
  const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new DOMException("full", "QuotaExceededError"); });
  try {
    result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
    render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
    await screen.findByText(/hasn’t confirmed the agent started/);
    const sent = command.mock.calls[1]?.[1] as { commandId: string };
    query.mockResolvedValueOnce({ receipt: { command_id: sent.commandId, status: "accepted" } });
    result.mockResolvedValueOnce({ kind: "task_created", task_id: "task-9" });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Check the start" })); });
    await waitFor(() => expect(push).toHaveBeenCalledWith({ screen: "computer_use", taskId: "task-9" }));
    expect(query).toHaveBeenCalledWith({ kind: "command_status", command_id: sent.commandId });
    expect(command).toHaveBeenCalledTimes(2);
  } finally { setItem.mockRestore(); }
});

it("a checked start that definitely failed says so and offers a way on", async () => {
  result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await screen.findByText(/hasn’t confirmed the agent started/);
  const sent = command.mock.calls[1]?.[1] as { commandId: string };
  query.mockResolvedValueOnce({ receipt: { command_id: sent.commandId, status: "accepted" } });
  result.mockRejectedValueOnce(new CommandError({ code: "permission_denied", message: "Agent signed out." } as never, null));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Check the start" })); });
  expect(await screen.findByText(/^Voice control stopped, but the agent didn’t start\. Agent signed out\./)).toBeTruthy();
  expect(screen.queryByText(/hasn’t confirmed the agent started/)).toBeNull();
  expect(screen.getByRole("button", { name: "Start computer use" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Dismiss" })).toBeTruthy();
});

it("a refused status question keeps the start unknown and its request", async () => {
  result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await screen.findByText(/hasn’t confirmed the agent started/);
  const sent = command.mock.calls[1]?.[1] as { commandId: string };
  // The computer refuses the question itself, not the start.
  query.mockRejectedValueOnce(new CommandError({ code: "permission_denied", message: "Access to this command's status is not permitted" } as never, null));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Check the start" })); });
  expect(await screen.findByText(/hasn’t confirmed the agent started/)).toBeTruthy();
  expect(screen.queryByText(/didn’t start/)).toBeNull();
  expect(screen.queryByRole("button", { name: "Start computer use" })).toBeNull();
  const draft = JSON.parse(localStorage.getItem(goalKey("phone", project.project_id)) ?? "null") as { attempt: { id: string } };
  expect(draft.attempt.id).toBe(sent.commandId);
  // Checking again asks about the same request.
  query.mockResolvedValueOnce({ receipt: null });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Check the start" })); });
  expect(query).toHaveBeenLastCalledWith({ kind: "command_status", command_id: sent.commandId });
});

it("an unknown start can be dismissed without sending anything or forgetting its request", async () => {
  const key = goalKey("phone", project.project_id);
  const earlier = JSON.stringify({ goal: "My earlier goal", target: "", harness: "", model: "" });
  localStorage.setItem(key, earlier);
  result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await screen.findByText(/hasn’t confirmed the agent started/);
  const sent = command.mock.calls[1]?.[1] as { commandId: string };
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByText(/hasn’t confirmed the agent started/)).toBeNull();
  expect(command).toHaveBeenCalledTimes(2);
  expect(query).not.toHaveBeenCalled();
  // Start computer use can check the same request, then recover the earlier draft.
  const draft = JSON.parse(localStorage.getItem(key) ?? "null") as { attempt: { id: string }; before: string };
  expect(draft.attempt.id).toBe(sent.commandId);
  expect(draft.before).toBe(earlier);
  expect(restorePreviousGoal(key, draft.attempt.id, draft.before)).toBe(true);
  expect(localStorage.getItem(key)).toBe(earlier);
});

it("Dismiss does not overwrite a newer goal or forget an unresolved request", async () => {
  const key = goalKey("phone", project.project_id);
  result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
  render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
  await screen.findByText(/hasn’t confirmed the agent started/);
  const request = JSON.parse(localStorage.getItem(key) ?? "null") as { attempt: { id: string }; before?: string };
  const newer = JSON.stringify({ goal: "A new goal", target: "", harness: "", model: "" });
  localStorage.setItem(key, newer);
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(localStorage.getItem(key)).toBe(newer);
  expect(screen.getByText(/hand-off stays here so its identity isn’t lost/)).toBeTruthy();
  expect(restorePreviousGoal(key, request.attempt.id, request.before)).toBe(false);
  expect(localStorage.getItem(key)).toBe(newer);
  expect(command).toHaveBeenCalledTimes(2);
});

it("an unreadable earlier goal is not overwritten by the hand-off", async () => {
  const key = goalKey("phone", project.project_id);
  const earlier = JSON.stringify({ goal: "Keep my goal", target: "", harness: "", model: "" });
  localStorage.setItem(key, earlier);
  const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new DOMException("blocked", "SecurityError"); });
  try {
    result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
    render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
    await screen.findByText(/hasn’t confirmed the agent started/);
  } finally { getItem.mockRestore(); }
  expect(localStorage.getItem(key)).toBe(earlier);
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(localStorage.getItem(key)).toBe(earlier);
  expect(screen.getByText(/hand-off stays here so its identity isn’t lost/)).toBeTruthy();
});

it("Dismiss keeps an unknown request open when it could not be saved", async () => {
  const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new DOMException("full", "QuotaExceededError"); });
  try {
    result.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new OutcomeUnknownError("lost"));
    render(<VoicePrompt value={offered()} disabled={false} send={vi.fn()} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^Hand to / })); });
    await screen.findByText(/hasn’t confirmed the agent started/);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.getByText(/hand-off stays here so its identity isn’t lost/)).toBeTruthy();
    expect(command).toHaveBeenCalledTimes(2);
  } finally { setItem.mockRestore(); }
});
