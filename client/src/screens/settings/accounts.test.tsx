// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { RouteProvider } from "../../shell/Screen";
import { Toasts } from "../../design/Toast";
import type { InputAction } from "../../protocol/types";
import Accounts from "./Accounts";
import { describedService } from "./AddService";
import { SignInStrip } from "../control/SignInStrip";
import { confirmHere } from "./useAccounts";

let app: App;
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const route = { screen: "accounts" } as const;
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>
      {children}
      <Toasts />
    </RouteProvider>
  </AppProvider>
);
const accounts = () => window.__sim!.host.accounts;
const account = (id: string) => accounts().view.accounts.find((a) => a.account_id === id)!;
/** The sim's agents are signed in, as their agent apps say; a sign-in test
 * starts from one signed out. */
const signedOut = (id: string) => Object.assign(account(id), { identity: null, state: { state: "signed_out" }, checked_at: null });
const row = (name: string) => screen.getByRole("button", { name: new RegExp(`^${name}`) });
const sheet = () => within(screen.getByRole("dialog"));

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = {
    platform,
    profile,
    profiles: [profile],
    session,
    ui: new UiStore(profile.profileId),
    switchTo: vi.fn(),
    pairAnother: vi.fn(),
    rename: vi.fn(),
    forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => {
    await Promise.resolve();
  });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.useRealTimers();
});

it("lists the computer's accounts with who each is signed in as, never a key", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  expect(row("Claude Code")).toBeTruthy();
  expect(screen.getByText("sam@example.com")).toBeTruthy();
  // Codex is signed in, as its agent app says.
  expect(row("Codex").textContent).toContain("sam@example.org");
  expect(row("OpenRouter")).toBeTruthy();
  expect(screen.getByText("Keys and tokens stay on", { exact: false }).textContent).toContain("never a key");
  expect(screen.getByText("Password manager on this computer")).toBeTruthy();
});

it("a sign-in short of a permission says so and offers Sign in again first", async () => {
  const note = "Agents can't change GitHub Actions workflows: this sign-in doesn't include GitHub's workflow permission. Sign in again to add it.";
  account("github").note = note;
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("sam-builds · missing a permission")).toBeTruthy();
  fireEvent.click(row("GitHub"));
  await advance(100);
  expect(sheet().getByText(note)).toBeTruthy();
  expect(sheet().getByRole("button", { name: "Sign in again" }).className).toContain("primary");
});

it("Pause all pauses every active account and says so", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Pause all" }));
  await advance(1000);
  expect(account("claude").state.state).toBe("paused");
  expect(account("github").state.state).toBe("paused");
  expect(screen.getByText("All accounts paused")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Pause all" })).toHaveProperty("disabled", true);
});

it("sets a key without keeping it on the phone", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("OpenRouter"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Set key" }));
  const field = sheet().getByLabelText("OpenRouter key") as HTMLInputElement;
  expect(field.type).toBe("password");
  fireEvent.change(field, { target: { value: "sk-or-v1-abcdef123456" } });
  fireEvent.click(sheet().getByRole("button", { name: "Save key" }));
  expect(field.value).toBe("");
  await advance(1000);
  expect(account("openrouter").state.state).toBe("active");
  expect(screen.getByText("Key saved for OpenRouter")).toBeTruthy();
  expect(JSON.stringify(app.session.state.get())).not.toContain("sk-or-v1");
  expect(JSON.stringify(localStorage)).not.toContain("sk-or-v1");
});

it("a refused key keeps the sheet open with the computer's words", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("OpenRouter"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Set key" }));
  fireEvent.change(sheet().getByLabelText("OpenRouter key"), { target: { value: "short" } });
  fireEvent.click(sheet().getByRole("button", { name: "Save key" }));
  await advance(1000);
  expect(screen.getByText("That does not look like a key.")).toBeTruthy();
  expect(account("openrouter").state.state).toBe("signed_out");
  expect(sheet().getByLabelText("OpenRouter key")).toBeTruthy();
});

it("signing in follows the computer's browser in Desktop control", async () => {
  signedOut("codex");
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Codex"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  await advance(500);
  const ui = app.ui.get();
  expect(ui.tab).toBe("control");
  expect(ui.stacks.control.at(-1)).toMatchObject({ screen: "desktop" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByText("Signing in to Codex")).toBeTruthy();
  await advance(4000);
  expect(account("codex")).toMatchObject({ identity: "sam@example.com", state: { state: "active" } });
});

it("an OAuth service without a client says how to give it one, instead of a sign-in", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Google \\(Gmail\\)"));
  await advance(100);
  expect(sheet().getByText("Needs your own OAuth client")).toBeTruthy();
  expect(sheet().getByText("accounts set-client google FILE", { exact: false })).toBeTruthy();
  expect(sheet().queryByRole("button", { name: "Sign in" })).toBeNull();
});

