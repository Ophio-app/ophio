// SPDX-License-Identifier: Apache-2.0
import type { AttentionItem, Account, AccountAction, AccountEvent, AccountsView, ProtocolError, ServiceProfile, SignInStage } from "../../protocol/types";
import type { SimHost } from "./host";

const fail = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message } as ProtocolError);

const BUILTINS: Account[] = [
  { account_id: "claude", service: "Claude Code", kind: "agent", builtin: true, identity: "sam@example.com", state: { state: "active" }, checked_at: null, variables: [], manage_url: "https://claude.ai/settings/account", needs: null, browser_sign_in: true, phone_sign_in: true, used_by: [] },
  { account_id: "codex", service: "Codex", kind: "agent", builtin: true, identity: "sam@example.org", state: { state: "active" }, checked_at: null, variables: [], manage_url: "https://chatgpt.com/#settings/Security", needs: null, browser_sign_in: true, phone_sign_in: true, used_by: [] },
  { account_id: "github", service: "GitHub", kind: "cli", builtin: true, identity: "sam-builds", state: { state: "active" }, checked_at: null, variables: ["GH_CONFIG_DIR"], manage_url: "https://github.com/settings/applications", needs: null, browser_sign_in: true, used_by: [] },
  { account_id: "openrouter", service: "OpenRouter", kind: "api_key", builtin: true, identity: null, state: { state: "signed_out" }, checked_at: null, variables: ["OPENROUTER_API_KEY"], manage_url: "https://openrouter.ai/settings/keys", needs: "key", browser_sign_in: false, used_by: [] },
  { account_id: "google", service: "Google (Gmail)", kind: "oauth", builtin: true, identity: null, state: { state: "signed_out" }, checked_at: null, variables: ["GOOGLE_OAUTH_ACCESS_TOKEN"], manage_url: "https://myaccount.google.com/permissions", needs: "client", browser_sign_in: true, used_by: [] },
];

type PhoneRun = { id: string; by: string; timers: ReturnType<typeof setTimeout>[]; waitingForCode: boolean };

const HOSTS: Record<string, string> = { claude: "claude.ai", codex: "auth.openai.com", github: "github.com", google: "accounts.google.com" };

