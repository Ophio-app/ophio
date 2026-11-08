// SPDX-License-Identifier: Apache-2.0
//
// Signing Claude and Codex in again from the phone: the flow in Accounts
// and its own screen, the notice under Needs you, its notification, and an
// older computer keeping today's way.
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession, withoutSecret } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { RouteProvider } from "../../shell/Screen";
import { Toasts } from "../../design/Toast";
import { trail } from "../../diagnostics/trail";
import { notificationFor, parseOpenRoute } from "../../platform/notifications";
import type { Command } from "../../protocol/types";
import Accounts from "./Accounts";
import SignInScreen from "./SignInScreen";
import { SignInNeedsYou } from "./SignInAgain";
import { AgentsAndAccounts } from "./AgentsAndAccounts";
import Attention from "../work/AttentionScreen";
import { signInWords } from "../control/SignInStrip";
import type { SignIn } from "../../protocol/types";

let app: App;
let opened: string[];
let frames: { type: string; [k: string]: unknown }[];
const queries = (kind: string) => frames.filter((f) => f.type === "query" && (f.query as { kind: string }).kind === kind).map((f) => f.query as Record<string, unknown>);
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
let route: { screen: string; [k: string]: unknown } = { screen: "accounts" };
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "work", depth: 1, active: true, route: route as never }}>
      {children}
      <Toasts />
    </RouteProvider>
  </AppProvider>
);
const accounts = () => window.__sim!.host.accounts;
const account = (id: string) => accounts().view.accounts.find((a) => a.account_id === id)!;
const sheet = () => within(screen.getByRole("dialog"));
const row = (name: string) => screen.getByRole("button", { name: new RegExp(`^${name}`) });
const bodies = () => Object.values(app.session.commands.get()).map((c) => c.body as Command);
const olderComputer = () => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "sign_in_on_phone_v1") } }));
/** Everything the phone keeps or shows, where a pasted code must never be. */
const everywhere = () => JSON.stringify([bodies(), app.session.state.get(), { ...localStorage }, trail.snapshot()]) + document.body.textContent;

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  route = { screen: "accounts" };
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  opened = [];
  platform.openExternal = async (url: string) => { opened.push(url); };
  const profile = (await platform.profiles())[0]!;
  const link = platform.control(profile.profileId);
  const send = link.send.bind(link);
  frames = [];
  link.send = (frame) => { frames.push(JSON.parse(JSON.stringify(frame))); return send(frame); };
  const session = new HostSession(profile.profileId, link);
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => { await Promise.resolve(); });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.useRealTimers();
});