it("an OAuth client chosen on the phone is sent once, and the sheet then offers a sign-in", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Google \\(Gmail\\)"));
  await advance(100);
  const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
  const json = JSON.stringify({ installed: { client_id: "123-abc.apps.googleusercontent.com", client_secret: "GOCSPX-secret" } });
  fireEvent.change(input, { target: { files: [new File([json], "client.json", { type: "application/json" })] } });
  await advance(1000);
  expect(screen.getByText("OAuth client saved for Google (Gmail)")).toBeTruthy();
  expect(account("google").needs).toBeNull();
  expect(sheet().getByRole("button", { name: "Sign in" })).toBeTruthy();
  expect(input.value).toBe("");
  expect(JSON.stringify(app.session.state.get())).not.toContain("GOCSPX");
  expect(JSON.stringify(localStorage)).not.toContain("GOCSPX");
});

it("a file that isn't an OAuth client is refused in the computer's words", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Google \\(Gmail\\)"));
  await advance(100);
  const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
  fireEvent.change(input, { target: { files: [new File(["not json"], "notes.json")] } });
  await advance(1000);
  expect(screen.getByText("That is not an OAuth client's JSON. Download it again from the provider.")).toBeTruthy();
  expect(account("google").needs).toBe("client");
  fireEvent.change(input, { target: { files: [new File(["x".repeat(64 * 1024 + 1)], "big.json")] } });
  await advance(100);
  expect(screen.getByText("big.json is too large to be an OAuth client file.")).toBeTruthy();
});

it("pauses one account and deletes a built-in only after a second word", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("GitHub"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Pause" }));
  await advance(1000);
  expect(account("github").state.state).toBe("paused");
  expect(sheet().getByText("Paused")).toBeTruthy();
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  expect(sheet().getByText("Delete GitHub?")).toBeTruthy();
  expect(account("github").state.state).toBe("paused");
  fireEvent.click(sheet().getByRole("button", { name: "Keep it" }));
  expect(sheet().queryByText("Delete GitHub?")).toBeNull();
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  await advance(1000);
  expect(account("github")).toMatchObject({ identity: null, state: { state: "signed_out" } });
  expect(screen.getByText("GitHub deleted")).toBeTruthy();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(row("GitHub")).toBeTruthy();
});

it("an action the computer accepts but then fails is not shown as done", async () => {
  const act = accounts().act.bind(accounts());
  vi.spyOn(accounts(), "act").mockImplementation((action, by) => {
    if (action.action === "delete") throw { code: "conflict", message: "A copy is in a locked part of the keyring. Unlock it, then try again" };
    return act(action, by);
  });
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Claude Code"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  fireEvent.click(sheet().getByRole("button", { name: "Delete permanently" }));
  await advance(1000);
  expect(screen.getByText("A copy is in a locked part of the keyring. Unlock it, then try again", { exact: false })).toBeTruthy();
  expect(screen.queryByText("Claude Code deleted")).toBeNull();
  expect(screen.getByRole("dialog")).toBeTruthy();
  expect(account("claude").state.state).toBe("active");
});

it("a signed-in OAuth account can have its client replaced", async () => {
  Object.assign(account("google"), { needs: null, state: { state: "active" }, identity: "sam@example.com" });
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(row("Google \\(Gmail\\)"));
  await advance(100);
  expect(sheet().getByRole("button", { name: "Replace the OAuth client" })).toBeTruthy();
  const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
  const json = JSON.stringify({ installed: { client_id: "456-def.apps.googleusercontent.com", client_secret: "GOCSPX-other" } });
  fireEvent.change(input, { target: { files: [new File([json], "client.json", { type: "application/json" })] } });
  await advance(1000);
  expect(screen.getByText("OAuth client saved for Google (Gmail)")).toBeTruthy();
  expect(JSON.stringify(app.session.state.get())).not.toContain("GOCSPX");
});

it("adds a service the form describes, and the computer's refusal is shown", async () => {
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Add a service" }));
  await advance(100);
  const add = () => sheet().getByRole("button", { name: "Add" });
  expect(add()).toHaveProperty("disabled", true);
  fireEvent.change(sheet().getByLabelText("Name"), { target: { value: "Groq" } });
  fireEvent.change(sheet().getByLabelText("Environment variable"), { target: { value: "OPENROUTER_API_KEY" } });
  fireEvent.click(add());
  await advance(1000);
  expect(screen.getByText("OpenRouter already gives tasks OPENROUTER_API_KEY.")).toBeTruthy();
  fireEvent.change(sheet().getByLabelText("Environment variable"), { target: { value: "GROQ_API_KEY" } });
  fireEvent.click(add());
  await advance(1000);
  expect(account("groq")).toMatchObject({ kind: "api_key", variables: ["GROQ_API_KEY"], needs: "key" });
  expect(screen.getByText("Groq added")).toBeTruthy();
  expect(row("Groq")).toBeTruthy();
});

