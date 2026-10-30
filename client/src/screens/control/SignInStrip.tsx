// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { KeyRound } from "lucide-react";
import { useApp, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import type { InputAction, SignIn } from "../../protocol/types";
import { confirmHere, finished, useAccounts } from "../settings/useAccounts";

/** What the stage asks of the person at the phone, in words. */
export function signInWords(s: SignIn): { title: string; detail: string | null } {
  const stage = s.stage;
  switch (stage.stage) {
    case "starting": return { title: `Signing in to ${s.service}`, detail: "Opening the sign-in on the computer…" };
    case "browser": return { title: `Signing in to ${s.service}`, detail: `Finish on ${stage.url_host} in the computer’s browser.` };
    case "code": return { title: `Signing in to ${s.service}`, detail: `Enter this code on ${stage.url_host}.` };
    case "checking": return { title: `Checking with ${s.service}…`, detail: null };
    case "done": return { title: stage.identity ? `Signed in as ${stage.identity}` : `Signed in to ${s.service}`, detail: null };
    case "failed": return { title: `${s.service} sign-in failed`, detail: stage.reason };
    case "cancelled": return { title: `${s.service} sign-in cancelled`, detail: null };
    case "timed_out": return { title: `${s.service} sign-in timed out`, detail: "It waited too long and was stopped. Nothing was saved." };
  }
}

/**
 * Follows the computer's sign-in over Desktop control: what to do, the code to
 * type and Cancel; then the result, with Try again or Back to Accounts.
 */
export function SignInStrip({ send, canType }: { send: (action: InputAction) => boolean; canType: boolean }) {
  const { ui, platform } = useApp();
  const online = useLink().state === "online";
  const accounts = useAccounts();
  const [busy, setBusy] = useState(false);
  const s = accounts.view?.sign_in;
  if (!s || !accounts.allowed || (finished(s.stage) && accounts.followed !== s.account_id)) return null;
  const words = signInWords(s);
  const done = finished(s.stage);
  const run = async (action: Parameters<typeof accounts.act>[0], label: string, confirm?: string) => {
    setBusy(true);
    try {
      if (confirm) await confirmHere(platform, confirm);
      await accounts.act(action, label);
    }
    catch (e) { toast(e instanceof Error ? e.message : String(e), { tone: "stop" }); }
    finally { setBusy(false); }
  };
  const back = () => { accounts.unfollow(); ui.push({ screen: "accounts" }, "work"); };
  return (
    <Notice icon={KeyRound} className="sign-in-strip" tone={s.stage.stage === "failed" || s.stage.stage === "timed_out" ? "warn" : "info"} title={words.title}
      action={<div className="sign-in-strip__actions">
        {s.stage.stage === "code" && (
          <Button size="sm" variant="primary" disabled={!canType} onClick={() => { if (s.stage.stage === "code" && !send({ action: "text", text: s.stage.code })) toast("Not typed. Take over first.", { tone: "stop" }); }}>
            Type it
          </Button>
        )}
        {!done && <Button size="sm" variant="quiet" disabled={!online || busy} onClick={() => void run({ action: "cancel_sign_in" }, "Cancelling the sign-in")}>Cancel</Button>}
        {done && s.stage.stage !== "done" && (
          <Button size="sm" disabled={!online || busy} onClick={() => void run({ action: "sign_in", account_id: s.account_id }, `Signing in to ${s.service}`, `Sign in to ${s.service}`)}>Try again</Button>
        )}
        {done && <Button size="sm" variant={s.stage.stage === "done" ? "primary" : "quiet"} onClick={back}>Back to Accounts</Button>}
      </div>}>
      {words.detail}
      {s.stage.stage === "code" && <p className="sign-in-strip__code" aria-label={`Code ${s.stage.code.split("").join(" ")}`}>{s.stage.code}</p>}
      {s.stage.stage === "code" && !canType && <p className="fine">Take over to type it, or type it with the keyboard.</p>}
    </Notice>
  );
}
