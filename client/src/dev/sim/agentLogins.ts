// SPDX-License-Identifier: Apache-2.0
import { uuidv7 } from "../../protocol/ids";
import type {
  AgentLogin, AgentLoginAction, AgentLoginEvent, AgentLoginsView, Approval, AttentionItem, LoginDetails, ProtocolError,
} from "../../protocol/types";
import type { SimHost } from "./host";
import { ids } from "./scenario";

const fail = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message } as ProtocolError);

/** `crates/protocol/src/agent_logins.rs`, word for word. */
const DISCLAIMER_VERSION = 1;
const DISCLAIMER = [
  "Agents can use the logins you add here to sign in to websites and act as you there. That includes posting, sending messages, buying, and changing or deleting things.",
  "Øphio is built to keep these passwords away from the AI:",
  "- It doesn't send your passwords to the AI model or its provider. Øphio types them into the page itself, and only on the sites you set.",
  "- It stores them encrypted on this computer, so the model can't read them.",
  "This is how Øphio is designed, not a guarantee. We've taken every reasonable measure to make it secure, but no software is perfectly secure. A website, a bug, or an agent working around Øphio could still expose a password. We also can't control what an agent does once it's signed in.",
  "Agent logins is provided as is, without any guarantee. You use it at your own risk. Øphio and its contributors aren't responsible for what agents do with your logins, or for any loss that results.",
  "Only add logins you're willing to let an agent use, and keep bank and payment accounts out.",
];
const OFF = "Agent logins is off on this computer. Read its notice and turn it on first.";

