// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useMemo, useState } from "react";
import { Eye, EyeOff, Globe, KeyRound, Pause, Pencil, Play, Plus, ShieldAlert, WandSparkles } from "lucide-react";
import { shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { LAUNCHERS_FEATURE, useCatalog } from "../../app/launchers";
import { PRODUCT_NAME } from "../../brand";
import { Age } from "../../design/Age";
import { Button, IconButton } from "../../design/Button";
import { Pill, Segmented, type Tone } from "../../design/Chip";
import { TextArea, TextField } from "../../design/Field";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import type { AgentLogin, AgentLoginAction, AgentLoginsView, LoginDetails, LoginHow, LoginStatus, LoginWho } from "../../protocol/types";
import { NotAllowed } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { useWarning } from "../../app/warnings";
import { confirmHere, useConfirmable } from "./useAccounts";
import { EVENT_WORDS, HOW_WORDS, SHOW_MS, STATUS_WORDS, generatePassword, splitSites, useAgentLogins } from "./useAgentLogins";
import "./accounts.css";
import "./agentLogins.css";

const TONE: Record<LoginStatus["status"], Tone> = { ready: "ok", paused: "neutral", needs_attention: "warn" };
const EXPLAIN = `Accounts are what ${PRODUCT_NAME}’s tools sign in as. Agent logins are usernames and passwords agents can use on websites, in ${PRODUCT_NAME}’s own browser.`;
const PAUSE_WORDS = "Pause stops agents filling this login and opening its sites. Tasks keep running.";
const PAUSE_ALL_WORDS = "Pause all stops agents filling any login and opening their sites. Tasks keep running.";
const HOW_HINT: Record<LoginHow, string> = {
  whenever: "Tasks it’s for can use it without asking.",
  ask_first: "The first time a task needs it, this phone asks you, once per task.",
};

type Act = (action: AgentLoginAction, label: string, confirm?: string) => Promise<boolean>;

export default function AgentLogins(_: RouteProps<"agent_logins">) {
  const { profile, platform } = useApp();
  const link = useLink();
  const online = link.state === "online";
  const logins = useAgentLogins();
  const confirmable = useConfirmable();
  const [open, setOpen] = useState<string | null>(null);
  const [form, setForm] = useState<AgentLogin | "add" | null>(null);
  const [reading, setReading] = useState(false);
  const [notNow, setNotNow] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const view = logins.view;
  const act: Act = async (action, label, confirm) => {
    setBusy(label);
    try {
      if (confirm) await confirmHere(platform, confirm);
      await logins.act(action, label);
      return true;
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
      return false;
    } finally { setBusy(null); }
  };
  // Until it is turned on, opening Agent logins opens the disclaimer.
  const asking = !!view && (reading || (!view.enabled && !notNow));
  const selected = view?.logins.find((l) => l.login_id === open) ?? null;
  const pauseAll = view?.paused_all
    ? { action: { action: "resume_all" } as const, label: "Resuming all agent logins", done: "Agent logins resumed", words: "Resume all", icon: Play }
    : { action: { action: "pause_all" } as const, label: "Pausing all agent logins", done: "All agent logins paused", words: "Pause all", icon: Pause };
  return (
    <Screen title="Agent logins" subtitle={profile.name} className="settings-screen agent-logins-screen">
      <div className="settings-stack">
        {!logins.supported ? (
          <Notice tone="warn" title="Update the computer">
            {profile.name} doesn’t keep logins for agents yet. Update {PRODUCT_NAME} on it to add them from here.
          </Notice>
        ) : !logins.allowed ? (
          <NotAllowed what="manage agent logins" permission="accounts" computerOnly
            detail="Agent logins holds passwords agents use on websites." />
        ) : <>
          {logins.error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={logins.reload}>Try again</Button>}>{logins.error}</Notice>}
          {!view && !logins.error && <div className="screen__loading" aria-busy="true" aria-label="Loading agent logins" />}
          {view && <>
            <Reminder view={view} onRead={() => setReading(true)} />
            <p className="settings-meta">{EXPLAIN}</p>
            {!view.enabled ? (
              <article className="settings-card settings-stack" aria-labelledby="agent-logins-off">
                <h3 id="agent-logins-off">Agent logins is off on {profile.name}</h3>
                <p>
                  {view.accepted_version != null
                    ? "The disclaimer changed since it was turned on. Read it and turn Agent logins on again; until then agents can’t use any login."
                    : "Read the disclaimer and turn it on to add logins agents can use."}
                </p>
                <Button variant="primary" disabled={!online} onClick={() => setReading(true)}>Read and turn on</Button>
              </article>
            ) : <>
              <Button block variant={view.paused_all ? "secondary" : "warn"} icon={pauseAll.icon}
                disabled={!online || !!busy || (!view.paused_all && !view.logins.length)} pending={busy === pauseAll.label}
                onClick={() => void act(pauseAll.action, pauseAll.label).then((ok) => ok && toast(pauseAll.done))}>
                {pauseAll.words}
              </Button>
              <p className="settings-meta">{view.paused_all ? "Every login is paused. Resume all lets agents use them again, as each one’s rules allow." : PAUSE_ALL_WORDS}</p>
            </>}
            {view.store_note && <Notice tone="warn" title={`Passwords are kept in ${view.store}`}>{view.store_note}</Notice>}
            {view.enabled && !view.browser && (
              <Notice tone="unknown" title="No browser for agents">
                Agents sign in only in {PRODUCT_NAME}’s own browser, which needs Chromium, Chrome, Edge or Brave on {profile.name}. Install one there; logins can be added now.
              </Notice>
            )}
            {confirmable === false && <Notice tone="unknown">Set a secure screen lock in Android settings to add, change, show or delete logins from this phone. Each asks for it first.</Notice>}
            {(view.enabled || view.logins.length > 0) && (
              <Section title="On this computer">
                {view.logins.length ? (
                  <div className="group">
                    {view.logins.map((l) => (
                      <ListRow key={l.login_id} icon={KeyRound} chevron onClick={() => setOpen(l.login_id)}
                        title={l.name}
                        subtitle={[l.sites[0], l.username].filter(Boolean).join(" · ")}
                        meta={<span className="account-row__meta">
                          <Pill tone={TONE[l.status.status]}>{STATUS_WORDS[l.status.status]}</Pill>
                          <span className="settings-meta"><WhoWords who={l.who} /></span>
                          <span className="settings-meta">{l.last_used != null ? <>Used <Age at={l.last_used} suffix=" ago" /></> : "Not used yet"}</span>
                        </span>} />
                    ))}
                  </div>
                ) : <Empty icon={KeyRound} title="No logins yet">Add one for each website agents may sign in to.</Empty>}
                {view.enabled && <Button icon={Plus} disabled={!online} onClick={() => setForm("add")}>Add a login</Button>}
              </Section>
            )}
            <History view={view} />
            <p className="settings-meta">
              Passwords stay on {profile.name}, in {view.store}. {PRODUCT_NAME} types them into the page itself; agents and this phone’s task screens never see one.
            </p>
          </>}
        </>}
      </div>
      {view && (
        <Disclaimer view={view} open={asking} online={online} busy={busy}
          onClose={() => { setReading(false); setNotNow(true); }}
          onAccept={async () => {
            if (!(await act({ action: "accept", version: view.disclaimer_version }, "Turning on Agent logins", "Turn on Agent logins"))) return;
            setReading(false);
            toast("Agent logins turned on");
          }} />
      )}
      {selected && view && (
        <LoginSheet login={selected} view={view} online={online} busy={busy} act={act} reveal={logins.password}
          onEdit={() => { setOpen(null); setForm(selected); }} onClose={() => setOpen(null)} />
      )}
      {form && view && (
        <LoginForm login={form === "add" ? null : form} view={view} online={online} busy={busy} act={act}
          onRead={() => setReading(true)} onClose={() => setForm(null)} />
      )}
    </Screen>
  );
}

