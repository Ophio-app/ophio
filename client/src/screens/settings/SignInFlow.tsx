// SPDX-License-Identifier: Apache-2.0
//
// An agent's sign-in, finished in this phone's browser: the computer starts
// it, the person opens the provider's page here and, for Claude, pastes back
// the code the page shows; Codex shows a code to type on the page instead.
// Every tap shows its next step at once and steps back, with the computer's
// words, if the computer refuses. The pasted code is a secret: it is sent
// once and never kept, shown or recorded.

import { useEffect, useRef, useState } from "react";
import { CheckCircle2, ClipboardPaste, Copy, ExternalLink, Monitor, ShieldCheck } from "lucide-react";
import { useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import type { Account, AccountAction, AccountsView } from "../../protocol/types";
import { confirmHere, finished, useAccounts } from "./useAccounts";
import { hasPhoneSignIn, helpLabel, shortService, webLink, type OnPhoneStage } from "./signIn";
import "./signIn.css";

/** Where ChatGPT turns on the code sign-in Codex uses. */
const CODEX_SECURITY = "https://chatgpt.com/settings/security";

export type SignInPhase =
  | { phase: "idle" }
  | { phase: "starting" }
  | { phase: "on_phone"; stage: OnPhoneStage }
  | { phase: "on_computer" }
  | { phase: "checking" }
  | { phase: "done"; identity: string | null }
  | { phase: "failed"; reason: string; helpUrl: string | null }
  | { phase: "timed_out" }
  | { phase: "cancelled" }
  | { phase: "other"; service: string }
  /** A stage a newer computer sends that this app doesn't know yet. */
  | { phase: "waiting" };

/** What a tap shows before the computer says so itself. */
type Pending = { kind: "starting" | "checking" | "cancelled"; since: AccountsView | null; settled: boolean };

/** One account's sign-in on this phone: what it shows now and what can be done. */
export function useSignInFlow(accountId: string) {
  const { platform, ui } = useApp();
  const online = useLink().state === "online";
  const accounts = useAccounts();
  const target = useHost((s) => s.targets[0]?.target_id ?? null);
  const feature = useHost(hasPhoneSignIn);
  const view = accounts.view;
  const account: Account | null = view?.accounts.find((a) => a.account_id === accountId) ?? null;
  const name = shortService(accountId, account?.service);
  const onPhone = feature && !!account?.phone_sign_in;
  const [pending, setPending] = useState<Pending | null>(null);
  const [busy, setBusy] = useState(false);
  // The code being pasted, here so it outlives the step that shows it.
  const [code, setCode] = useState("");
  const latest = useRef(view);
  latest.current = view;

  // A tap's step holds until the computer's own answer has been read back,
  // so the screen never flickers back to the step before it.
  useEffect(() => {
    if (!pending?.settled) return;
    if (view !== pending.since) { setPending(null); return; }
    const t = setTimeout(() => setPending(null), 4000);
    return () => clearTimeout(t);
  }, [pending, view]);

  const send = async (kind: Pending["kind"], action: AccountAction, label: string, confirm?: string): Promise<boolean> => {
    setPending({ kind, since: null, settled: false });
    try {
      if (confirm) await confirmHere(platform, confirm);
      await accounts.act(action, label);
      setPending((p) => (p?.kind === kind ? { ...p, settled: true, since: latest.current } : p));
      return true;
    } catch (e) {
      setPending(null);
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
      return false;
    }
  };

  const s = view?.sign_in ?? null;
  const mine = s?.account_id === accountId ? s : null;
  let phase: SignInPhase;
  if (pending) phase = { phase: pending.kind };
  else if (mine && (!finished(mine.stage) || accounts.followed === accountId)) {
    const st = mine.stage;
    switch (st.stage) {
      case "starting": phase = { phase: "starting" }; break;
      case "on_phone": phase = { phase: "on_phone", stage: st }; break;
      case "browser": case "code": phase = { phase: "on_computer" }; break;
      case "checking": phase = { phase: "checking" }; break;
      case "done": phase = { phase: "done", identity: st.identity }; break;
      case "failed": phase = { phase: "failed", reason: st.reason, helpUrl: webLink(st.help_url) }; break;
      case "timed_out": phase = { phase: "timed_out" }; break;
      case "cancelled": phase = { phase: "cancelled" }; break;
      default: phase = { phase: "waiting" }; break;
    }
  } else if (s && !finished(s.stage)) phase = { phase: "other", service: s.service };
  else phase = { phase: "idle" };

  // A code never sent is dropped once the step that asked for it has gone.
  const asking = phase.phase === "on_phone" || phase.phase === "checking";
  useEffect(() => { if (!asking) setCode(""); }, [asking]);

  const start = () => send("starting", { action: "sign_in", account_id: accountId, ...(onPhone ? { on_phone: true } : {}) }, `Signing in to ${name}`, `Sign in to ${name}`);
  /** Sends the pasted code once and clears it; not sent, it is kept for another try. */
  const submit = async () => {
    const sent = code.trim();
    if (!sent) return;
    const signedFor = latest.current?.sign_in;
    if (!signedFor || signedFor.account_id !== accountId) { toast("This sign-in has ended. Start it again.", { tone: "stop" }); return; }
    setCode("");
    // Named for the sign-in it was typed for, so a code for an earlier one is refused.
    if (!(await send("checking", { action: "submit_sign_in_code", account_id: signedFor.account_id, started_at: signedFor.started_at, code: sent }, `Sending ${name}’s sign-in code`))) setCode((now) => now || sent);
  };
  const cancel = () => send("cancelled", { action: "cancel_sign_in" }, "Cancelling the sign-in");
  /** Today's way: the computer's own browser, watched over Desktop control. */
  const onComputer = async () => {
    setBusy(true);
    try {
      await confirmHere(platform, `Sign in to ${name}`);
      // The computer answers once the phone's sign-in has stopped; only then can the next start.
      if (mine && !finished(mine.stage)) {
        try { await accounts.act({ action: "cancel_sign_in" }, "Cancelling the sign-in"); }
        catch (e) { throw new Error(`The sign-in on this phone hasn’t stopped yet, so nothing changed. ${e instanceof Error ? e.message : ""} Try again in a moment.`.replace(/\s+/g, " ").trim()); }
      }
      await accounts.act({ action: "sign_in", account_id: accountId }, `Signing in to ${name}`);
      if (account?.browser_sign_in && target) ui.push({ screen: "desktop", targetId: target }, "control");
      return true;
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
      return false;
    } finally { setBusy(false); }
  };
  const openPage = (link: string) => {
    void platform.openExternal(link).catch(() => toast("The sign-in page couldn’t be opened. Try again.", { tone: "stop" }));
  };
  return { account, name, phase, online, onPhone, code, setCode, busy: busy || (!!pending && !pending.settled), start, submit, cancel, onComputer, openPage, unfollow: accounts.unfollow, canWatch: !!target };
}

export type SignInFlowState = ReturnType<typeof useSignInFlow>;

/** The running sign-in, step by step. Shown while `flow.phase` isn't idle. */
export function SignInFlow({ flow, onClose, onLeave }: { flow: SignInFlowState; onClose: () => void; onLeave?: () => void }) {
  const { profile } = useApp();
  const { phase, name, online } = flow;
  const close = () => { flow.unfollow(); onClose(); };
  const disabled = !online || flow.busy;
  const instead = (
    <Button block variant="quiet" icon={Monitor} disabled={disabled} onClick={() => void flow.onComputer().then((ok) => { if (ok) onLeave?.(); })}>
      Finish on the computer’s screen instead
    </Button>
  );
  const again = <>
    <Button block variant="primary" icon={ShieldCheck} disabled={disabled} onClick={() => void flow.start()}>Try again</Button>
    {instead}
  </>;
  switch (phase.phase) {
    case "idle": return null;
    case "starting":
      return <div className="sign-in-flow" aria-live="polite">
        <p className="sign-in-flow__status" aria-busy="true">Starting the sign-in on {profile.name}…</p>
        <Button block variant="quiet" disabled>Cancel</Button>
      </div>;
    case "on_phone":
      return <div className="sign-in-flow">
        {(phase.stage.paste_code ?? flow.account?.account_id === "claude") ? <PasteSteps flow={flow} stage={phase.stage} /> : <CodexSteps flow={flow} stage={phase.stage} />}
        <div className="sign-in-flow__more">
          <Button block variant="quiet" disabled={disabled} onClick={() => void flow.cancel()}>Cancel</Button>
          {instead}
        </div>
      </div>;
    case "on_computer":
      return <div className="sign-in-flow">
        <Notice icon={Monitor} title={`Signing in to ${name}`}>Finish it in {profile.name}’s browser.</Notice>
        <Button block variant="quiet" disabled={disabled} onClick={() => void flow.cancel()}>Cancel</Button>
      </div>;
    case "waiting":
      return <div className="sign-in-flow" aria-live="polite">
        <p className="sign-in-flow__status" aria-busy="true">Signing in to {name}…</p>
        <Button block variant="quiet" disabled={disabled} onClick={() => void flow.cancel()}>Cancel</Button>
      </div>;
    case "checking":
      return <div className="sign-in-flow" aria-live="polite">
        <p className="sign-in-flow__status" aria-busy="true">Checking the sign-in…</p>
      </div>;
    case "done":
      return <div className="sign-in-flow" aria-live="polite">
        <p className="sign-in-flow__done">
          <CheckCircle2 aria-hidden className="sign-in-flow__done-icon" />
          <span>{phase.identity ? <>{name} is signed in as <bdi>{phase.identity}</bdi></> : `${name} is signed in`}</span>
        </p>
        <p className="settings-meta">New {name} tasks on {profile.name} can start again.</p>
        <Button block variant="primary" onClick={close}>Done</Button>
      </div>;
    case "failed":
      return <div className="sign-in-flow" aria-live="polite">
        <Notice tone="warn" title="The sign-in didn’t work">{phase.reason}</Notice>
        {phase.helpUrl && <Button block icon={ExternalLink} onClick={() => flow.openPage(phase.helpUrl!)}>{helpLabel(phase.helpUrl)}</Button>}
        {again}
      </div>;
    case "timed_out":
      return <div className="sign-in-flow" aria-live="polite">
        <Notice tone="warn" title="The sign-in timed out">It waited too long and was stopped. Nothing changed on {profile.name}.</Notice>
        {again}
      </div>;
    case "cancelled":
      return <div className="sign-in-flow" aria-live="polite">
        <Notice tone="unknown" title="Sign-in cancelled">Nothing changed on {profile.name}.</Notice>
        {again}
      </div>;
    case "other":
      return <Notice tone="unknown" title="Another sign-in is running">
        {phase.service} is signing in on {profile.name}. Finish or cancel it first.
      </Notice>;
  }
}

function OpenPage({ flow, stage, then }: { flow: SignInFlowState; stage: OnPhoneStage; then: string }) {
  return <div className="sign-in-step">
    <Button block variant="primary" icon={ExternalLink} disabled={!flow.online} onClick={() => flow.openPage(stage.link)}>Open the sign-in page</Button>
    <p className="settings-meta">Opens <bdi>{stage.url_host}</bdi> in this phone’s browser. {then}</p>
  </div>;
}

/** Claude: sign in on the page, then paste the code it shows back here. */
function PasteSteps({ flow, stage }: { flow: SignInFlowState; stage: OnPhoneStage }) {
  const { code, setCode } = flow;
  const field = useRef<HTMLInputElement>(null);
  const canPaste = typeof navigator !== "undefined" && typeof navigator.clipboard?.readText === "function";
  const paste = async () => {
    try {
      const text = (await navigator.clipboard.readText()).trim();
      if (text) setCode(text); else toast("Nothing to paste yet. Copy the code from the sign-in page first.");
    } catch {
      field.current?.focus();
      toast("Press and hold the box to paste the code.");
    }
  };
  return <>
    <OpenPage flow={flow} stage={stage} then="Sign in there, then copy the code it shows." />
    <form className="sign-in-step" onSubmit={(e) => { e.preventDefault(); void flow.submit(); }}>
      <TextField ref={field} label="Paste the code from the sign-in page" value={code} onChange={setCode}
        type="text" name="sign-in-code" autoComplete="one-time-code" autoCapitalize="off" autoCorrect="off" spellCheck={false}
        enterKeyHint="send" inputMode="text"
        error={stage.code_refused && !code ? "That code didn’t work. Copy the whole code from the page and paste it again." : undefined} />
      <div className="sign-in-step__row">
        {canPaste && <Button type="button" icon={ClipboardPaste} onClick={() => void paste()}>Paste</Button>}
        <Button type="submit" variant="primary" disabled={!flow.online || flow.busy || !code.trim()}>Sign in</Button>
      </div>
    </form>
  </>;
}

/** Codex: type the code shown here on the page. */
function CodexSteps({ flow, stage }: { flow: SignInFlowState; stage: OnPhoneStage }) {
  const code = stage.code ?? "";
  const copy = () => void navigator.clipboard.writeText(code).then(
    () => toast("Code copied"),
    () => toast("Copy is unavailable. Type the code on the page instead.", { tone: "stop" }),
  );
  return <>
    {code && <div className="sign-in-step">
      <p className="sign-in-step__lead">Enter this code on the sign-in page.</p>
      <div className="sign-in-code">
        <span className="sign-in-code__value" aria-label={`Code ${code.split("").join(" ")}`}>{code}</span>
        <Button size="sm" icon={Copy} onClick={copy}>Copy</Button>
      </div>
    </div>}
    <OpenPage flow={flow} stage={stage} then="This moves on by itself once you’ve signed in." />
    <div className="sign-in-help">
      <p className="settings-meta">If the page says device code sign-in is off, turn it on in ChatGPT’s security settings first.</p>
      <Button size="sm" variant="quiet" icon={ExternalLink} onClick={() => flow.openPage(CODEX_SECURITY)}>Open ChatGPT’s security settings</Button>
    </div>
  </>;
}