it("Claude signs in on the phone: open the page, paste the code, a refused code says so, then done", async () => {
  window.__sim!.signedOut("claude");
  render(<Accounts route={{ screen: "accounts" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Claude Code"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  // The tap shows the next step at once.
  expect(sheet().getByText(/Starting the sign-in on/)).toBeTruthy();
  await advance(1500);
  expect(bodies().find((b) => b.command === "account" && b.action.action === "sign_in")).toMatchObject({ action: { account_id: "claude", on_phone: true } });
  expect(sheet().getByText(/Opens/).textContent).toContain("claude.com in this phone’s browser");
  // The full link is opened, never shown.
  expect(document.body.textContent).not.toContain("oauth/authorize");
  fireEvent.click(sheet().getByRole("button", { name: "Open the sign-in page" }));
  expect(opened).toEqual(["https://claude.com/cai/oauth/authorize?code=true&state=sim"]);

  const field = sheet().getByLabelText("Paste the code from the sign-in page") as HTMLInputElement;
  expect(field.getAttribute("autocapitalize")).toBe("off");
  expect(field.getAttribute("autocorrect")).toBe("off");
  expect(field.getAttribute("spellcheck")).toBe("false");
  expect(field.getAttribute("autocomplete")).toBe("one-time-code");
  fireEvent.change(field, { target: { value: "bad" } });
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  expect(sheet().getByText("Checking the sign-in…")).toBeTruthy();
  await advance(1500);
  expect(sheet().getByText("That code didn’t work. Copy the whole code from the page and paste it again.")).toBeTruthy();
  expect((sheet().getByLabelText("Paste the code from the sign-in page") as HTMLInputElement).value).toBe("");

  const secret = "zQ9-secret-code-77#sim-state";
  fireEvent.change(sheet().getByLabelText("Paste the code from the sign-in page"), { target: { value: secret } });
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  expect(sheet().getByText("Checking the sign-in…")).toBeTruthy();
  await advance(2500);
  expect(sheet().getByText(/Claude is signed in as/).textContent).toBe("Claude is signed in as sam@example.com");
  expect(account("claude")).toMatchObject({ state: { state: "active" }, identity: "sam@example.com" });
  // Sent once, kept nowhere: not in the command record, state, storage, the trail or on screen.
  const started = accounts().view.sign_in!.started_at;
  expect(bodies().filter((b) => b.command === "account" && b.action.action === "submit_sign_in_code").map((b) => b.command === "account" && b.action))
    .toEqual([{ action: "submit_sign_in_code", account_id: "claude", started_at: started, code: "" }, { action: "submit_sign_in_code", account_id: "claude", started_at: started, code: "" }]);
  expect(everywhere()).not.toContain("zQ9-secret");
  fireEvent.click(sheet().getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("a code the computer couldn't be sent is kept for another try, with the computer's words", async () => {
  window.__sim!.signedOut("claude");
  render(<Accounts route={{ screen: "accounts" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Claude Code"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  await advance(1500);
  const real = accounts().act.bind(accounts());
  vi.spyOn(accounts(), "act").mockImplementation((action, by) => {
    if (action.action === "submit_sign_in_code") throw { code: "conflict", message: "The sign-in already ended." };
    return real(action, by);
  });
  fireEvent.change(sheet().getByLabelText("Paste the code from the sign-in page"), { target: { value: "kept-code-1" } });
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  expect(sheet().getByText("Checking the sign-in…")).toBeTruthy();
  await advance(1500);
  expect(screen.getByText("The sign-in already ended.", { exact: false })).toBeTruthy();
  expect((sheet().getByLabelText("Paste the code from the sign-in page") as HTMLInputElement).value).toBe("kept-code-1");
  // Never in a toast or the command record.
  expect(screen.getByRole("region", { name: "Notifications" }).textContent).not.toContain("kept-code-1");
  expect(JSON.stringify(bodies())).not.toContain("kept-code-1");
});

it("Codex shows its code to type on the page, copies it, and finishes by itself", async () => {
  window.__sim!.signedOut("codex");
  const writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  render(<Accounts route={{ screen: "accounts" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Codex"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  await advance(1500);
  expect(sheet().getByText("AB12-CDE34")).toBeTruthy();
  expect(sheet().getByText(/Opens/).textContent).toContain("auth.openai.com in this phone’s browser");
  fireEvent.click(sheet().getByRole("button", { name: "Copy" }));
  await advance(10);
  expect(writeText).toHaveBeenCalledWith("AB12-CDE34");
  expect(screen.getByText("Code copied")).toBeTruthy();
  expect(sheet().getByText(/device code sign-in is off/)).toBeTruthy();
  fireEvent.click(sheet().getByRole("button", { name: "Open ChatGPT’s security settings" }));
  expect(opened).toEqual(["https://chatgpt.com/settings/security"]);
  fireEvent.click(sheet().getByRole("button", { name: "Open the sign-in page" }));
  expect(opened.at(-1)).toBe("https://auth.openai.com/codex/device");
  await advance(9500);
  expect(sheet().getByText(/Codex is signed in as/)).toBeTruthy();
});

it("a failed Codex sign-in says why, offers the page that fixes it and starts again on the phone", async () => {
  window.__sim!.signedOut("codex");
  render(<Accounts route={{ screen: "accounts" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Codex"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  await advance(1500);
  accounts().failSignIn = "Device code sign-in is turned off for this ChatGPT account.";
  accounts().failHelpUrl = "https://chatgpt.com/settings/security";
  window.__sim!.finishSignIn();
  await advance(1500);
  expect(sheet().getByText("The sign-in didn’t work")).toBeTruthy();
  expect(sheet().getByText("Device code sign-in is turned off for this ChatGPT account.")).toBeTruthy();
  fireEvent.click(sheet().getByRole("button", { name: "Open ChatGPT’s security settings" }));
  expect(opened).toEqual(["https://chatgpt.com/settings/security"]);
  fireEvent.click(sheet().getByRole("button", { name: "Try again" }));
  expect(sheet().getByText(/Starting the sign-in on/)).toBeTruthy();
  await advance(1500);
  expect(sheet().getByText("AB12-CDE34")).toBeTruthy();
  const starts = bodies().filter((b) => b.command === "account" && b.action.action === "sign_in");
  expect(starts).toHaveLength(2);
  expect(starts.every((b) => b.command === "account" && b.action.action === "sign_in" && b.action.on_phone === true)).toBe(true);
  // Cancel, shown at once.
  fireEvent.click(sheet().getByRole("button", { name: "Cancel" }));
  expect(sheet().getByText("Sign-in cancelled")).toBeTruthy();
  await advance(1500);
  expect(accounts().view.sign_in?.stage.stage).toBe("cancelled");
});

it("an older computer keeps today's remedy and its own browser, and is never sent a phone sign-in", async () => {
  window.__sim!.signedOut("claude");
  await advance(500);
  const { unmount } = render(<AgentsAndAccounts />, { wrapper });
  expect(screen.getByText("Claude isn’t signed in on studio")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Sign in again" })).toBeTruthy();
  unmount();
  olderComputer();
  render(<AgentsAndAccounts />, { wrapper });
  expect(screen.getByText("Run claude auth login on the computer.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Sign in again" })).toBeNull();
  // Even a send attempt the screen didn't gate is refused here, before it leaves.
  await expect(app.session.command({ command: "account", action: { action: "submit_sign_in_code", account_id: "claude", started_at: 1, code: "x" } }, { label: "Sending" })).rejects.toThrow();
  expect(JSON.stringify(bodies())).not.toContain("submit_sign_in_code");
});

it("the computer's notice sits under Needs you with Sign in again, which opens and starts the sign-in", async () => {
  window.__sim!.signedOut("claude");
  await advance(500);
  render(<SignInNeedsYou />, { wrapper });
  expect(screen.getByText("Claude isn’t signed in on studio")).toBeTruthy();
  expect(screen.getByText(/New Claude tasks can’t start/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Sign in again" }));
  const top = app.ui.get().stacks[app.ui.get().tab].at(-1);
  expect(top).toEqual({ screen: "sign_in", accountId: "claude" });
  cleanup();
  route = top!;
  render(<SignInScreen route={{ screen: "sign_in", accountId: "claude" }} />, { wrapper });
  // Opened by the tap, it starts as soon as the computer's accounts are read.
  await advance(1500);
  expect(bodies().find((b) => b.command === "account" && b.action.action === "sign_in")).toMatchObject({ action: { account_id: "claude", on_phone: true } });
  await advance(1500);
  expect(screen.getByRole("button", { name: "Open the sign-in page" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Paste the code from the sign-in page"), { target: { value: "good-code" } });
  fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
  await advance(2500);
  expect(screen.getByText(/Claude is signed in as/)).toBeTruthy();
  // The computer resolves its notice once signed in.
  expect(Object.values(app.session.state.get().attention).filter((a) => a.account_id && a.resolved_at === null)).toHaveLength(0);
});

it("opened without a tap, as later from a list, the screen waits to be asked", async () => {
  window.__sim!.signedOut("codex");
  route = { screen: "sign_in", accountId: "codex" };
  render(<SignInScreen route={{ screen: "sign_in", accountId: "codex" }} />, { wrapper });
  await advance(1500);
  expect(screen.getByRole("button", { name: "Sign in on this phone" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Sign in on the computer’s screen instead" })).toBeTruthy();
  expect(bodies().some((b) => b.command === "account" && b.action.action === "sign_in")).toBe(false);
});

it("the attention screen shows the sign-in notice instead of a missing task", async () => {
  window.__sim!.signedOut("claude");
  await advance(500);
  const item = Object.values(app.session.state.get().attention).find((a) => a.account_id === "claude")!;
  route = { screen: "attention", attentionId: item.attention_id };
  render(<Attention route={{ screen: "attention", attentionId: item.attention_id }} />, { wrapper });
  expect(screen.getByText("Claude isn’t signed in on studio")).toBeTruthy();
  expect(screen.queryByText(/no longer available/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Sign in again" }));
  expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toEqual({ screen: "sign_in", accountId: "claude" });
});

it("a sign-in notification names the agent and computer and leads to its sign-in", async () => {
  expect(parseOpenRoute({ kind: "sign_in", profileId: "p1", accountId: "claude" })).toEqual({ kind: "sign_in", profileId: "p1", accountId: "claude" });
  expect(parseOpenRoute({ kind: "sign_in", profileId: "p1" })).toBeNull();
  expect(parseOpenRoute({ kind: "sign_in", profileId: "", accountId: "claude" })).toBeNull();
  window.__sim!.signedOut("codex");
  await advance(500);
  const state = app.session.state.get();
  const item = Object.values(state.attention).find((a) => a.account_id === "codex")!;
  expect(notificationFor(item, state, { profileId: "p1", name: "Studio" }, "task_title")).toMatchObject({
    title: "Codex isn’t signed in on Studio", body: "Tap to sign in again.", category: "needs_you",
    route: { kind: "sign_in", profileId: "p1", accountId: "codex" },
  });
  olderComputer();
  expect(notificationFor(item, app.session.state.get(), { profileId: "p1", name: "Studio" }, "task_title")).toBeNull();
});

it("the phone keeps no pasted code once its command has left, only which sign-in it was for", () => {
  const kept = withoutSecret({ command: "account", action: { action: "submit_sign_in_code", account_id: "claude", started_at: 42, code: "abc#state" } });
  expect(JSON.stringify(kept)).not.toContain("abc#state");
  expect(kept).toMatchObject({ action: { account_id: "claude", started_at: 42 } });
});

it("asks for account notices in every snapshot and subscription, and for sign-in links only where the computer offers them", async () => {
  expect(queries("snapshot").length).toBeGreaterThan(0);
  expect(queries("snapshot").every((q) => q.account_items === true)).toBe(true);
  const subs = frames.filter((f) => f.type === "subscribe");
  expect(subs.length).toBeGreaterThan(0);
  expect(subs.every((f) => f.account_items === true)).toBe(true);
  const ask = async () => { const p = app.session.query({ kind: "accounts" }); await advance(500); await p; };
  await ask();
  expect(queries("accounts").at(-1)).toEqual({ kind: "accounts", on_phone: true });
  olderComputer();
  await ask();
  expect(queries("accounts").at(-1)).toEqual({ kind: "accounts" });
});

it("Codex's step never shows the paste box, and Claude's is chosen by paste_code", async () => {
  window.__sim!.signedOut("codex");
  render(<Accounts route={{ screen: "accounts" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Codex"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  // The code comes after the stage: before it, Codex still never asks for a paste.
  for (let i = 0; i < 15; i++) {
    await advance(100);
    expect(sheet().queryByLabelText("Paste the code from the sign-in page")).toBeNull();
  }
  expect(accounts().view.sign_in?.account_id).toBe("codex");
  expect(sheet().getByText("AB12-CDE34")).toBeTruthy();
});

it("Not now puts the notice away at once, and brings it back if the computer refuses", async () => {
  window.__sim!.signedOut("claude");
  await advance(500);
  render(<SignInNeedsYou />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Not now" }));
  expect(screen.queryByText("Claude isn’t signed in on studio")).toBeNull();
  await advance(1500);
  expect(screen.queryByText("Claude isn’t signed in on studio")).toBeNull();
  expect(bodies().find((b) => b.command === "account" && b.action.action === "dismiss_sign_in_notice")).toMatchObject({ action: { account_id: "claude" } });
  // A later lapse raises a new notice; this time the computer refuses.
  window.__sim!.signedOut("claude");
  await advance(500);
  const real = accounts().act.bind(accounts());
  vi.spyOn(accounts(), "act").mockImplementation((action, by) => {
    if (action.action === "dismiss_sign_in_notice") throw { code: "permission_denied", message: "Only a person can put this away." };
    return real(action, by);
  });
  cleanup();
  render(<><SignInNeedsYou /><Toasts /></>, { wrapper: ({ children }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route: route as never }}>{children}</RouteProvider></AppProvider> });
  fireEvent.click(screen.getByRole("button", { name: "Not now" }));
  expect(screen.queryByText("Claude isn’t signed in on studio")).toBeNull();
  await advance(1500);
  expect(screen.getByText("Claude isn’t signed in on studio")).toBeTruthy();
  expect(screen.getByText("Only a person can put this away.")).toBeTruthy();
});

it("Not now isn't offered where the computer can't sign agents in on the phone", async () => {
  window.__sim!.signedOut("claude");
  await advance(500);
  olderComputer();
  render(<SignInNeedsYou />, { wrapper });
  expect(screen.getByText("Claude isn’t signed in on studio")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Sign in again" })).toBeNull();
});

it("words a stage it doesn't know without failing", () => {
  const s = { account_id: "claude", service: "Claude Code", started_at: 1, stage: { stage: "from_the_future" } } as unknown as SignIn;
  expect(signInWords(s)).toEqual({ title: "Signing in to Claude Code", detail: null });
});

it("a sign-in stage this app doesn't know waits plainly, with Cancel, and never breaks the sheet", async () => {
  window.__sim!.signedOut("claude");
  render(<Accounts route={{ screen: "accounts" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Claude Code"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  await advance(1500);
  // A newer computer sends a stage this app has never seen.
  const view = accounts().view;
  view.sign_in = { ...view.sign_in!, stage: { stage: "some_future_stage" } as never };
  window.__sim!.host.emit({ type: "accounts_changed" }, { kind: "companion" }, "observed");
  await advance(1000);
  expect(sheet().getByText(/^Signing in to Claude/)).toBeTruthy();
  const cancel = sheet().getByRole("button", { name: "Cancel" }) as HTMLButtonElement;
  expect(cancel.disabled).toBe(false);
  fireEvent.click(cancel);
  await advance(1000);
  expect(bodies().some((b) => b.command === "account" && b.action.action === "cancel_sign_in")).toBe(true);
});