function Reminder({ view, onRead }: { view: AgentLoginsView; onRead: () => void }) {
  return (
    <Notice tone="warn" icon={ShieldAlert} action={<Button size="sm" variant="quiet" onClick={onRead}>Read the disclaimer</Button>}>
      {view.reminder}
    </Notice>
  );
}

/** The disclaimer's paragraphs, with its "- " lines gathered into lists. */
function DisclaimerText({ lines }: { lines: string[] }) {
  const blocks: (string | string[])[] = [];
  for (const line of lines) {
    if (line.startsWith("- ")) {
      const last = blocks[blocks.length - 1];
      if (Array.isArray(last)) last.push(line.slice(2));
      else blocks.push([line.slice(2)]);
    } else blocks.push(line);
  }
  return <>{blocks.map((b, i) => typeof b === "string"
    ? <p key={i}>{b}</p>
    : <ul key={i}>{b.map((item, j) => <li key={j}>{item}</li>)}</ul>)}</>;
}

function Disclaimer({ view, open, online, busy, onClose, onAccept }: {
  view: AgentLoginsView; open: boolean; online: boolean; busy: string | null; onClose: () => void; onAccept: () => Promise<void>;
}) {
  const [ticked, setTicked] = useState(false);
  useEffect(() => { if (!open) setTicked(false); }, [open]);
  const footer = view.enabled
    ? <Button variant="primary" onClick={onClose}>Done</Button>
    : <>
      <Button variant="quiet" onClick={onClose}>Not now</Button>
      <Button variant="primary" disabled={!online || !ticked || !!busy} pending={busy === "Turning on Agent logins"} onClick={() => void onAccept()}>
        Turn on Agent logins
      </Button>
    </>;
  return (
    <Sheet open={open} onClose={onClose} title={view.disclaimer_title} size="tall" footer={footer}>
      <div className="settings-stack agent-logins-disclaimer">
        {view.accepted_version != null && view.accepted_version < view.disclaimer_version && (
          <Notice tone="unknown">This wording is new since Agent logins was turned on here, so it asks again.</Notice>
        )}
        <DisclaimerText lines={view.disclaimer} />
        {!view.enabled && (
          <label className="settings-check">
            <input type="checkbox" checked={ticked} onChange={(e) => setTicked(e.target.checked)} />
            {view.disclaimer_confirm}
          </label>
        )}
      </div>
    </Sheet>
  );
}

