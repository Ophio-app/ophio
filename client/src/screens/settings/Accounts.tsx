// SPDX-License-Identifier: Apache-2.0
import { useRef, useState } from "react";
import { FileJson, KeyRound, Monitor, Pause, Play, Plus, ShieldCheck } from "lucide-react";
import { useApp, useHost, useLink } from "../../app/context";
import { PRODUCT_NAME, PROGRAM_NAME } from "../../brand";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Pill, type Tone } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { InlineCode } from "../../design/InlineCode";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import type { Account, AccountAction, AccountsView, PasswordManager, SignIn } from "../../protocol/types";
import { NotAllowed } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { useWarning } from "../../app/warnings";
import { AddService } from "./AddService";
import { EVENT_WORDS, KIND_WORDS, MANAGERS, STATE_WORDS, confirmHere, finished, useAccounts, useConfirmable } from "./useAccounts";
import "./accounts.css";

const TONE: Record<Account["state"]["state"], Tone> = { active: "ok", paused: "neutral", signed_out: "neutral", needs_attention: "warn" };
const PAUSE_WORDS = `Pause stops this account being given to ${PRODUCT_NAME}’s new tasks. Tasks already running keep what they were given. Programs you run yourself aren’t affected.`;
const PAUSE_ALL_WORDS = `Pause all stops any account being given to ${PRODUCT_NAME}’s new tasks. Tasks already running keep what they were given. Programs you run yourself aren’t affected.`;

type Act = (action: AccountAction, label: string, confirm?: string) => Promise<boolean>;

export default function Accounts(_: RouteProps<"accounts">) {
  const { profile, platform, ui } = useApp();
  const link = useLink();
  const online = link.state === "online";
  const accounts = useAccounts();
  const confirmable = useConfirmable();
  const target = useHost((s) => s.targets[0]?.target_id ?? null);
  const [open, setOpen] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const view = accounts.view;
  const act: Act = async (action, label, confirm) => {
    setBusy(label);
    try {
      if (confirm) await confirmHere(platform, confirm);
      await accounts.act(action, label);
      return true;
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
      return false;
    } finally { setBusy(null); }
  };
  const watch = () => { if (target) ui.push({ screen: "desktop", targetId: target }, "control"); };
  const signIn = async (a: Account) => {
    if (await act({ action: "sign_in", account_id: a.account_id }, `Signing in to ${a.service}`, `Sign in to ${a.service}`) && a.browser_sign_in) {
      setOpen(null);
      watch();
    }
  };
  const selected = view?.accounts.find((a) => a.account_id === open) ?? null;
  const anyActive = !!view?.accounts.some((a) => a.state.state === "active" || a.state.state === "needs_attention");
  return (
    <Screen title="Accounts" subtitle={profile.name} className="settings-screen accounts-screen">
      <div className="settings-stack">
        {!accounts.supported ? (
          <Notice tone="warn" title="Update the computer">
            {profile.name} doesn’t manage accounts yet. Update {PRODUCT_NAME} on it to sign in from here.
          </Notice>
        ) : !accounts.allowed ? (
          <NotAllowed what="manage accounts" permission="accounts" computerOnly
            detail="Accounts holds keys and sign-ins for every task on the computer." />
        ) : <>
          {accounts.error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={accounts.reload}>Try again</Button>}>{accounts.error}</Notice>}
          {!view && !accounts.error && <div className="screen__loading" aria-busy="true" aria-label="Loading accounts" />}
          {view && <>
            <Button block variant="warn" icon={Pause} disabled={!online || !anyActive || !!busy}
              pending={busy === "Pausing all accounts"}
              onClick={() => void act({ action: "pause_all" }, "Pausing all accounts").then((ok) => ok && toast("All accounts paused"))}>
              Pause all
            </Button>
            <p className="settings-meta">{PAUSE_ALL_WORDS}</p>
            {view.store_note && <Notice tone="warn" title={`Keys are kept in ${view.store}`}>{view.store_note}</Notice>}
            {confirmable === false && <Notice tone="unknown">Set a secure screen lock in Android settings to sign in, set keys or remove accounts from this phone. Each asks for it first.</Notice>}
            {view.sign_in && !finished(view.sign_in.stage) && (
              <SignInNow signIn={view.sign_in} onOpen={target ? watch : undefined} />
            )}
            <Section title="On this computer">
              <div className="group">
                {view.accounts.map((a) => (
                  <ListRow key={a.account_id} icon={KeyRound} chevron onClick={() => setOpen(a.account_id)}
                    title={a.service}
                    subtitle={a.note && a.state.state === "active" ? `${a.identity ?? KIND_WORDS[a.kind]} · missing a permission` : a.identity ?? KIND_WORDS[a.kind]}
                    meta={<span className="account-row__meta">
                      <Pill tone={TONE[a.state.state]}>{STATE_WORDS[a.state.state]}</Pill>
                      {a.checked_at != null && <span className="settings-meta">Checked <Age at={a.checked_at} suffix=" ago" /></span>}
                    </span>} />
                ))}
              </div>
              <Button icon={Plus} disabled={!online} onClick={() => setAdding(true)}>Add a service</Button>
            </Section>
            {!view.password_manager_done && <PasswordManagerCard view={view} computer={profile.name} online={online} act={act} />}
            <History view={view} />
            <p className="settings-meta">Keys and tokens stay on {profile.name}, in {view.store}. This phone sees who each account is signed in as, never a key.</p>
          </>}
        </>}
      </div>
      {selected && view && (
        <AccountSheet account={selected} view={view} online={online} busy={busy} act={act}
          onSignIn={() => void signIn(selected)} onClose={() => setOpen(null)} />
      )}
      <AddService open={adding} online={online} onClose={() => setAdding(false)}
        onAdd={async (service) => {
          if (!(await act({ action: "add_service", service }, `Adding ${service.name}`, `Add ${service.name}`))) return false;
          toast(`${service.name} added`);
          return true;
        }} />
    </Screen>
  );
}

