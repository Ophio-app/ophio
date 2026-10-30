// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import type { TerminalEvent, TerminalLink } from "../../host/link";
import { RouteProvider } from "../../shell/Screen";
import { terminalTitle } from "./terminalTitle";
import Terminal from "./Terminal";

/** The last terminal mounted, its scroll position, how often it was taken to the latest row and the rows it was scrolled by. */
const xterm = vi.hoisted(() => ({ buffer: { active: { viewportY: 0, baseY: 0 } }, toLatest: 0, scrolled: () => {}, lines: [] as number[] }));
vi.mock("@xterm/xterm", () => ({ Terminal: class {
  options = {}; cols = 80; rows = 24;
  buffer = xterm.buffer;
  scrollToBottom() { xterm.toLatest++; xterm.buffer.active.viewportY = xterm.buffer.active.baseY; xterm.scrolled(); }
  scrollLines(lines: number) { xterm.lines.push(lines); xterm.buffer.active.viewportY += lines; xterm.scrolled(); }
  element?: HTMLElement;
  parser = { registerOscHandler: () => ({ dispose() {} }) };
  textarea = document.createElement("textarea");
  loadAddon() {} open(host: HTMLElement) {
    this.textarea.setAttribute("aria-label", "Terminal input"); host.append(this.textarea);
    // 24 rows of 20px.
    this.element = document.createElement("div");
    const rows = this.element.appendChild(document.createElement("div"));
    rows.className = "xterm-screen";
    Object.defineProperty(rows, "offsetHeight", { value: 480 });
  } dispose() { this.textarea.remove(); }
  write(_data: Uint8Array, done?: () => void) { done?.(); }
  onData() { return { dispose() {} }; }
  onSelectionChange() { return { dispose() {} }; }
  onScroll(listener: () => void) { xterm.scrolled = listener; return { dispose() {} }; }
} }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));
let app: App;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  Object.assign(xterm, { buffer: { active: { viewportY: 0, baseY: 0 } }, toLatest: 0, scrolled: () => {}, lines: [] });
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
function mount(id: string) {
  const route = { screen: "terminal" as const, terminalId: id };
  app.ui.selectTab("control"); app.ui.push(route);
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "control", route, depth: 1, active: true }}><Terminal route={route} /></RouteProvider></AppProvider>);
}
it("removes a refused terminal and opens a shell in its remembered project and folder", async () => {
  const previous = window.__sim!.host.terminals[0]!;
  app.session.state.set(s => ({ ...s, terminals: [previous] }));
  window.__sim!.host.terminals = [];
  mount(previous.terminal_id); await advance(200);
  expect(screen.getByText(/This terminal is no longer on studio/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Reconnect terminal" })).toBeNull();
  expect(app.session.state.get().terminals.some(t => t.terminal_id === previous.terminal_id)).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Open a new terminal" }));
  expect(screen.getByRole("button", { name: "Opening a terminal…" }).getAttribute("aria-busy")).toBe("true");
  await advance(700);
  expect(window.__sim!.host.terminals[0]).toMatchObject({ project_id: previous.project_id, cwd: previous.cwd, harness: null });
  expect(app.ui.route()).toMatchObject({ screen: "terminal", terminalId: window.__sim!.host.terminals[0]!.terminal_id });
});
it("shows legacy offline reasons beside Reconnect, progress, then the new outcome", async () => {
  let listener: (event: TerminalEvent) => void = () => {};
  const link: TerminalLink = { send: () => false, input: () => false, close() {}, listen(l) { listener = l; return () => {}; } };
  vi.spyOn(app.platform, "terminal").mockReturnValue(link);
  mount(window.__sim!.host.terminals[0]!.terminal_id);
  act(() => listener({ type: "state", state: { state: "offline", since: 0, reason: "Terminal access was withdrawn. Review grants on studio.", retryAt: null } }));
  expect(screen.getByText(/Terminal access was withdrawn/)).toBeTruthy();
  const button = screen.getByRole("button", { name: "Reconnect terminal" });
  expect(button.closest(".terminal-recovery")).toBeTruthy();
  fireEvent.click(button);
  expect((screen.getByRole("button", { name: "Reconnecting…" }) as HTMLButtonElement).disabled).toBe(true);
  act(() => listener({ type: "state", state: { state: "failed", reason: "revoked", detail: "This phone was removed. Pair again." } }));
  expect(screen.getByText("This phone was removed. Pair again.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Reconnect terminal" })).toBeTruthy();
});

it("keeps command focus and the command draft ready for consecutive Send line actions", async () => {
  const id = window.__sim!.host.terminals[0]!.terminal_id;
  mount(id); await advance(500);
  const field = screen.getByRole("textbox", { name: /Command for/ });
  act(() => field.focus());
  fireEvent.change(field, { target: { value: "pwd" } });
  fireEvent.click(screen.getByRole("button", { name: "Send line" }));
  expect(document.activeElement).toBe(field);
  expect((field as HTMLTextAreaElement).value).toBe("");
  fireEvent.change(field, { target: { value: "ls" } });
  fireEvent.click(screen.getByRole("button", { name: "Send line" }));
  expect(document.activeElement).toBe(field);
});

it("gives a terminal cramped for its text size the compact layout before anything is typed", async () => {
  const size = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };
  Object.defineProperties(window, { innerWidth: { value: 844, configurable: true }, innerHeight: { value: 250, configurable: true } });
  try {
    const { container } = mount(window.__sim!.host.terminals[0]!.terminal_id);
    act(() => { window.dispatchEvent(new Event("resize")); });
    await advance(500);
    expect(container.querySelector(".terminal-screen--compact")).toBeTruthy();
    expect(container.querySelector(".terminal-screen--typing")).toBeNull();
    expect(screen.queryByText("Attached")).toBeNull();
    expect(screen.getByRole("radio", { name: "Text" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Detach" })).toBeTruthy();
  } finally {
    Object.defineProperties(window, { innerWidth: { value: size.innerWidth, configurable: true }, innerHeight: { value: size.innerHeight, configurable: true } });
    act(() => { window.dispatchEvent(new Event("resize")); });
  }
});
it("on a phone on its side gives the output the height, keeps the session's controls in the bar, and reads the history by swiping", async () => {
  const size = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };
  Object.defineProperties(window, { innerWidth: { value: 844, configurable: true }, innerHeight: { value: 390, configurable: true } });
  try {
    const { container } = mount(window.__sim!.host.terminals[0]!.terminal_id);
    act(() => { window.dispatchEvent(new Event("resize")); });
    await advance(500);
    expect(container.querySelector(".terminal-screen--compact")).toBeTruthy();
    expect(container.querySelector(".terminal-screen--typing")).toBeNull();
    const bar = container.querySelector<HTMLElement>(".bar")!;
    expect(within(bar).getByRole("button", { name: "Detach" })).toBeTruthy();
    expect(within(bar).getByRole("button", { name: "Close terminal" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Detach" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Close terminal" })).toHaveLength(1);
    fireEvent.click(screen.getByRole("radio", { name: "Keys" }));
    expect(within(container.querySelector<HTMLElement>(".terminal-keys")!).getByRole("button", { name: "Show keyboard" }).textContent).toBe("");
    // With the keyboard open, the bar beside the keys offers Hide keyboard once.
    act(() => { screen.getByLabelText("Terminal input").focus(); });
    Object.defineProperty(window, "innerHeight", { value: 220, configurable: true });
    act(() => { window.dispatchEvent(new Event("resize")); });
    await advance(100);
    expect(document.documentElement.classList.contains("kb-open")).toBe(true);
    expect(screen.getAllByRole("button", { name: "Hide keyboard" })).toHaveLength(1);
    act(() => { screen.getByLabelText("Terminal input").blur(); });
    Object.defineProperty(window, "innerHeight", { value: 390, configurable: true });
    act(() => { window.dispatchEvent(new Event("resize")); });
    await advance(100);

    // 100 rows above the view; a finger moving down 60px shows three earlier ones.
    Object.assign(xterm.buffer.active, { viewportY: 100, baseY: 100 });
    const output = container.querySelector<HTMLElement>(".terminal-output")!;
    const touch = (type: string, y: number) => {
      const e = new Event(type, { bubbles: true, cancelable: true });
      Object.defineProperty(e, "touches", { value: type === "touchend" ? [] : [{ clientX: 100, clientY: y }] });
      act(() => { output.dispatchEvent(e); });
      return e;
    };
    touch("touchstart", 200);
    const moves = [220, 240, 260].map(y => touch("touchmove", y));
    touch("touchend", 260);
    expect(moves.every(e => e.defaultPrevented)).toBe(true);
    expect(xterm.lines.reduce((a, b) => a + b, 0)).toBe(-3);
    expect(screen.getByRole("button", { name: "Jump to latest" })).toBeTruthy();
  } finally {
    Object.defineProperties(window, { innerWidth: { value: size.innerWidth, configurable: true }, innerHeight: { value: size.innerHeight, configurable: true } });
    act(() => { window.dispatchEvent(new Event("resize")); });
  }
});
it("keeps status and ended actions in the content column", async () => {
  const previous = window.__sim!.host.terminals[0]!;
  app.session.state.set(s => ({ ...s, terminals: [previous] }));
  window.__sim!.host.terminals = [];
  mount(previous.terminal_id); await advance(200);
  expect(screen.getByText("Terminal ended").closest(".screen__inner")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Open a new terminal" }).closest(".screen__inner")).toBeTruthy();
});
it("leaves main-link outages to the disconnected notice and translates raw side-link failures", () => {
  let listener: (event: TerminalEvent) => void = () => {};
  const link: TerminalLink = { send: () => false, input: () => false, close() {}, listen(l) { listener = l; return () => {}; } };
  vi.spyOn(app.platform, "terminal").mockReturnValue(link);
  mount(window.__sim!.host.terminals[0]!.terminal_id);
  act(() => listener({ type: "state", state: { state: "offline", since: 0, reason: "connection failed", retryAt: null } }));
  expect(screen.getByText("The terminal connection was interrupted. Reconnect to try again.")).toBeTruthy();
  act(() => app.session.link.set({ state: "offline", since: 0, reason: "connection failed", retryAt: null }));
  expect(screen.queryByText("connection failed")).toBeNull();
  expect(screen.queryByText("Connection interrupted")).toBeNull();
  expect(screen.queryByText("The terminal connection was interrupted. Reconnect to try again.")).toBeNull();
  expect(screen.getByText(/Computer disconnected/)).toBeTruthy();
});

it("an exited terminal opens its recorded output without input controls", async () => {
  const host = window.__sim!.host;
  const ended = { ...host.terminals[0]!, state: { state: "exited" as const, code: 0 } };
  host.terminals = [ended];
  app.session.state.set(s => ({ ...s, terminals: [ended] }));
  mount(ended.terminal_id); await advance(500);
  expect(screen.queryByRole("textbox", { name: /Command for/ })).toBeNull();
  expect(screen.queryByRole("button", { name: "Send line" })).toBeNull();
  expect(screen.queryByRole("textbox", { name: "Terminal input" })).toBeNull();
  expect(document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Terminal input"]')?.tabIndex).toBe(-1);
  expect(screen.queryByText("Text mode")).toBeNull();
  expect(screen.queryByText("Key mode")).toBeNull();
  expect(screen.getByText(/Session ended/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Close terminal" })).toBeNull();
});

it.each([0, null])("keeps Session ended after a late send failure (code %s)", async (code) => {
  const terminal = window.__sim!.host.terminals[0]!;
  let listener: (event: TerminalEvent) => void = () => {};
  const send = vi.fn(() => true), input = vi.fn(() => true), close = vi.fn();
  const link: TerminalLink = { send, input, close, listen(l) { listener = l; return () => {}; } };
  vi.spyOn(app.platform, "terminal").mockReturnValue(link);
  const view = mount(terminal.terminal_id);
  act(() => listener({ type: "text", frame: { type: "attached", terminal, replay_from: 0, replay_truncated: false } }));
  act(() => {
    listener({ type: "text", frame: { type: "exited", code } });
    // Mirrors native link_send(false) resolving after the exit frame.
    listener({ type: "state", state: { state: "offline", since: 0, reason: "The connection could not send this frame.", retryAt: null } });
    listener({ type: "text", frame: { type: "error", message: "Late frame failed" } });
  });
  const sendsAtExit = send.mock.calls.length;
  expect(screen.getByText(/Session ended/)).toBeTruthy();
  expect(screen.getByText(code === null ? "This terminal has exited. Its exit code is unknown." : "This terminal exited with code 0.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Reconnect terminal" })).toBeNull();
  expect(screen.queryByText(/could not send this frame/)).toBeNull();
  expect(screen.queryByRole("textbox", { name: /Command for/ })).toBeNull();
  expect(app.session.state.get().terminals.find(t => t.terminal_id === terminal.terminal_id)?.state).toEqual({ state: "exited", code });
  expect(close).toHaveBeenCalled();
  await advance(11_000);
  view.unmount();
  expect(send).toHaveBeenCalledTimes(sendsAtExit);
  expect(input).not.toHaveBeenCalled();
});

it("Send line exit ends the simulated session and keeps its output view", async () => {
  mount(window.__sim!.host.terminals[0]!.terminal_id); await advance(500);
  fireEvent.change(screen.getByRole("textbox", { name: /Command for/ }), { target: { value: "exit 7" } });
  fireEvent.click(screen.getByRole("button", { name: "Send line" }));
  await advance(500);
  expect(screen.getByText(/Session ended/)).toBeTruthy();
  expect(screen.getByText("This terminal exited with code 7.")).toBeTruthy();
  expect(screen.queryByRole("textbox", { name: "Terminal input" })).toBeNull();
  expect(document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Terminal input"]')?.disabled).toBe(true);
  expect(screen.queryByRole("button", { name: "Reconnect terminal" })).toBeNull();
  expect(document.querySelector(".terminal-output")).toBeTruthy();
});

it("ended terminals have no Detach or running-programs promise", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const info = window.__sim!.host.terminals[0]!;
  mount(info.terminal_id); await advance(500);
  fireEvent.click(screen.getByRole("button", { name: "Close terminal" }));
  expect(screen.getByRole("button", { name: "Keep terminal" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: `Close ‘${info.title}’` }));
  await advance(700);
  expect(screen.getByText(/Closed by you ·/)).toBeTruthy();
  expect(screen.queryByText(/This terminal exited with code 0/)).toBeNull();
  expect(app.ui.route()).toEqual({ screen: "control" });
  expect(screen.queryByRole("button", { name: "Detach" })).toBeNull();
  expect(screen.queryByText(/Detach leaves programs running/)).toBeNull();
});
it("duplicate titles use the same session number in the inventory and header", async () => {
  const first = window.__sim!.host.terminals[0]!;
  const second = { ...first, terminal_id: "second-shell", created_at: first.created_at + 1000 };
  window.__sim!.host.terminals.push(second);
  app.session.state.set(s => ({ ...s, terminals: [second, first] }));
  expect(terminalTitle(first, [second, first])).toBe(first.title);
  const label = `${first.title} (2)`;
  expect(terminalTitle(second, [second, first])).toBe(label);
  mount(second.terminal_id); await advance(500);
  expect(screen.getByRole("heading", { name: label })).toBeTruthy();
});

it("an admitted Close does not claim a user close or leave before confirmation", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const info = window.__sim!.host.terminals[0]!;
  mount(info.terminal_id); await advance(500);
  vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "pending-close", status: "accepted", seq: null, result: null, error: null });
  fireEvent.click(screen.getByRole("button", { name: "Close terminal" }));
  fireEvent.click(screen.getByRole("button", { name: `Close ‘${info.title}’` }));
  await advance(700);
  expect(app.ui.route()).toEqual({ screen: "terminal", terminalId: info.terminal_id });
  expect(screen.queryByText(/Closed by you/)).toBeNull();
  act(() => app.session.state.set(s => ({ ...s, commandStages: { ...s.commandStages,
    "pending-close": { stage: "observed", at: Date.now(), detail: null } } })));
  await advance(100);
  expect(app.ui.route()).toEqual({ screen: "control" });
});

it("the output follows new rows unless the person scrolled back to read, and Jump to latest returns to them", async () => {
  mount(window.__sim!.host.terminals[0]!.terminal_id); await advance(500);
  const [link] = window.__sim!.host.controlSim.terminalLinks;
  const print = async (text: string) => { link!.input(new TextEncoder().encode(`echo ${text}\r`)); await advance(300); };
  const rows = xterm.buffer.active;
  rows.baseY = rows.viewportY = 40;
  let before = xterm.toLatest;
  await print("at the latest row");
  expect(xterm.toLatest).toBeGreaterThan(before);

  rows.viewportY = 10;
  act(() => xterm.scrolled());
  before = xterm.toLatest;
  await print("while reading back");
  expect(xterm.toLatest).toBe(before);
  expect(rows.viewportY).toBe(10);

  act(() => fireEvent.click(screen.getByRole("button", { name: "Jump to latest" })));
  expect(rows.viewportY).toBe(rows.baseY);
  expect(screen.queryByRole("button", { name: "Jump to latest" })).toBeNull();
  before = xterm.toLatest;
  await print("following again");
  expect(xterm.toLatest).toBeGreaterThan(before);
});