/** "Every project · Any agent", or which. */
function WhoWords({ who }: { who: LoginWho }) {
  const projects = useHost((s) => s.projects);
  const p = who.projects === null ? "Every project"
    : who.projects.length === 1 ? (projects[who.projects[0]!]?.name ?? "1 project") : `${who.projects.length} projects`;
  const a = who.agents === null ? null : who.agents.length === 1 ? who.agents[0]! : `${who.agents.length} agents`;
  return <>{a ? `${p} · ${a}` : p}</>;
}

/** A password or key field, hidden until its eye is pressed. */
function SecretField({ label, value, onChange, autoComplete, name, hint, generate, autoFocus }: {
  label: string; value: string; onChange: (v: string) => void; autoComplete: string; name: string; hint?: string;
  generate?: boolean; autoFocus?: boolean;
}) {
  const id = useId();
  const [visible, setVisible] = useState(false);
  return (
    <div className="field agent-login-secret">
      <label htmlFor={id} className="field__label">{label}</label>
      <div className="agent-login-secret__row">
        <input id={id} className="field__input" type={visible ? "text" : "password"} value={value} onChange={(e) => onChange(e.target.value)}
          autoFocus={autoFocus} autoComplete={autoComplete} name={name} spellCheck={false} autoCapitalize="off" autoCorrect="off"
          aria-describedby={hint ? `${id}-h` : undefined} />
        <IconButton icon={visible ? EyeOff : Eye} label={visible ? `Hide ${label.toLowerCase()}` : `Show ${label.toLowerCase()}`}
          pressed={visible} onClick={() => setVisible((v) => !v)} />
      </div>
      {hint && <p className="field__hint" id={`${id}-h`}>{hint}</p>}
      {generate && (
        <Button size="sm" variant="quiet" icon={WandSparkles} onClick={() => { onChange(generatePassword()); setVisible(true); }}>
          Generate
        </Button>
      )}
    </div>
  );
}

interface Draft {
  name: string;
  sites: string;
  username: string;
  notes: string;
  who: LoginWho;
  how: LoginHow;
  notify: boolean;
  password: string;
  codeKey: string;
}