/** The computer's accounts (`crates/accounts`), as the phone sees them. */
export class AccountsSim {
  view: AccountsView;
  /** The next sign-in fails with this reason, as a refused or closed browser would. */
  failSignIn: string | null = null;
  /** How long a Codex sign-in on the phone takes to finish by itself; 0 or null means never. */
  autoFinishMs: number | null = 8_000;
  /** Where a failed sign-in on the phone points to, as the provider's help page. Used once. */
  failHelpUrl: string | null = null;
  #timers: ReturnType<typeof setTimeout>[] = [];
  #phone: PhoneRun | null = null;
  #lapses = 0;
  constructor(private readonly host: SimHost) {
    const now = Date.now();
    this.view = {
      accounts: BUILTINS.map(a => ({ ...a, checked_at: a.state.state === "active" ? now - 6 * 60_000 : null })),
      sign_in: null,
      history: [
        { at: now - 2 * 86400_000, account_id: "github", service: "GitHub", event: "signed_in", by: "Pixel 9", detail: "sam-builds" },
        { at: now - 3 * 86400_000, account_id: "claude", service: "Claude Code", event: "signed_in", by: "This computer", detail: null },
      ],
      store: "Secret Service",
      store_note: null,
      browser: "Firefox",
      password_manager_done: false,
    };
  }
  /** As the computer: a sign-in's link and code only to a phone that finishes it itself. */
  query(onPhone = false): { kind: "accounts"; view: AccountsView } {
    const view = structuredClone(this.view);
    const stage = view.sign_in?.stage;
    if (!onPhone && view.sign_in && stage?.stage === "on_phone") view.sign_in = { ...view.sign_in, stage: { stage: "browser", url_host: stage.url_host } };
    return { kind: "accounts", view };
  }
  act(action: AccountAction, by = "Pixel 9"): null {
    switch (action.action) {
      case "sign_in": return action.on_phone ? this.#signInOnPhone(action.account_id, by) : this.#signIn(action.account_id, by);
      case "submit_sign_in_code": {
        const s = this.view.sign_in;
        if (s && (s.account_id !== action.account_id || s.started_at !== action.started_at)) throw fail("conflict", "That code was for an earlier sign-in. Start the sign-in again.");
        return this.#submitCode(action.code, by);
      }
      case "dismiss_sign_in_notice":
        this.#resolveNotice(action.account_id);
        return null;
      case "cancel_sign_in":
        if (this.view.sign_in && !finished(this.view.sign_in.stage)) {
          this.#clearPhone();
          this.#stage({ stage: "cancelled" });
        }
        return null;
      case "pause":
      case "resume": {
        const a = this.#account(action.account_id);
        if (a.state.state === "signed_out") throw fail("account_not_ready", `${a.service} is not signed in.`);
        a.state = { state: action.action === "pause" ? "paused" : "active" };
        this.#record(a, action.action === "pause" ? "paused" : "resumed", by);
        return null;
      }
      case "pause_all":
        for (const a of this.view.accounts) if (a.state.state === "active" || a.state.state === "needs_attention") a.state = { state: "paused" };
        this.view.history.unshift({ at: Date.now(), account_id: "", service: "All accounts", event: "paused_all", by, detail: null });
        this.#changed();
        return null;
      case "sign_out":
      case "delete": {
        const a = this.#account(action.account_id);
        a.state = { state: "signed_out" };
        a.identity = null;
        a.checked_at = Date.now();
        if (action.action === "delete") {
          if (a.kind === "api_key") a.needs = "key";
          if (a.kind === "oauth") a.needs = "client";
          if (!a.builtin) this.view.accounts = this.view.accounts.filter(x => x !== a);
        }
        this.#record(a, action.action === "delete" ? "deleted" : "signed_out", by);
        return null;
      }
      case "set_key": {
        const a = this.#account(action.account_id);
        if (a.kind === "oauth") {
          if (!clientId(action.key)) throw fail("invalid_request", "That is not an OAuth client's JSON. Download it again from the provider.");
          a.needs = null;
          this.#record(a, "key_set", by, "OAuth client");
          return null;
        }
        if (a.kind !== "api_key") throw fail("invalid_request", `${a.service} has no key to set. Sign in instead.`);
        if (action.key.trim().length < 8 || /\s/.test(action.key.trim())) throw fail("invalid_request", "That does not look like a key.");
        a.state = { state: "active" };
        a.needs = null;
        a.checked_at = Date.now();
        this.#record(a, "key_set", by);
        return null;
      }
      case "add_service": return this.#add(action.service, by);
      case "check":
        if (action.account_id !== null) this.#account(action.account_id);
        for (const a of this.view.accounts) if (action.account_id === null || a.account_id === action.account_id) a.checked_at = Date.now();
        this.#changed();
        return null;
      case "set_up_password_manager":
        return null;
      case "password_manager_done":
        this.view.password_manager_done = true;
        this.#changed();
        return null;
    }
  }
  /** A task running on the account now, as the computer reports it. */
  use(accountId: string, taskId: string): void {
    this.#account(accountId).used_by.push(taskId);
    this.#changed();
  }
  dispose(): void {
    for (const t of this.#timers) clearTimeout(t);
    this.#clearPhone();
  }
  /** Completes the running sign-in on the phone now: checking, then done (or failed as `failSignIn` says). */
  finishSignIn(): void {
    const run = this.#phone;
    if (!run || !this.view.sign_in || finished(this.view.sign_in.stage)) return;
    for (const t of run.timers) clearTimeout(t);
    run.timers = [];
    this.#stage({ stage: "checking" });
    this.#conclude(run);
  }
  /** The computer reports the agent signed out, and tells this phone once per lapse. */
  signedOut(id: "claude" | "codex", mayManage: boolean): void {
    const a = this.#account(id);
    const name = id === "claude" ? "Claude" : "Codex";
    a.state = { state: "signed_out" };
    a.identity = null;
    a.checked_at = Date.now();
    this.#changed();
    const harness = this.host.harnesses.find(h => h.kind === (id === "claude" ? "claude_code" : "codex"));
    if (harness) {
      harness.account = { state: "signed_out", remedy: `Run ${id} ${id === "claude" ? "auth login" : "login"} on the computer.` };
      this.host.emit({ type: "harnesses_changed", harnesses: this.host.harnesses });
    }
    if (!mayManage || this.host.attention.some(x => x.account_id === id && x.resolved_at === null)) return;
    const item: AttentionItem = {
      attention_id: `sign-in-${id}-${++this.#lapses}`, kind: "waiting_for_you", topic: "needs_you", account_id: id,
      title: `${name} isn’t signed in on ${this.host.host.name}`,
      body: `New ${name} tasks can’t start until it’s signed in again. You can sign in from this phone.`,
      task_id: null, run_id: null, approval_id: null, created_at: Date.now(), resolved_at: null, priority: 5,
    };
    this.host.attention.push(item);
    this.host.emit({ type: "attention_upserted", item });
  }
  #recovered(id: string): void {
    const kind = id === "claude" ? "claude_code" : id === "codex" ? "codex" : null;
    const harness = kind && this.host.harnesses.find(h => h.kind === kind);
    if (harness && harness.account.state === "signed_out") {
      harness.account = { state: "signed_in", account_label: "Personal", plan: id === "claude" ? "Max" : "Pro" };
      this.host.emit({ type: "harnesses_changed", harnesses: this.host.harnesses });
    }
    this.#resolveNotice(id);
  }
  /** Puts away this lapse's notice. New objects: the phone's copy is its own. */
  #resolveNotice(id: string): void {
    const open = this.host.attention.filter(x => x.account_id === id && x.resolved_at === null).map(x => x.attention_id);
    if (!open.length) return;
    const now = Date.now();
    this.host.attention = this.host.attention.map(x => open.includes(x.attention_id) ? { ...x, resolved_at: now } : x);
    for (const attention_id of open) this.host.emit({ type: "attention_resolved", attention_id });
  }
  #clearPhone(): void {
    if (this.#phone) for (const t of this.#phone.timers) clearTimeout(t);
    this.#phone = null;
  }
  #after(run: { timers: ReturnType<typeof setTimeout>[] }, ms: number, fn: () => void): void {
    run.timers.push(setTimeout(() => {
      if (this.#phone === run && this.view.sign_in && !finished(this.view.sign_in.stage)) fn();
    }, ms));
  }
  #signInOnPhone(id: string, by: string): null {
    const a = this.#account(id);
    if (!a.phone_sign_in) throw fail("invalid_request", `${a.service} can't be signed in on the phone.`);
    if (this.view.sign_in && !finished(this.view.sign_in.stage)) throw fail("conflict", "Another sign-in is running. Finish or cancel it first.");
    this.#clearPhone();
    const run: PhoneRun = { id, by, timers: [], waitingForCode: false };
    this.#phone = run;
    this.view.sign_in = { account_id: id, service: a.service, started_at: Date.now(), stage: { stage: "starting" } };
    this.#changed();
    this.#after(run, 400, () => {
      if (id === "claude") {
        run.waitingForCode = true;
        this.#stage({ stage: "on_phone", link: "https://claude.com/cai/oauth/authorize?code=true&state=sim", url_host: "claude.com", paste_code: true });
        return;
      }
      this.#stage({ stage: "on_phone", link: "https://auth.openai.com/codex/device", url_host: "auth.openai.com", code: "AB12-CDE34" });
      const wait = this.autoFinishMs;
      if (wait) {
        this.#after(run, wait, () => this.#stage({ stage: "checking" }));
        this.#after(run, wait + 600, () => this.#conclude(run));
      }
    });
    return null;
  }
  #submitCode(code: string, _by: string): null {
    const run = this.#phone;
    const stage = this.view.sign_in?.stage;
    if (!run || !run.waitingForCode || stage?.stage !== "on_phone" || !stage.paste_code) throw fail("invalid_request", "No sign-in is waiting for a code.");
    if (code.trim() === "bad") {
      this.#stage({ ...stage, code_refused: true });
      return null;
    }
    run.waitingForCode = false;
    this.#stage({ stage: "checking" });
    this.#after(run, 800, () => this.#conclude(run));
    return null;
  }
  /** Ends the sign-in on the phone as done, or as failed when a failure was asked for. */
  #conclude(run: PhoneRun): void {
    const a = this.#account(run.id);
    const failure = this.failSignIn;
    const helpUrl = this.failHelpUrl;
    this.failSignIn = null;
    this.failHelpUrl = null;
    this.#phone = null;
    if (failure) {
      this.#stage(helpUrl ? { stage: "failed", reason: failure, help_url: helpUrl } : { stage: "failed", reason: failure });
      this.#record(a, "sign_in_failed", run.by, failure);
      return;
    }
    a.state = { state: "active" };
    a.identity = "sam@example.com";
    a.checked_at = Date.now();
    this.#stage({ stage: "done", identity: a.identity });
    this.#record(a, "signed_in", run.by, a.identity);
    this.#recovered(run.id);
  }
  #signIn(id: string, by: string): null {
    const a = this.#account(id);
    if (a.needs === "key") throw fail("invalid_request", `${a.service} uses a key. Set its key instead.`);
    if (a.needs === "client") throw fail("invalid_request", `Set ${a.service}'s OAuth client first.`);
    if (this.view.sign_in && !finished(this.view.sign_in.stage)) throw fail("conflict", "Another sign-in is running. Finish or cancel it first.");
    const host = HOSTS[id] ?? "example.com";
    this.view.sign_in = { account_id: id, service: a.service, started_at: Date.now(), stage: { stage: "starting" } };
    this.#changed();
    const failure = this.failSignIn;
    this.failSignIn = null;
    const step = (ms: number, fn: () => void) => this.#timers.push(setTimeout(() => {
      if (this.view.sign_in?.account_id === id && !finished(this.view.sign_in.stage)) fn();
    }, ms));
    step(400, () => this.#stage(id === "codex" || id === "github" ? { stage: "code", code: "WDJB-MJHT", url_host: host } : { stage: "browser", url_host: host }));
    step(3_000, () => this.#stage({ stage: "checking" }));
    step(3_600, () => {
      if (failure) {
        this.#stage({ stage: "failed", reason: failure });
        this.#record(a, "sign_in_failed", by, failure);
        return;
      }
      a.state = { state: "active" };
      a.identity = id === "github" ? "sam-builds" : "sam@example.com";
      a.checked_at = Date.now();
      this.#stage({ stage: "done", identity: a.identity });
      this.#record(a, "signed_in", by, a.identity);
      this.#recovered(id);
    });
    return null;
  }
  #add(service: ServiceProfile, by: string): null {
    const name = service.name.trim();
    if (!name || name.length > 60) throw fail("invalid_request", "Give the service a name of up to 60 characters.");
    const id = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "service";
    if (this.view.accounts.some(a => a.account_id === id)) throw fail("conflict", `${name} is already in Accounts.`);
    const s = service.service;
    const variable = s.kind === "cli" ? s.config_variable : s.variable;
    if (this.view.accounts.some(a => a.variables.includes(variable))) throw fail("invalid_request", `${this.view.accounts.find(a => a.variables.includes(variable))!.service} already gives tasks ${variable}.`);
    const account: Account = {
      account_id: id, service: name, kind: s.kind, builtin: false, identity: null, state: { state: "signed_out" }, checked_at: null,
      variables: [variable], manage_url: s.manage_url, needs: s.kind === "api_key" ? "key" : null, browser_sign_in: s.kind !== "api_key", used_by: [],
    };
    this.view.accounts.push(account);
    this.#record(account, "service_added", by);
    return null;
  }
  #account(id: string): Account {
    const a = this.view.accounts.find(x => x.account_id === id);
    if (!a) throw fail("not_found", "There is no such account.");
    return a;
  }
  #stage(stage: SignInStage): void {
    if (this.view.sign_in) this.view.sign_in = { ...this.view.sign_in, stage };
    this.#changed();
  }
  #record(a: Account, event: AccountEvent, by: string, detail: string | null = null): void {
    this.view.history.unshift({ at: Date.now(), account_id: a.account_id, service: a.service, event, by, detail });
    this.view.history.splice(200);
    this.#changed();
  }
  #changed(): void {
    this.host.emit({ type: "accounts_changed" }, { kind: "companion" }, "observed");
  }
}

function finished(stage: SignInStage): boolean {
  return ["done", "failed", "cancelled", "timed_out"].includes(stage.stage);
}

/** The client id in an OAuth client download, as the computer reads it. */
function clientId(text: string): string | null {
  try {
    const value = JSON.parse(text) as Record<string, unknown>;
    const fields = (value.installed ?? value.web ?? value) as { client_id?: unknown };
    return typeof fields.client_id === "string" && /^\S{1,256}$/.test(fields.client_id.trim()) ? fields.client_id : null;
  } catch {
    return null;
  }
}