function SignInNow({ signIn, onOpen }: { signIn: SignIn; onOpen?: () => void }) {
  return (
    <Notice icon={Monitor} title={`Signing in to ${signIn.service}`}
      action={onOpen && <Button size="sm" onClick={onOpen}>Open Desktop control</Button>}>
      Finish it in the computer’s browser.
    </Notice>
  );
}

function AccountSheet({ account: a, view, online, busy, act, onSignIn, onClose }: {
  account: Account; view: AccountsView; online: boolean; busy: string | null; act: Act; onSignIn: () => void; onClose: () => void;
}) {
  const { profile } = useApp();
  const tasks = useHost((s) => s.tasks);
  const [key, setKey] = useState("");
  const [keying, setKeying] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const close = () => { setKey(""); setKeying(false); setDeleting(false); onClose(); };
  const warn = useWarning("account.delete");
  const deleteNow = () => void act({ action: "delete", account_id: a.account_id }, `Deleting ${a.service}`, `Delete ${a.service}`)
    .then((ok) => { if (ok) { toast(`${a.service} deleted`); close(); } });
  const disabled = !online || !!busy;
  const signedIn = a.state.state !== "signed_out";
  const signingIn = !!view.sign_in && !finished(view.sign_in.stage);
  const clientFile = useRef<HTMLInputElement>(null);
  const saveClient = async (file: File | undefined) => {
    if (clientFile.current) clientFile.current.value = "";
    if (!file) return;
    // Google's download is about 400 bytes; the computer takes up to 64 KiB.
    if (file.size > 64 * 1024) { toast(`${file.name} is too large to be an OAuth client file.`, { tone: "stop" }); return; }
    const sent = await file.text();
    if (await act({ action: "set_key", account_id: a.account_id, key: sent }, `Saving the OAuth client for ${a.service}`, `Save the OAuth client for ${a.service}`)) {
      toast(`OAuth client saved for ${a.service}`);
    }
  };
  const saveKey = async () => {
    const sent = key;
    setKey("");
    if (await act({ action: "set_key", account_id: a.account_id, key: sent }, `Saving the key for ${a.service}`, `Save the key for ${a.service}`)) {
      setKeying(false);
      toast(`Key saved for ${a.service}`);
    }
  };
  const footer = deleting ? <>
    <Button onClick={() => setDeleting(false)}>Keep it</Button>
    <Button variant="stop" disabled={disabled} pending={busy === `Deleting ${a.service}`}
      onClick={() => { warn.agreed(); deleteNow(); }}>
      Delete permanently
    </Button>
  </> : keying ? <>
    <Button variant="quiet" onClick={() => { setKey(""); setKeying(false); }}>Cancel</Button>
    <Button variant="primary" disabled={disabled || !key.trim()} pending={busy === `Saving the key for ${a.service}`} onClick={() => void saveKey()}>Save key</Button>
  </> : undefined;
  return (
    <Sheet open onClose={close} title={a.service} subtitle={KIND_WORDS[a.kind]} footer={footer}>
      <div className="settings-stack">
        <p className="account-status">
          <Pill tone={TONE[a.state.state]}>{STATE_WORDS[a.state.state]}</Pill>
          {a.identity && <span>{a.identity}</span>}
        </p>
        {a.state.state === "needs_attention" && <Notice tone="warn">{a.state.reason}</Notice>}
        {a.state.state === "active" && a.note && <Notice tone="warn" title="Missing a permission">{a.note}</Notice>}
        <p className="settings-meta">
          {a.checked_at != null ? <>Checked <Age at={a.checked_at} suffix=" ago" />{"\u00a0· "}</> : "Not checked yet\u00a0· "}
          <Button size="sm" variant="quiet" disabled={disabled} pending={busy === `Checking ${a.service}`} onClick={() => void act({ action: "check", account_id: a.account_id }, `Checking ${a.service}`)}>Check now</Button>
        </p>
        <p className="settings-meta">
          {a.kind === "agent"
            ? `${a.service} uses its own sign-in on ${profile.name}.`
            : a.variables.length ? `Tasks receive ${a.variables.join(", ")} while it is active.` : null}
        </p>
        {deleting ? <>
          <Notice tone="warn" title={`Delete ${a.service}?`}>
            {a.builtin
              ? `This signs it out and removes its saved key from ${profile.name}, then checks again. ${a.service} stays in the list, signed out.`
              : `This signs it out and removes its saved key and the service from ${profile.name}, then checks again.`}
            {a.manage_url && <> Where the service can’t end the session itself, {profile.name} opens its security page so you can end it there.</>}
          </Notice>
          {warn.check}
        </> : keying ? (
          <form className="settings-stack" onSubmit={(e) => { e.preventDefault(); if (key.trim()) void saveKey(); }}>
            <TextField label={`${a.service} key`} type="password" value={key} onChange={setKey} autoFocus
              autoComplete="current-password" spellCheck={false} autoCapitalize="off" name={`${a.account_id}-key`}
              hint={`Sent once to ${profile.name} over the paired connection and kept in ${view.store}. It is never shown again.`} />
          </form>
        ) : <>
          {a.needs === "client" ? (
            <>
              <Notice tone="unknown" title="Needs your own OAuth client">
                {a.service} signs in with an OAuth client you create, a Desktop app. Choose its JSON file here, or save it on {profile.name} and run{" "}
                <InlineCode className="cmd cmd--inline">{PROGRAM_NAME} accounts set-client {a.account_id} FILE</InlineCode> there.
              </Notice>
              <Button block variant="primary" icon={FileJson} disabled={disabled} pending={busy === `Saving the OAuth client for ${a.service}`}
                onClick={() => clientFile.current?.click()}>
                Choose the client file
              </Button>
            </>
          ) : a.needs === "key" || a.kind === "api_key" ? (
            <Button block variant={a.needs ? "primary" : "secondary"} icon={KeyRound} disabled={disabled} onClick={() => setKeying(true)}>
              {a.needs ? "Set key" : "Replace key"}
            </Button>
          ) : (
            <Button block variant={signedIn && !a.note ? "secondary" : "primary"} icon={ShieldCheck} disabled={disabled || signingIn}
              pending={busy === `Signing in to ${a.service}`} onClick={onSignIn}>
              {signedIn ? "Sign in again" : "Sign in"}
            </Button>
          )}
          {a.kind === "oauth" && a.needs !== "client" && (
            <Button block variant="quiet" icon={FileJson} disabled={disabled || (signingIn && view.sign_in?.account_id === a.account_id)}
              pending={busy === `Saving the OAuth client for ${a.service}`} onClick={() => clientFile.current?.click()}>
              Replace the OAuth client
            </Button>
          )}
          {a.kind === "oauth" && (
            <input ref={clientFile} type="file" accept="application/json,.json" className="visually-hidden" tabIndex={-1} aria-hidden
              onChange={(e) => void saveClient(e.target.files?.[0])} />
          )}
          {signingIn && view.sign_in?.account_id !== a.account_id && <p className="settings-meta">Another sign-in is running.</p>}
          {(a.state.state === "active" || a.state.state === "needs_attention") && <>
            <Button block icon={Pause} disabled={disabled} pending={busy === `Pausing ${a.service}`}
              onClick={() => void act({ action: "pause", account_id: a.account_id }, `Pausing ${a.service}`)}>Pause</Button>
            <p className="settings-meta">{PAUSE_WORDS}</p>
          </>}
          {a.state.state === "paused" && (
            <Button block icon={Play} disabled={disabled} pending={busy === `Resuming ${a.service}`}
              onClick={() => void act({ action: "resume", account_id: a.account_id }, `Resuming ${a.service}`)}>Resume</Button>
          )}
          {signedIn && (
            <Button block variant="stop-outline" disabled={disabled} pending={busy === `Signing out of ${a.service}`}
              onClick={() => void act({ action: "sign_out", account_id: a.account_id }, `Signing out of ${a.service}`, `Sign out of ${a.service}`)}>Sign out</Button>
          )}
          <Button block variant="stop-outline" disabled={disabled} pending={busy === `Deleting ${a.service}`}
            onClick={() => warn.ask(() => setDeleting(true), deleteNow)}>Delete permanently</Button>
          <section aria-label="Used by">
            <h3 className="settings-meta">Used by</h3>
            {a.used_by.length ? <ul className="account-tasks">{a.used_by.map((id) => <li key={id}>{tasks[id]?.title ?? "A task"}</li>)}</ul>
              : <p className="settings-meta">No running task uses it now.</p>}
          </section>
        </>}
      </div>
    </Sheet>
  );
}