function draftOf(l: AgentLogin | null): Draft {
  return l
    ? { name: l.name, sites: l.sites.join("\n"), username: l.username, notes: l.notes, who: l.who, how: l.how, notify: l.notify, password: "", codeKey: "" }
    : { name: "", sites: "", username: "", notes: "", who: { projects: null, agents: null }, how: "whenever", notify: false, password: "", codeKey: "" };
}

/** What's missing before the computer is asked, in words; the computer checks the rest. */
function problem(d: Draft, adding: boolean): string | null {
  if (!d.name.trim()) return "Give the login a name.";
  if (!splitSites(d.sites).length) return "Add the site it’s for.";
  if (!d.username.trim()) return "Add the username.";
  if (adding && !d.password) return "Add the password, or generate one.";
  if (d.who.projects?.length === 0) return "Choose at least one project, or every project.";
  if (d.who.agents?.length === 0) return "Choose at least one agent, or any agent.";
  return null;
}

function LoginForm({ login, view, online, busy, act, onRead, onClose }: {
  login: AgentLogin | null; view: AgentLoginsView; online: boolean; busy: string | null; act: Act; onRead: () => void; onClose: () => void;
}) {
  const { profile } = useApp();
  const [d, setD] = useState(() => draftOf(login));
  const [tried, setTried] = useState(false);
  const set = <K extends keyof Draft>(k: K) => (v: Draft[K]) => setD((x) => ({ ...x, [k]: v }));
  const adding = !login;
  const label = adding ? `Adding ${d.name.trim() || "the login"}` : `Saving ${login.name}`;
  const wrong = problem(d, adding);
  const close = onClose;
  const save = async () => {
    setTried(true);
    if (wrong) return;
    const details: LoginDetails = {
      name: d.name.trim(), sites: splitSites(d.sites), username: d.username.trim(), notes: d.notes.trim(),
      who: d.who, how: d.how, notify: d.notify,
    };
    // The typed secrets leave the form as they're sent.
    const password = d.password;
    const codeKey = d.codeKey.trim();
    setD((x) => ({ ...x, password: "", codeKey: "" }));
    const ok = adding
      ? await act({ action: "add", details, password, code_key: codeKey || null }, label, `Add ${details.name}`)
      : await act({ action: "edit", login_id: login.login_id, details }, label, `Save ${details.name}`);
    if (ok) { toast(adding ? `${details.name} added` : `${details.name} saved`); close(); }
    else setD((x) => ({ ...x, password, codeKey }));
  };
  const footer = <>
    <Button variant="quiet" onClick={close}>Cancel</Button>
    <Button variant="primary" type="submit" form="agent-login-form" disabled={!online || !!busy} pending={busy === label}>
      {adding ? "Add login" : "Save"}
    </Button>
  </>;
  return (
    <Sheet open onClose={close} title={adding ? "Add a login" : `Edit ${login.name}`} size="tall" footer={footer}>
      <form id="agent-login-form" className="settings-stack" onSubmit={(e) => { e.preventDefault(); void save(); }}>
        {adding && <Reminder view={view} onRead={onRead} />}
        <TextField label="Name" value={d.name} onChange={set("name")} placeholder="Facebook" autoComplete="off" maxLength={100} />
        <TextArea label="Sites" value={d.sites} onChange={set("sites")} placeholder="facebook.com" rows={1} maxRows={5}
          autoComplete="off" autoCapitalize="off" spellCheck={false}
          hint="The main site first, then any others the login uses, one per line. It’s filled on these and their subdomains only." />
        <TextField label="Username" value={d.username} onChange={set("username")} autoComplete="username" name="username"
          autoCapitalize="off" spellCheck={false} maxLength={200} />
        {adding && <>
          <SecretField label="Password" value={d.password} onChange={set("password")} autoComplete="new-password" name="password" generate
            hint={`Sent once to ${profile.name} and kept in ${view.store}. A generated one is shown so you can save it in your own password manager too.`} />
          <SecretField label="2FA setup key (optional)" value={d.codeKey} onChange={set("codeKey")} autoComplete="off" name="code-key"
            hint={`For sites that use an authenticator app: the setup key or its otpauth:// link. ${PRODUCT_NAME} then makes the codes, so it holds both factors; leave it out for your important accounts and the agent will ask you.`} />
        </>}
        <TextArea label="Notes for the agent" value={d.notes} onChange={set("notes")} rows={2} maxRows={6} maxLength={2000}
          placeholder="Use the business page; never post publicly without asking"
          hint="Not secret: the agent reads these." />
        <WhoFields who={d.who} onChange={set("who")} />
        <fieldset className="settings-stack">
          <legend className="field__label">How</legend>
          <Segmented label="How" value={d.how} onChange={set("how")}
            options={(["whenever", "ask_first"] as const).map((v) => ({ value: v, label: HOW_WORDS[v] }))} />
          <p className="settings-meta">{HOW_HINT[d.how]}</p>
        </fieldset>
        <label className="settings-check">
          <input type="checkbox" checked={d.notify} onChange={(e) => set("notify")(e.target.checked)} />
          Tell me each time it’s used
        </label>
        {tried && wrong && <p className="field__error" role="alert">{wrong}</p>}
      </form>
    </Sheet>
  );
}

