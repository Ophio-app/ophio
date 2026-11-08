// SPDX-License-Identifier: Apache-2.0
//
// Signing one agent in again, opened from Sign in again or its notification.
// Opened by a tap, it starts at once; opened later, as from a notification,
// it waits for the person to choose.

import { useEffect, useRef } from "react";
import { Monitor, ShieldCheck } from "lucide-react";
import { useApp } from "../../app/context";
import { PRODUCT_NAME } from "../../brand";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { Notice } from "../../design/Notice";
import { NotAllowed } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { SignInFlow, useSignInFlow } from "./SignInFlow";
import { takeStart } from "./signIn";
import type { StartWhen } from "../../app/signInFeature";
import { STATE_WORDS, useAccounts } from "./useAccounts";
import "./accounts.css";
import "./settings.css";

export default function SignInScreen({ route }: RouteProps<"sign_in">) {
  const { profile, ui } = useApp();
  const accounts = useAccounts();
  const flow = useSignInFlow(route.accountId);
  const { account, phase, name, online } = flow;
  // Asked for by a tap, it starts at once; by a notification, once the
  // computer's current word says the agent is still signed out.
  const asked = useRef<StartWhen | null | undefined>(undefined);
  const initial = useRef(accounts.view);
  useEffect(() => {
    if (asked.current === undefined) asked.current = takeStart(profile.profileId, route.accountId);
    const when = asked.current;
    if (!when || !account) return;
    if (when === "if_signed_out" && initial.current !== null && accounts.view === initial.current) return;
    asked.current = null;
    const running = ["starting", "on_phone", "checking", "on_computer", "other"].includes(phase.phase);
    const out = account.state.state === "signed_out" || account.state.state === "needs_attention";
    if (!running && (when === "always" || out)) void flow.start();
  });
  const close = () => ui.finish(["sign_in", "attention"]);
  const disabled = !online || flow.busy;
  return (
    <Screen title={`Sign in to ${name}`} subtitle={profile.name} className="settings-screen sign-in-screen">
      <div className="settings-stack">
        {!accounts.supported ? (
          <Notice tone="warn" title="Update the computer">
            {profile.name} can’t sign agents in from this phone yet. Update {PRODUCT_NAME} on it.
          </Notice>
        ) : !accounts.allowed ? (
          <NotAllowed what="manage accounts" permission="accounts" computerOnly
            detail="Signing an agent in uses the computer’s accounts." />
        ) : accounts.error && !account ? (
          <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={accounts.reload}>Try again</Button>}>{accounts.error}</Notice>
        ) : !account ? (
          accounts.view
            ? <Notice tone="unknown">{profile.name} has no account called {route.accountId}.</Notice>
            : <div className="screen__loading" aria-busy="true" aria-label="Loading accounts" />
        ) : phase.phase !== "idle" ? (
          <SignInFlow flow={flow} onClose={close} />
        ) : <>
          <p className="account-status">
            <Pill tone={account.state.state === "active" ? "ok" : account.state.state === "needs_attention" ? "warn" : "neutral"}>{STATE_WORDS[account.state.state]}</Pill>
            {account.identity && <bdi>{account.identity}</bdi>}
          </p>
          <p>
            {flow.onPhone
              ? `${name} signs in with its own account on ${profile.name}. Sign in here: its sign-in page opens in this phone’s browser.`
              : `${name} signs in in ${profile.name}’s browser. Watch and finish it from Desktop control.`}
          </p>
          {flow.onPhone ? <>
            <Button block variant="primary" icon={ShieldCheck} disabled={disabled} onClick={() => void flow.start()}>Sign in on this phone</Button>
            <Button block variant="quiet" icon={Monitor} disabled={disabled} onClick={() => void flow.onComputer()}>Sign in on the computer’s screen instead</Button>
          </> : (
            <Button block variant="primary" icon={ShieldCheck} disabled={disabled} onClick={() => void flow.onComputer()}>Sign in</Button>
          )}
        </>}
      </div>
    </Screen>
  );
}