it("describes each kind of service only once its required fields are filled", () => {
  const blank = { name: "", variable: "", check: "", manage: "", authorize: "", token: "", scopes: "", signIn: "", status: "", signOut: "" };
  expect(describedService("api_key", { ...blank, name: "Groq" })).toBeNull();
  expect(describedService("api_key", { ...blank, name: " Groq ", variable: "GROQ_API_KEY", check: " " })).toEqual({
    name: "Groq", service: { kind: "api_key", variable: "GROQ_API_KEY", check_url: null, manage_url: null },
  });
  const oauth = { ...blank, name: "Slack", variable: "SLACK_TOKEN", authorize: "https://slack.com/oauth" };
  expect(describedService("oauth", oauth)).toBeNull();
  expect(describedService("oauth", { ...oauth, token: "https://slack.com/api/token", scopes: "chat:write, users:read  im:read" })).toMatchObject({
    service: { kind: "oauth", scopes: ["chat:write", "users:read", "im:read"] },
  });
  const cli = { ...blank, name: "gcloud", variable: "CLOUDSDK_CONFIG", signIn: "gcloud auth login" };
  expect(describedService("cli", cli)).toBeNull();
  expect(describedService("cli", { ...cli, status: "gcloud auth list" })).toEqual({
    name: "gcloud",
    service: { kind: "cli", sign_in: "gcloud auth login", status: "gcloud auth list", sign_out: null, config_variable: "CLOUDSDK_CONFIG", manage_url: null },
  });
});

it("a device without the accounts grant is told only the computer can allow it", async () => {
  window.__sim!.permissions({ accounts: false });
  await advance(3000);
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("This device may not manage accounts")).toBeTruthy();
  expect(screen.getByText("Only the computer can allow it", { exact: false })).toBeTruthy();
  expect(screen.queryByText("devices allow", { exact: false })?.textContent).not.toContain("Connections on a device");
  expect(screen.queryByRole("button", { name: /^Claude Code/ })).toBeNull();
});

it("an older computer is asked to update rather than refusing each action", async () => {
  app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "accounts_v1") } }));
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("Update the computer")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Pause all" })).toBeNull();
});

it("the Desktop strip shows the code, types it when in control and returns to Accounts", async () => {
  const sent: InputAction[] = [];
  const send = (action: InputAction) => { sent.push(action); return true; };
  const { rerender } = render(<SignInStrip send={send} canType={false} />, { wrapper });
  await advance(500);
  expect(screen.queryByText(/Signing in/)).toBeNull();
  const started = app.session.command({ command: "account", action: { action: "sign_in", account_id: "github" } }, { label: "Signing in to GitHub" });
  await advance(1000);
  await started;
  // Started from another phone: shown while running, but not followed once finished.
  expect(screen.getByText("Signing in to GitHub")).toBeTruthy();
  expect(screen.getByText("WDJB-MJHT")).toBeTruthy();
  expect(screen.getByText("Take over to type it, or type it with the keyboard.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Type it" })).toHaveProperty("disabled", true);
  rerender(<SignInStrip send={send} canType />);
  fireEvent.click(screen.getByRole("button", { name: "Type it" }));
  expect(sent).toEqual([{ action: "text", text: "WDJB-MJHT" }]);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await advance(1000);
  expect(accounts().view.sign_in?.stage.stage).toBe("cancelled");
  expect(screen.queryByText("GitHub sign-in cancelled")).toBeNull();
});

it("the strip follows this phone's own sign-in to its result", async () => {
  signedOut("codex");
  render(<Accounts route={route} />, { wrapper });
  await advance(1000);
  accounts().failSignIn = "The browser was closed.";
  fireEvent.click(row("Codex"));
  await advance(100);
  fireEvent.click(sheet().getByRole("button", { name: "Sign in" }));
  await advance(500);
  cleanup();
  render(<SignInStrip send={() => true} canType />, { wrapper });
  await advance(4000);
  expect(screen.getByText("Codex sign-in failed")).toBeTruthy();
  expect(screen.getByText("The browser was closed.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  await advance(4000);
  expect(screen.getByText("Signed in as sam@example.com")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Back to Accounts" }));
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "accounts" });
  await advance(100);
  expect(screen.queryByText("Signed in as sam@example.com")).toBeNull();
});

it("an account action isn't sent from a phone that can't ask for its unlock", async () => {
  const platform = createSimPlatform();
  await expect(confirmHere({ ...platform, appLock: undefined }, "Pause GitHub")).rejects.toThrow(/can’t confirm/);
  const confirm = vi.fn((_title: string) => platform.appLock!.state());
  await confirmHere({ ...platform, appLock: { ...platform.appLock!, confirm } }, "Pause GitHub");
  expect(confirm).toHaveBeenCalledWith("Pause GitHub");
  const refused = vi.fn((_title: string) => Promise.reject(new Error("Not confirmed")));
  await expect(confirmHere({ ...platform, appLock: { ...platform.appLock!, confirm: refused } }, "x")).rejects.toThrow("Not confirmed");
});