function WhoFields({ who, onChange }: { who: LoginWho; onChange: (who: LoginWho) => void }) {
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  const launchers = useHost((s) => !!s.capabilities?.features.includes(LAUNCHERS_FEATURE));
  const { catalog } = useCatalog(null, launchers);
  const agents = useMemo(() => {
    const names = new Map<string, string>();
    for (const e of catalog?.entries ?? []) if (e.kind === "agent" && !names.has(e.id)) names.set(e.id, e.name);
    // Agents chosen before stay listed though the catalog no longer has them.
    for (const id of who.agents ?? []) if (!names.has(id)) names.set(id, id);
    return [...names].sort((a, b) => a[1].localeCompare(b[1]));
  }, [catalog, who.agents]);
  const sorted = [...projects].sort((a, b) => a.name.localeCompare(b.name));
  const toggle = (list: string[], id: string, on: boolean) => on ? [...list, id] : list.filter((x) => x !== id);
  return (
    <fieldset className="settings-stack">
      <legend className="field__label">Who can use it</legend>
      <Segmented label="Projects" value={who.projects === null ? "all" : "some"}
        onChange={(v) => onChange({ ...who, projects: v === "all" ? null : (who.projects ?? []) })}
        options={[{ value: "all", label: "Every project" }, { value: "some", label: "Only these projects" }]} />
      {who.projects !== null && (
        <div className="agent-login-choices">
          {sorted.map((p) => (
            <label key={p.project_id} className="settings-check">
              <input type="checkbox" checked={who.projects!.includes(p.project_id)}
                onChange={(e) => onChange({ ...who, projects: toggle(who.projects!, p.project_id, e.target.checked) })} />
              {p.name}
            </label>
          ))}
        </div>
      )}
      {(launchers || who.agents !== null) && <>
        <Segmented label="Agents" value={who.agents === null ? "any" : "some"}
          onChange={(v) => onChange({ ...who, agents: v === "any" ? null : (who.agents ?? []) })}
          options={[{ value: "any", label: "Any agent" }, { value: "some", label: "Only these agents", disabled: !agents.length }]} />
        {who.agents !== null && (
          <div className="agent-login-choices">
            {agents.map(([id, name]) => (
              <label key={id} className="settings-check">
                <input type="checkbox" checked={who.agents!.includes(id)}
                  onChange={(e) => onChange({ ...who, agents: toggle(who.agents!, id, e.target.checked) })} />
                {name}
              </label>
            ))}
          </div>
        )}
        <p className="settings-meta">Only these agents means only tasks started from one of them in Agents &amp; skills.</p>
      </>}
    </fieldset>
  );
}

type Mode = null | "password" | "code" | "deleting";

