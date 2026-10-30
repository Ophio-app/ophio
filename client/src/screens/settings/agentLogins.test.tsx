// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession, withoutSecret } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { LoginUseNotices } from "../../shell/LoginUseNotices";
import { Toasts } from "../../design/Toast";
import { signInCode } from "../../design/ApprovalCard";
import { applyEvent, emptyHostState } from "../../state/projection";
import type { LoginDetails } from "../../protocol/types";
import { AttentionList } from "../work/shared";
import AgentLogins from "./AgentLogins";
import { generatePassword, SHOW_MS, splitSites } from "./useAgentLogins";

let app: App;
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const route = { screen: "agent_logins" } as const;
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>
      {children}
      <Toasts />
    </RouteProvider>
  </AppProvider>
);
const sim = () => window.__sim!.host.agentLogins;
const sheet = () => within(screen.getByRole("dialog"));
const confirm = () => vi.spyOn(app.platform.appLock!, "confirm");

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = {
    platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => { await Promise.resolve(); });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("opens on the disclaimer, which turns Agent logins on only once ticked and unlocked", async () => {
  const unlock = confirm();
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  expect(sheet().getByRole("heading", { name: "Before you give agents logins" })).toBeTruthy();
  expect(sheet().getByText(/doesn't send your passwords to the AI model/).tagName).toBe("LI");
  const turnOn = sheet().getByRole("button", { name: "Turn on Agent logins" }) as HTMLButtonElement;
  expect(turnOn.disabled).toBe(true);
  fireEvent.click(sheet().getByRole("checkbox", { name: /agents act with my logins/ }));
  expect(turnOn.disabled).toBe(false);
  fireEvent.click(turnOn);
  await advance(600);
  expect(unlock).toHaveBeenCalledWith("Turn on Agent logins");
  expect(sim().view.enabled).toBe(true);
  expect(sim().view.history[0]?.event).toBe("accepted");
  expect(screen.getByText("Agent logins turned on")).toBeTruthy();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByRole("button", { name: "Add a login" })).toBeTruthy();
});

it("Not now leaves it off, says so, and adds nothing", async () => {
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(sheet().getByRole("button", { name: "Not now" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByRole("heading", { name: /Agent logins is off on/ })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Add a login" })).toBeNull();
  expect(screen.getByText(sim().view.reminder)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Read and turn on" }));
  expect(sheet().getByRole("button", { name: "Turn on Agent logins" })).toBeTruthy();
});

it("adds a login, sending its password once and keeping none of it on the phone", async () => {
  sim().seed();
  const unlock = confirm();
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: "Add a login" }));
  const form = sheet();
  fireEvent.click(form.getByRole("button", { name: "Add login" }));
  await advance(50);
  expect(form.getByRole("alert").textContent).toBe("Give the login a name.");
  fireEvent.change(form.getByLabelText("Name"), { target: { value: "Seed swap" } });
  fireEvent.change(form.getByLabelText("Sites"), { target: { value: "https://www.SeedSwap.example/login\nshop.seedswap.example" } });
  fireEvent.change(form.getByLabelText("Username"), { target: { value: "sam" } });
  fireEvent.click(form.getByRole("button", { name: "Add login" }));
  await advance(50);
  expect(form.getByRole("alert").textContent).toBe("Add the password, or generate one.");
  const password = form.getByLabelText("Password") as HTMLInputElement;
  expect(password.type).toBe("password");
  fireEvent.change(password, { target: { value: "Pumpkin-Vine-77" } });
  fireEvent.click(form.getByRole("button", { name: "Show password" }));
  expect(password.type).toBe("text");
  fireEvent.click(form.getByRole("button", { name: "Add login" }));
  await advance(600);
  expect(unlock).toHaveBeenCalledWith("Add Seed swap");
  const added = sim().view.logins.find((l) => l.name === "Seed swap")!;
  expect(added.sites).toEqual(["seedswap.example", "shop.seedswap.example"]);
  expect(sim().passwords.get(added.login_id)).toBe("Pumpkin-Vine-77");
  expect(screen.getByText("Seed swap added")).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Seed swap/ })).toBeTruthy();
  const kept = JSON.stringify(Object.values(app.session.commands.get())) + JSON.stringify({ ...localStorage });
  expect(kept).not.toContain("Pumpkin-Vine-77");
});