function PasswordManagerCard({ view, computer, online, act }: { view: AccountsView; computer: string; online: boolean; act: Act }) {
  const [manager, setManager] = useState<PasswordManager | null>(null);
  return (
    <article className="settings-card" aria-labelledby="password-manager-title">
      <h3 id="password-manager-title">Password manager on this computer</h3>
      <p>
        Sign-ins open in {view.browser ?? "the default browser"} on {computer}. Add your password manager’s extension there once,
        so it can fill passwords and passkeys while you sign in from this phone.
      </p>
      <div className="account-managers" role="group" aria-label="Password manager">
        {MANAGERS.map(([id, name]) => (
          <Button key={id} size="sm" variant={manager === id ? "primary" : "secondary"} disabled={!online}
            onClick={() => { setManager(id); void act({ action: "set_up_password_manager", manager: id }, `Opening ${name}’s extension page`).then((ok) => ok && toast(`Opened on ${computer}`)); }}>
            {name}
          </Button>
        ))}
      </div>
      <p className="settings-meta">Each opens its extension page in that browser, on {computer}’s screen.</p>
      <Button disabled={!online} onClick={() => void act({ action: "password_manager_done" }, "Saving")}>It’s set up</Button>
    </article>
  );
}

function History({ view }: { view: AccountsView }) {
  if (!view.history.length) return <Empty title="No account history yet" />;
  return (
    <details className="account-history">
      <summary>History</summary>
      <ul>
        {view.history.map((h, i) => (
          <li key={`${h.at}-${i}`}>
            <strong>{EVENT_WORDS[h.event]}</strong> · {h.service}{h.detail ? ` · ${h.detail}` : ""}
            <p className="settings-meta">{h.by} · <Age at={h.at} suffix=" ago" /></p>
          </li>
        ))}
      </ul>
    </details>
  );
}