function LoginSheet({ login: l, view, online, busy, act, reveal, onEdit, onClose }: {
  login: AgentLogin; view: AgentLoginsView; online: boolean; busy: string | null; act: Act;
  reveal: (loginId: string) => Promise<string>; onEdit: () => void; onClose: () => void;
}) {
  const { profile, platform } = useApp();
  const tasks = useHost((s) => s.tasks);
  const [mode, setMode] = useState<Mode>(null);
  const [secret, setSecret] = useState("");
  const [shown, setShown] = useState<string | null>(null);
  const [showing, setShowing] = useState(false);
  // A shown password hides itself again.
  useEffect(() => {
    if (shown === null) return;
    const t = setTimeout(() => setShown(null), SHOW_MS);
    return () => clearTimeout(t);
  }, [shown]);
  const back = () => { setSecret(""); setMode(null); };
  const close = () => { setShown(null); back(); onClose(); };
  const warn = useWarning("login.delete");
  const deleteNow = () => void act({ action: "delete", login_id: l.login_id }, `Deleting ${l.name}`, `Delete ${l.name}`)
    .then((ok) => { if (ok) { toast(`${l.name} deleted`); close(); } });
  const disabled = !online || !!busy;
  const on = view.enabled;
  const show = async () => {
    setShowing(true);
    try {
      await confirmHere(platform, `Show the password for ${l.name}`);
      setShown(await reveal(l.login_id));
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
    } finally { setShowing(false); }
  };
  const saveSecret = async (remove = false) => {
    const sent = remove ? "" : secret;
    setSecret("");
    const action: AgentLoginAction = mode === "password"
      ? { action: "set_secret", login_id: l.login_id, password: sent, code_key: null }
      : { action: "set_secret", login_id: l.login_id, password: null, code_key: sent };
    const what = mode === "password" ? "password" : "2FA key";
    const verb = remove ? `Removing the 2FA key for ${l.name}` : `Saving the ${what} for ${l.name}`;
    if (await act(action, verb, remove ? `Remove the 2FA key for ${l.name}` : `Save the ${what} for ${l.name}`)) {
      toast(remove ? `2FA key removed for ${l.name}` : `${what === "password" ? "Password" : "2FA key"} saved for ${l.name}`);
      back();
    } else setSecret(sent);
  };
  const footer = mode === "deleting" ? <>
    <Button onClick={back}>Keep it</Button>
    <Button variant="stop" disabled={disabled} pending={busy === `Deleting ${l.name}`}
      onClick={() => { warn.agreed(); deleteNow(); }}>
      Delete permanently
    </Button>
  </> : mode ? <>
    <Button variant="quiet" onClick={back}>Cancel</Button>
    <Button variant="primary" type="submit" form="agent-login-secret" disabled={disabled || !secret.trim()}
      pending={busy === `Saving the ${mode === "password" ? "password" : "2FA key"} for ${l.name}`}>Save</Button>
  </> : undefined;
  return (
    <Sheet open onClose={close} title={l.name} subtitle={l.username} footer={footer} size={mode ? "auto" : "tall"}>
      <div className="settings-stack">
        <p className="account-status">
          <Pill tone={TONE[l.status.status]}>{STATUS_WORDS[l.status.status]}</Pill>
          <span>{HOW_WORDS[l.how]}</span>
          {l.has_code_key && <Pill>2FA key saved</Pill>}
        </p>
        {l.status.status === "needs_attention" && <Notice tone="warn">{l.status.reason}</Notice>}
        {mode === "deleting" ? <>
          <Notice tone="warn" title={`Delete ${l.name}?`}>
            This removes its password{l.has_code_key ? " and 2FA key" : ""} from {view.store} and signs the agent browser out of {l.sites.join(", ")} on {profile.name}.
          </Notice>
          {warn.check}
        </> : mode ? (
          <form id="agent-login-secret" className="settings-stack" onSubmit={(e) => { e.preventDefault(); if (secret.trim()) void saveSecret(); }}>
            {mode === "password"
              ? <SecretField label="New password" value={secret} onChange={setSecret} autoComplete="new-password" name="password" generate autoFocus
                hint={`Sent once to ${profile.name} and kept in ${view.store}. Change it on the site too, or the agent can’t sign in.`} />
              : <SecretField label="2FA setup key" value={secret} onChange={setSecret} autoComplete="off" name="code-key" autoFocus
                hint="The setup key the site shows for an authenticator app, or its otpauth:// link." />}
            {mode === "code" && l.has_code_key && (
              <Button block variant="stop-outline" disabled={disabled} pending={busy === `Removing the 2FA key for ${l.name}`} onClick={() => void saveSecret(true)}>
                Remove the 2FA key
              </Button>
            )}
          </form>
        ) : <>
          <section aria-label="Sites" className="agent-login-sites">
            {l.sites.map((s) => <span key={s} className="agent-login-site"><Globe aria-hidden size={16} />{s}</span>)}
          </section>
          {l.notes && <p className="agent-login-notes">{l.notes}</p>}
          <p className="settings-meta">
            <WhoWords who={l.who} />{l.notify ? " · Tells you each time it’s used" : ""}
          </p>
          {shown !== null ? (
            <div className="agent-login-shown" role="group" aria-label={`Password for ${l.name}`}>
              <code className="agent-login-shown__text">{shown}</code>
              <p className="settings-meta">Hidden again after {SHOW_MS / 1000} seconds.</p>
              <Button size="sm" icon={EyeOff} onClick={() => setShown(null)}>Hide</Button>
            </div>
          ) : on && l.has_password && (
            <Button block icon={Eye} disabled={!online || showing} pending={showing} onClick={() => void show()}>Show password</Button>
          )}
          {on && <>
            <Button block icon={Pencil} disabled={disabled} onClick={onEdit}>Edit</Button>
            <Button block icon={KeyRound} disabled={disabled} onClick={() => setMode("password")}>Replace password</Button>
            <Button block disabled={disabled} onClick={() => setMode("code")}>{l.has_code_key ? "Replace or remove the 2FA key" : "Add a 2FA key"}</Button>
            {l.paused ? (
              <Button block icon={Play} disabled={disabled} pending={busy === `Resuming ${l.name}`}
                onClick={() => void act({ action: "resume", login_id: l.login_id }, `Resuming ${l.name}`)}>Resume</Button>
            ) : <>
              <Button block icon={Pause} disabled={disabled} pending={busy === `Pausing ${l.name}`}
                onClick={() => void act({ action: "pause", login_id: l.login_id }, `Pausing ${l.name}`)}>Pause</Button>
              <p className="settings-meta">{PAUSE_WORDS}</p>
            </>}
          </>}
          <Button block variant="stop-outline" disabled={disabled} pending={busy === `Signing the agent browser out of ${l.name}`}
            onClick={() => void act({ action: "sign_out", login_id: l.login_id }, `Signing the agent browser out of ${l.name}`, `Sign the agent browser out of ${l.name}`)
              .then((ok) => ok && toast(`Signed out of ${l.sites[0] ?? l.name}`))}>
            Sign the agent browser out
          </Button>
          <p className="settings-meta">Clears the agent browser’s cookies and data for {l.sites.join(", ")}. The next task signs in again.</p>
          <Button block variant="stop-outline" disabled={disabled} pending={busy === `Deleting ${l.name}`}
            onClick={() => warn.ask(() => setMode("deleting"), deleteNow)}>Delete permanently</Button>
          <section aria-label="Used by">
            <h3 className="settings-meta">Used by</h3>
            {l.used_by.length ? <ul className="account-tasks">{l.used_by.map((id) => <li key={id}>{tasks[id]?.title ?? "A task"}</li>)}</ul>
              : <p className="settings-meta">No running task uses it now.</p>}
          </section>
          <History view={view} loginId={l.login_id} />
        </>}
      </div>
    </Sheet>
  );
}

function History({ view, loginId }: { view: AgentLoginsView; loginId?: string }) {
  const tasks = useHost((s) => s.tasks);
  const entries = loginId ? view.history.filter((h) => h.login_id === loginId) : view.history;
  if (!entries.length) return loginId ? <p className="settings-meta">No history yet.</p> : <Empty title="No agent login history yet" />;
  return (
    <details className="account-history">
      <summary>History</summary>
      <ul>
        {entries.map((h, i) => (
          <li key={`${h.at}-${i}`}>
            <strong>{EVENT_WORDS[h.event]}</strong>
            {!loginId && h.name ? ` · ${h.name}` : ""}{h.site ? ` · ${h.site}` : ""}{h.detail ? ` · ${h.detail}` : ""}
            <p className="settings-meta">
              {h.by}{h.task_id ? ` · ${tasks[h.task_id]?.title ?? "a task"}` : ""} · <Age at={h.at} suffix=" ago" />
            </p>
          </li>
        ))}
      </ul>
    </details>
  );
}
