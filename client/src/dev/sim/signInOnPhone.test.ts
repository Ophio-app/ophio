// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SimHost } from "./host";

let host: SimHost;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost(); });
afterEach(() => { host.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); });

const stage = () => host.accounts.view.sign_in?.stage;
const signIn = (account_id: string) => host.accounts.act({ action: "sign_in", account_id, on_phone: true });
/** A code for the sign-in running now, as the phone names it. */
const submit = (code: string) => host.accounts.act({ action: "submit_sign_in_code", account_id: host.accounts.view.sign_in?.account_id ?? "claude", started_at: host.accounts.view.sign_in?.started_at ?? 0, code });
const open = () => host.attention.filter(a => a.account_id && a.resolved_at === null);

it("advertises the feature and marks only Claude and Codex as phone sign-in", () => {
  expect(host.host.capabilities.features).toContain("sign_in_on_phone_v1");
  const flags = host.accounts.view.accounts.filter(a => a.phone_sign_in).map(a => a.account_id);
  expect(flags).toEqual(["claude", "codex"]);
  expect(() => signIn("github")).toThrow(/can't be signed in on the phone/);
});

it("signs Claude in by a pasted code, refusing a bad one first", async () => {
  signIn("claude");
  expect(stage()).toEqual({ stage: "starting" });
  await vi.advanceTimersByTimeAsync(400);
  expect(stage()).toMatchObject({ stage: "on_phone", url_host: "claude.com", paste_code: true });
  submit(" bad ");
  expect(stage()).toMatchObject({ stage: "on_phone", paste_code: true, code_refused: true });
  submit("good-code");
  expect(stage()).toEqual({ stage: "checking" });
  await vi.advanceTimersByTimeAsync(800);
  expect(stage()).toEqual({ stage: "done", identity: "sam@example.com" });
  expect(JSON.stringify(host.accounts.view.history)).not.toContain("good-code");
});

it("refuses a code when nothing is waiting for one", async () => {
  expect(() => submit("x")).toThrow(/No sign-in is waiting for a code/);
  signIn("codex");
  await vi.advanceTimersByTimeAsync(400);
  expect(() => submit("x")).toThrow(/No sign-in is waiting for a code/);
});

it("finishes Codex by itself after autoFinishMs, or never when it is 0", async () => {
  signIn("codex");
  await vi.advanceTimersByTimeAsync(400);
  expect(stage()).toMatchObject({ stage: "on_phone", code: "AB12-CDE34", url_host: "auth.openai.com" });
  await vi.advanceTimersByTimeAsync(8_000);
  expect(stage()).toEqual({ stage: "checking" });
  await vi.advanceTimersByTimeAsync(700);
  expect(stage()).toMatchObject({ stage: "done" });

  host.accounts.autoFinishMs = 0;
  signIn("codex");
  await vi.advanceTimersByTimeAsync(60_000);
  expect(stage()).toMatchObject({ stage: "on_phone" });
  host.accounts.finishSignIn();
  expect(stage()).toMatchObject({ stage: "done" });
});

it("fails with a help link when asked to, then goes back to normal", async () => {
  host.accounts.failSignIn = "Codex sign-in with a code is off.";
  host.accounts.failHelpUrl = "https://chatgpt.com/help";
  signIn("codex");
  await vi.advanceTimersByTimeAsync(400);
  host.accounts.finishSignIn();
  expect(stage()).toEqual({ stage: "failed", reason: "Codex sign-in with a code is off.", help_url: "https://chatgpt.com/help" });
  expect(host.accounts.failSignIn).toBeNull();
  expect(host.accounts.failHelpUrl).toBeNull();
});

it("never fires the timers of a cancelled sign-in", async () => {
  signIn("codex");
  await vi.advanceTimersByTimeAsync(400);
  host.accounts.act({ action: "cancel_sign_in" });
  expect(stage()).toEqual({ stage: "cancelled" });
  signIn("claude");
  await vi.advanceTimersByTimeAsync(20_000);
  expect(stage()).toMatchObject({ stage: "on_phone", url_host: "claude.com" });
});

it("raises one notice per lapse and resolves it when the sign-in is done", async () => {
  host.accounts.signedOut("claude", true);
  host.accounts.signedOut("claude", true);
  expect(open()).toHaveLength(1);
  expect(open()[0]).toMatchObject({ account_id: "claude", task_id: null, run_id: null, approval_id: null, kind: "waiting_for_you", topic: "needs_you", title: "Claude isn’t signed in on studio" });
  expect(host.harnesses.find(h => h.kind === "claude_code")!.account).toMatchObject({ state: "signed_out" });
  expect(host.accounts.view.accounts.find(a => a.account_id === "claude")!.state.state).toBe("signed_out");

  signIn("claude");
  await vi.advanceTimersByTimeAsync(400);
  submit("ok");
  await vi.advanceTimersByTimeAsync(800);
  expect(open()).toHaveLength(0);
  expect(host.harnesses.find(h => h.kind === "claude_code")!.account.state).toBe("signed_in");
  expect(host.journal.some(j => j.event.type === "attention_resolved")).toBe(true);
});

it("shows no notice to a device without the accounts grant", () => {
  host.accounts.signedOut("codex", false);
  expect(open()).toHaveLength(0);
  expect(host.harnesses.find(h => h.kind === "codex")!.account).toMatchObject({ state: "signed_out", remedy: "Run codex login on the computer." });
});

it("refuses a code typed for an earlier sign-in", async () => {
  signIn("claude");
  await vi.advanceTimersByTimeAsync(400);
  const started = host.accounts.view.sign_in!.started_at;
  expect(() => host.accounts.act({ action: "submit_sign_in_code", account_id: "claude", started_at: started - 1, code: "x" })).toThrow(/earlier sign-in/);
  expect(() => host.accounts.act({ action: "submit_sign_in_code", account_id: "codex", started_at: started, code: "x" })).toThrow(/earlier sign-in/);
});

it("shows the link and code only to a phone that asks for them", async () => {
  signIn("codex");
  await vi.advanceTimersByTimeAsync(400);
  expect(host.accounts.query(true).view.sign_in?.stage).toMatchObject({ stage: "on_phone", code: "AB12-CDE34" });
  expect(host.accounts.query().view.sign_in?.stage).toEqual({ stage: "browser", url_host: "auth.openai.com" });
});

it("Not now puts away the open notice", () => {
  host.accounts.signedOut("claude", true);
  expect(open()).toHaveLength(1);
  host.accounts.act({ action: "dismiss_sign_in_notice", account_id: "claude" });
  expect(open()).toHaveLength(0);
});