it("the computer's refusal keeps the form open with what was typed", async () => {
  sim().seed();
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: "Add a login" }));
  const form = sheet();
  fireEvent.change(form.getByLabelText("Name"), { target: { value: "Router" } });
  fireEvent.change(form.getByLabelText("Sites"), { target: { value: "192.168.1.1" } });
  fireEvent.change(form.getByLabelText("Username"), { target: { value: "admin" } });
  fireEvent.change(form.getByLabelText("Password"), { target: { value: "hunter22" } });
  fireEvent.click(form.getByRole("button", { name: "Add login" }));
  await advance(600);
  expect(screen.getByText("192.168.1.1 is an IP address. Use the site's name.")).toBeTruthy();
  expect((form.getByLabelText("Password") as HTMLInputElement).value).toBe("hunter22");
  expect(sim().view.logins.some((l) => l.name === "Router")).toBe(false);
});

it("shows a password only after the phone's unlock, then hides it again", async () => {
  sim().seed();
  const unlock = confirm();
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: /^Facebook/ }));
  fireEvent.click(sheet().getByRole("button", { name: "Show password" }));
  await advance(300);
  expect(unlock).toHaveBeenCalledWith("Show the password for Facebook");
  expect(sheet().getByRole("group", { name: "Password for Facebook" }).textContent).toContain("kV7-qPz2!mWx9_nRt4Lh");
  expect(sim().view.history[0]?.event).toBe("shown");
  await advance(SHOW_MS);
  expect(sheet().queryByRole("group", { name: "Password for Facebook" })).toBeNull();
});

it("a refused unlock shows and sends nothing", async () => {
  sim().seed();
  confirm().mockRejectedValue(new Error("Not unlocked. Try again with your screen lock."));
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: /^Facebook/ }));
  fireEvent.click(sheet().getByRole("button", { name: "Show password" }));
  await advance(300);
  expect(screen.getByText("Not unlocked. Try again with your screen lock.")).toBeTruthy();
  expect(sim().view.history.some((h) => h.event === "shown")).toBe(false);
});

it("deletes only after a second word and the unlock", async () => {
  sim().seed();
  const unlock = confirm();
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: /^Facebook/ }));
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  expect(sheet().getByText(/removes its password and 2FA key from Secret Service/)).toBeTruthy();
  fireEvent.click(sheet().getByRole("button", { name: "Keep it" }));
  expect(sim().view.logins.some((l) => l.name === "Facebook")).toBe(true);
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  await advance(600);
  expect(unlock).toHaveBeenCalledWith("Delete Facebook");
  expect(sim().view.logins.some((l) => l.name === "Facebook")).toBe(false);
  expect(screen.getByText("Facebook deleted")).toBeTruthy();
});

it("Pause all pauses every login and says so", async () => {
  sim().seed();
  render(<AgentLogins route={route} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: "Pause all" }));
  await advance(600);
  expect(sim().view.paused_all).toBe(true);
  expect(screen.getByText("All agent logins paused")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Resume all" })).toBeTruthy();
});

it("a code the site sent is typed into the request and goes to the computer, not the agent or the phone's storage", async () => {
  sim().seed();
  const id = sim().ask("login_code");
  render(<AttentionList projectId={ids.pTide} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: /signing in to Facebook and needs the code/ }));
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  const field = screen.getByLabelText("Code");
  fireEvent.change(field, { target: { value: "12" } });
  fireEvent.click(screen.getByRole("button", { name: "Fill code" }));
  expect(screen.getByText("A code is 3 to 12 letters or digits.")).toBeTruthy();
  fireEvent.change(field, { target: { value: "482 913" } });
  fireEvent.click(screen.getByRole("button", { name: "Fill code" }));
  await advance(800);
  expect(sim().codes.get(id)).toBe("482913");
  expect(app.session.state.get().approvals[id]?.state).toBe("allowed");
  const kept = JSON.stringify(Object.values(app.session.commands.get())) + JSON.stringify({ ...localStorage });
  // The whole code, either way it was typed: ids are random hex, and may hold
  // any three digits.
  expect(kept).not.toMatch(/482 ?913/);
});

it("allowing a login asks for the phone's unlock first, and a refused unlock sends nothing", async () => {
  sim().seed();
  const id = sim().ask("login_use");
  const unlock = confirm().mockRejectedValueOnce(new Error("Not unlocked. Try again with your screen lock."));
  render(<AttentionList projectId={ids.pTide} />, { wrapper });
  await advance(600);
  fireEvent.click(screen.getByRole("button", { name: /wants to use the login Facebook/ }));
  fireEvent.click(screen.getAllByRole("button", { name: "Allow" }).at(-1)!);
  await advance(600);
  expect(unlock).toHaveBeenCalledWith(expect.stringContaining("sign in to Facebook"));
  expect(app.session.state.get().approvals[id]?.state).toBe("pending");
  fireEvent.click(screen.getAllByRole("button", { name: "Allow" }).at(-1)!);
  await advance(800);
  expect(app.session.state.get().approvals[id]?.state).toBe("allowed");
  expect(sim().view.history.slice(0, 2).map((h) => h.event)).toEqual(["filled", "approved"]);
});

