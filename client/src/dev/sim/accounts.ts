// SPDX-License-Identifier: Apache-2.0
import type { Account, AccountAction, AccountEvent, AccountsView, ProtocolError, ServiceProfile, SignInStage } from "../../protocol/types";
import type { SimHost } from "./host";

const fail = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message } as ProtocolError);

const BUILTINS: Account[] = [
  { account_id: "claude", service: "Claude Code", kind: "agent", builtin: true, identity: "sam@example.com", state: { state: "active" }, checked_at: null, variables: [], manage_url: "https://claude.ai/settings/account", needs: null, browser_sign_in: true, used_by: [] },
  { account_id: "codex", service: "Codex", kind: "agent", builtin: true, identity: "sam@example.org", state: { state: "active" }, checked_at: null, variables: [], manage_url: "https://chatgpt.com/#settings/Security", needs: null, browser_sign_in: true, used_by: [] },
  { account_id: "github", service: "GitHub", kind: "cli", builtin: true, identity: "sam-builds", state: { state: "active" }, checked_at: null, variables: ["GH_CONFIG_DIR"], manage_url: "https://github.com/settings/applications", needs: null, browser_sign_in: true, used_by: [] },
  { account_id: "openrouter", service: "OpenRouter", kind: "api_key", builtin: true, identity: null, state: { state: "signed_out" }, checked_at: null, variables: ["OPENROUTER_API_KEY"], manage_url: "https://openrouter.ai/settings/keys", needs: "key", browser_sign_in: false, used_by: [] },
  { account_id: "google", service: "Google (Gmail)", kind: "oauth", builtin: true, identity: null, state: { state: "signed_out" }, checked_at: null, variables: ["GOOGLE_OAUTH_ACCESS_TOKEN"], manage_url: "https://myaccount.google.com/permissions", needs: "client", browser_sign_in: true, used_by: [] },
];

const HOSTS: Record<string, string> = { claude: "claude.ai", codex: "auth.openai.com", github: "github.com", google: "accounts.google.com" };

/** The computer's accounts (`crates/accounts`), as the phone sees them. */
export class AccountsSim {
  view: AccountsView;
  /** The next sign-in fails with this reason, as a refused or closed browser would. */
  failSignIn: string | null = null;
  #timers: ReturnType<typeof setTimeout>[] = [];
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
  query(): { kind: "accounts"; view: AccountsView } {
    return { kind: "accounts", view: structuredClone(this.view) };
  }
  act(action: AccountAction, by = "Pixel 9"): null {
    switch (action.action) {
      case "sign_in": return this.#signIn(action.account_id, by);
      case "cancel_sign_in":
        if (this.view.sign_in && !finished(this.view.sign_in.stage)) this.#stage({ stage: "cancelled" });
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