/** The computer's agent logins (`crates/accounts/src/logins.rs`), as the phone sees them. */
export class AgentLoginsSim {
  view: AgentLoginsView;
  /** What the computer's store holds; never in the view. */
  readonly passwords = new Map<string, string>();
  readonly codeKeys = new Map<string, string>();
  /** The codes typed into code prompts, by request. */
  readonly codes = new Map<string, string>();
  /** The login each open request is about. */
  readonly #asks = new Map<string, string>();
  constructor(private readonly host: SimHost) {
    this.view = {
      enabled: false, accepted_version: null, disclaimer_version: DISCLAIMER_VERSION,
      disclaimer_title: "Before you give agents logins", disclaimer: DISCLAIMER,
      disclaimer_confirm: "I understand that agents act with my logins and that I use this at my own risk.",
      reminder: "Agents can act as you on these sites. You use Agent logins at your own risk.",
      paused_all: false, logins: [], history: [], store: "Secret Service", store_note: null, browser: "Chromium",
    };
  }
  query(): { kind: "agent_logins"; view: AgentLoginsView } {
    return { kind: "agent_logins", view: structuredClone(this.view) };
  }
  password(loginId: string, by = "Pixel 9"): { kind: "agent_login_password"; password: string } {
    this.#gate();
    const login = this.#login(loginId);
    this.#record(login, "shown", by);
    return { kind: "agent_login_password", password: this.passwords.get(loginId) ?? "" };
  }
  act(action: AgentLoginAction, by = "Pixel 9"): null {
    switch (action.action) {
      case "accept":
        if (action.version !== DISCLAIMER_VERSION) throw fail("invalid_request", "The notice has changed since it was shown. Read it again.");
        this.view.enabled = true;
        this.view.accepted_version = action.version;
        this.#record(null, "accepted", by);
        return null;
      case "add": {
        this.#gate();
        const password = action.password ?? "";
        if (!password) throw fail("invalid_request", "Add the login's password.");
        if (!password.trim()) throw fail("invalid_request", "A password can't be only spaces.");
        const details = this.#details(action.details, null);
        const login: AgentLogin = {
          login_id: uuidv7(), ...details, has_password: true, has_code_key: !!action.code_key,
          paused: false, status: { status: "ready" }, last_used: null, used_by: [],
        };
        this.view.logins.push(login);
        this.passwords.set(login.login_id, password);
        if (action.code_key) this.codeKeys.set(login.login_id, action.code_key);
        this.#record(login, "added", by);
        return null;
      }
      case "edit": {
        this.#gate();
        const login = this.#login(action.login_id);
        Object.assign(login, this.#details(action.details, login.login_id));
        this.#record(login, "edited", by);
        return null;
      }
      case "set_secret": {
        this.#gate();
        const login = this.#login(action.login_id);
        if (action.password === null && action.code_key === null) throw fail("invalid_request", "There is nothing to change.");
        if (action.password !== null) {
          if (!action.password.trim()) throw fail("invalid_request", "A password can't be only spaces.");
          this.passwords.set(login.login_id, action.password);
          login.has_password = true;
          this.#record(login, "password_set", by);
        }
        if (action.code_key === "") {
          this.codeKeys.delete(login.login_id);
          login.has_code_key = false;
          this.#record(login, "code_key_removed", by);
        } else if (action.code_key !== null) {
          this.codeKeys.set(login.login_id, action.code_key);
          login.has_code_key = true;
          this.#record(login, "code_key_set", by);
        }
        return null;
      }
      case "pause":
      case "resume": {
        this.#gate();
        const login = this.#login(action.login_id);
        login.paused = action.action === "pause";
        login.status = login.paused ? { status: "paused" } : { status: "ready" };
        this.#record(login, action.action === "pause" ? "paused" : "resumed", by);
        return null;
      }
      case "pause_all":
      case "resume_all":
        this.#gate();
        this.view.paused_all = action.action === "pause_all";
        this.#record(null, this.view.paused_all ? "paused_all" : "resumed_all", by);
        return null;
      case "sign_out":
        this.#record(this.#login(action.login_id), "signed_out", by);
        return null;
      case "delete": {
        const login = this.#login(action.login_id);
        this.view.logins = this.view.logins.filter((l) => l !== login);
        this.passwords.delete(login.login_id);
        this.codeKeys.delete(login.login_id);
        this.#record(login, "deleted", by);
        return null;
      }
      case "answer_code": {
        const code = action.code.replace(/[\s-]/g, "");
        if (!/^[A-Za-z0-9]{3,12}$/.test(code)) throw fail("invalid_request", "A code is 3 to 12 letters and digits.");
        const a = this.host.approvals.find((x) => x.approval_id === action.approval_id);
        if (!a || a.kind !== "login_code") throw fail("not_found", "There is no such code request.");
        if (a.state !== "pending") throw fail("expired", "The code request has ended.");
        this.codes.set(a.approval_id, code);
        this.host.settleApproval(a.approval_id, "allowed");
        const login = this.view.logins.find((l) => l.login_id === this.#asks.get(a.approval_id));
        if (login) this.#record(login, "code_given", by, a.task_id);
        this.#asks.delete(a.approval_id);
        return null;
      }
    }
  }
  /** Turns it on with two logins, as a returning owner would have it. */
  seed(): void {
    const now = Date.now();
    this.view.enabled = true;
    this.view.accepted_version = DISCLAIMER_VERSION;
    const add = (login: Omit<AgentLogin, "login_id" | "has_password" | "paused" | "status" | "used_by">, password: string): AgentLogin => {
      const full: AgentLogin = { login_id: uuidv7(), has_password: true, paused: false, status: { status: "ready" }, used_by: [], ...login };
      this.view.logins.push(full);
      this.passwords.set(full.login_id, password);
      return full;
    };
    const forum = add({ name: "Garden forum", sites: ["gardenforum.example"], username: "sam-grows", has_code_key: false,
      notes: "Post in the seed-swap board only.", who: { projects: [ids.pGarden], agents: null }, how: "whenever", notify: false, last_used: now - 3 * 3600_000 }, "tomato-Basil-41");
    add({ name: "Facebook", sites: ["facebook.com", "messenger.com"], username: "sam@example.com", has_code_key: true,
      notes: "", who: { projects: null, agents: null }, how: "ask_first", notify: true, last_used: null }, "kV7-qPz2!mWx9_nRt4Lh");
    this.view.history = [
      { at: now - 3 * 3600_000, login_id: forum.login_id, name: forum.name, event: "filled", by: "Claude Code", task_id: null, site: "gardenforum.example", detail: null },
      { at: now - 2 * 86400_000, login_id: forum.login_id, name: forum.name, event: "added", by: "Pixel 9", task_id: null, site: null, detail: null },
      { at: now - 2 * 86400_000, login_id: null, name: null, event: "accepted", by: "Pixel 9", task_id: null, site: null, detail: null },
    ];
    this.#changed();
  }
  /** An agent asks to use an "Ask me first" login (`login_use`) or for a code
   * the site sent (`login_code`), on the flaky-test task's run. */
  ask(kind: "login_use" | "login_code", loginId = this.view.logins.find((l) => l.how === "ask_first")?.login_id ?? this.view.logins[0]?.login_id): string {
    const login = this.#login(loginId ?? "");
    const task = this.host.tasks.find((t) => t.task_id === ids.tFlaky)!;
    const site = login.sites[0] ?? "";
    const id = uuidv7();
    const now = Date.now();
    const use = kind === "login_use";
    const approval: Approval = {
      approval_id: id, run_id: ids.rFlaky, task_id: task.task_id, project_id: task.project_id, native_request_id: `login-${id}`, kind,
      title: use ? `Let “${task.title}” sign in to ${login.name} on ${site}?` : `${login.name}: the site asked for a code it sent you`,
      detail: use
        ? `Claude Code wants to use the login ${login.name} on ${login.sites.join(", ")}. If you allow it, Ophio fills it for this task until the task's turn ends; the agent never sees the password.`
        : `“${task.title}” is signing in to ${login.name} and needs the code the site sent you. Type it here: Ophio fills it in, and the agent never sees it.`,
      raw_request: null, scope: use ? "This task" : "This code once", revision: 1, state: "pending",
      requested_at: now, expires_at: null, resolved_by: null, resolved_at: null,
    };
    const item: AttentionItem = {
      attention_id: uuidv7(), kind: "approval", title: approval.title, body: approval.detail, task_id: task.task_id, run_id: ids.rFlaky,
      approval_id: id, created_at: now, resolved_at: null, priority: 10,
    };
    this.#asks.set(id, login.login_id);
    if (!use) this.#record(login, "code_asked", "Claude Code", task.task_id, site);
    this.host.ask(approval, item);
    return id;
  }
  /** The owner's answer to a `login_use` request reached the computer. */
  decided(a: Approval, allowed: boolean): void {
    const login = this.view.logins.find((l) => l.login_id === this.#asks.get(a.approval_id));
    this.#asks.delete(a.approval_id);
    if (!login) return;
    this.#record(login, allowed ? "approved" : "refused", "Pixel 9", a.task_id);
    if (allowed) this.use(login.login_id, a.task_id);
  }
  /** The computer filled the login for a task's agent. */
  use(loginId: string, taskId: string): void {
    const login = this.#login(loginId);
    const site = login.sites[0] ?? "";
    login.last_used = Date.now();
    if (!login.used_by.includes(taskId)) login.used_by.push(taskId);
    this.#record(login, "filled", "Claude Code", taskId, site);
    this.host.emit({ type: "agent_login_used", login_id: login.login_id, name: login.name, task_id: taskId, site, notify: login.notify }, { kind: "companion" }, "observed");
  }
  #details(d: LoginDetails, self: string | null): Omit<LoginDetails, never> {
    const name = d.name.trim();
    if (!name || name.length > 60 || /[\n\r]/.test(name)) throw fail("invalid_request", "A login's name is 1 to 60 characters on one line.");
    if (this.view.logins.some((l) => l.login_id !== self && l.name.toLowerCase() === name.toLowerCase())) throw fail("invalid_request", `There is already a login called ${name}.`);
    const username = d.username.trim();
    if (!username || username.length > 200 || /[\n\r]/.test(username)) throw fail("invalid_request", "A username is 1 to 200 characters on one line.");
    if (d.notes.length > 2000) throw fail("invalid_request", "Notes are at most 2,000 characters.");
    if (d.sites.length === 0 || d.sites.length > 10) throw fail("invalid_request", "A login is for 1 to 10 sites.");
    const sites = [...new Set(d.sites.map(site))];
    if (d.who.projects?.length === 0) throw fail("invalid_request", "Choose at least one project, or let every project use it.");
    if (d.who.agents?.length === 0) throw fail("invalid_request", "Choose at least one agent, or let every agent use it.");
    return { ...d, name, username, sites };
  }
  #gate(): void {
    if (!this.view.enabled) throw fail("permission_denied", OFF);
  }
  #login(id: string): AgentLogin {
    const login = this.view.logins.find((l) => l.login_id === id);
    if (!login) throw fail("not_found", "There is no such login.");
    return login;
  }
  #record(login: AgentLogin | null, event: AgentLoginEvent, by: string, taskId: string | null = null, site: string | null = null, detail: string | null = null): void {
    this.view.history.unshift({ at: Date.now(), login_id: login?.login_id ?? null, name: login?.name ?? null, event, by, task_id: taskId, site, detail });
    this.view.history.splice(500);
    this.#changed();
  }
  #changed(): void {
    this.host.emit({ type: "agent_logins_changed" }, { kind: "companion" }, "observed");
  }
}

/** The host a typed site is on, as `logins::site` reduces it. */
function site(text: string): string {
  const shown = text.trim().slice(0, 80);
  let url: URL;
  try { url = new URL(shown.includes("://") ? shown : `https://${shown}`); } catch { throw fail("invalid_request", `${shown} isn't a web address.`); }
  if (url.protocol !== "https:" && url.protocol !== "http:") throw fail("invalid_request", `${shown} isn't a web address (https://…).`);
  if (url.username || url.password) throw fail("invalid_request", "Leave the username and password out of the site's address.");
  const host = url.hostname.replace(/\.$/, "").replace(/^www\./, "");
  if (/^[\d.]+$/.test(host) || host.startsWith("[")) throw fail("invalid_request", `${shown} is an IP address. Use the site's name.`);
  if (host.split(".").length < 2 || host.split(".").some((l) => !l)) throw fail("invalid_request", `${shown} isn't a whole site name, such as example.com.`);
  return host;
}