it("tells of a use while the app is open, only for logins that ask to be told", async () => {
  sim().seed();
  const [forum, facebook] = sim().view.logins;
  render(<LoginUseNotices />, { wrapper });
  await advance(300);
  act(() => { sim().use(forum!.login_id, ids.tFlaky); });
  await advance(300);
  expect(screen.queryByText(/was used on/)).toBeNull();
  act(() => { sim().use(facebook!.login_id, ids.tFlaky); });
  await advance(300);
  expect(screen.getByText("Facebook was used on facebook.com for “Fix the flaky retry test in the payments client”")).toBeTruthy();
});

it("a replayed use is not told of again", () => {
  const used = { type: "agent_login_used", login_id: "l", name: "Facebook", task_id: ids.tFlaky, site: "facebook.com", notify: true } as const;
  const env = (seq: number) => ({ seq, at: 1, source: { kind: "companion" }, provenance: "observed", event: used }) as Parameters<typeof applyEvent>[1];
  const replayed = applyEvent({ ...emptyHostState, caughtUp: false }, env(5), 1);
  expect(replayed.agentLoginUse).toBeNull();
  expect(replayed.agentLoginsRevision).toBe(1);
  const live = applyEvent({ ...emptyHostState, caughtUp: true }, env(6), 1);
  expect(live.agentLoginUse).toMatchObject({ seq: 6, name: "Facebook", site: "facebook.com" });
});

it("keeps no password, key or code in what the phone stores of a command", () => {
  const details: LoginDetails = { name: "x", sites: ["x.com"], username: "u", notes: "", who: { projects: null, agents: null }, how: "whenever", notify: false };
  expect(withoutSecret({ command: "agent_login", action: { action: "add", details, password: "p", code_key: "k" } }))
    .toEqual({ command: "agent_login", action: { action: "add", details, password: "", code_key: "" } });
  expect(withoutSecret({ command: "agent_login", action: { action: "set_secret", login_id: "l", password: null, code_key: "k" } }))
    .toEqual({ command: "agent_login", action: { action: "set_secret", login_id: "l", password: null, code_key: "" } });
  expect(withoutSecret({ command: "agent_login", action: { action: "answer_code", approval_id: "a", code: "123456" } }))
    .toEqual({ command: "agent_login", action: { action: "answer_code", approval_id: "a", code: "" } });
  const pause = { command: "agent_login", action: { action: "pause", login_id: "l" } } as const;
  expect(withoutSecret(pause)).toBe(pause);
  expect(withoutSecret({ command: "unlock_desktop", password: "p" })).toEqual({ command: "unlock_desktop", password: "" });
});

it("generates passwords evenly from an alphabet without look-alikes, with every kind sites ask for", () => {
  for (let i = 0; i < 50; i++) {
    const p = generatePassword();
    expect(p).toHaveLength(20);
    expect(p).toMatch(/[a-z]/); expect(p).toMatch(/[A-Z]/); expect(p).toMatch(/[0-9]/); expect(p).toMatch(/[-_!@#%]/);
    expect(p).not.toMatch(/[lIO01]/);
  }
  // Values past the last whole run of the alphabet are thrown away, not wrapped.
  const draws = [[0xffff_ffff, 0, 25, 26, 52], [60, 61, 0, 25, 26]];
  const random = (n: number) => {
    const d = draws.shift();
    if (!d) throw new Error("drew too often");
    return Uint32Array.from({ length: n }, (_, i) => d[i] ?? 0);
  };
  expect(generatePassword(5, random)).toBe("aBC6#");
});

it("reads sites and codes the way the computer does", () => {
  expect(splitSites(" facebook.com, messenger.com\n\nm.facebook.com ")).toEqual(["facebook.com", "messenger.com", "m.facebook.com"]);
  expect(signInCode("482 913")).toBe("482913");
  expect(signInCode("G-482913")).toBe("G482913");
  expect(signInCode("12")).toBeNull();
  expect(signInCode("1234!")).toBeNull();
});
