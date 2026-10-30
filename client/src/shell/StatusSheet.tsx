// SPDX-License-Identifier: Apache-2.0
//
// Connection and readiness. Network, identity, desktop, terminal,
// agent tools and accounts are separate facts with their own ages, so one
// working part never vouches for another.

import { PRODUCT_NAME, PROGRAM_NAME } from "../brand";
import { RefreshCw } from "lucide-react";
import { useApp, useHost, useLink } from "../app/context";
import { Button } from "../design/Button";
import { EvidenceRow, type EvidenceStatus } from "../design/Evidence";
import { Section } from "../design/ListRow";
import { remedyText } from "../design/remedy";
import { Remedy } from "../design/Remedy";
import { Notice } from "../design/Notice";
import { Sheet } from "../design/Sheet";
import { deviceKind } from "../design/formFactor";
import { clockTime, useNow } from "../design/time";
import type { AccountReadiness, Availability, InputScope, ReadinessCheck as Check, UsageLimit } from "../protocol/types";
import { freshness } from "./freshness";
import { useStore } from "../state/store";
import { ReadinessCheck } from "../screens/work/ReadinessCheck";
import "./StatusSheet.css";

export const availabilityStatus = (a: Availability): EvidenceStatus =>
  a.state === "available" ? "passed" : a.state === "unknown" ? "unknown"
    : a.state === "temporarily_unavailable" ? "unavailable"
    : a.state === "permission_missing" || a.state === "unsupported" ? "info" : "failed";

/** Older companions may repeat a remedy in detail or omit sentence endings. */
export function checkText(...parts: (string | null | undefined)[]): string {
  const seen = new Set<string>();
  return parts.flatMap(part => {
    const text = part?.trim();
    if (!text) return [];
    const key = text.replace(/[.!?]+$/, "").toLocaleLowerCase();
    if (seen.has(key)) return [];
    seen.add(key);
    return [/[.!?…]$/.test(text) ? text : `${text}.`];
  }).join(" ");
}

/** What a capability's state means for the person. The tool's own reason
 * comes apart, to be shown on request. */
export function plainAvailability(a: Availability): { text: string | null; reason: string | null } {
  switch (a.state) {
    case "available":
      return { text: null, reason: null };
    case "permission_missing":
      return { text: checkText("Needs setup", remedyText(a.remedy)), reason: null };
    case "revoked":
      return { text: "Access revoked. Pair this device again to restore it.", reason: null };
    case "unsupported":
      return { text: "Not available on this computer.", reason: a.reason };
    case "temporarily_unavailable":
      return { text: "Not right now.", reason: a.reason };
    case "incompatible":
      return { text: "Not available with these versions.", reason: a.reason };
    case "unknown":
      return { text: "Not known yet.", reason: a.reason };
  }
}

/** A limit whose reset time has passed no longer holds, whether or not the
 * computer has said so yet. */
export function usageLimitNow(limit: UsageLimit | undefined, now: number): UsageLimit | null {
  return limit && (limit.resets_at === null || limit.resets_at > now) ? limit : null;
}

export function usageLimitText(limit: UsageLimit, now: number): string {
  return limit.resets_at === null ? "Usage limit reached." : `Usage limit reached. Resets at ${clockTime(limit.resets_at, now)}.`;
}

export function connectionCheck(check: Check, live: boolean, addresses: string[]): Check {
  if (check.area !== "network" || !live) return check;
  return { ...check, label: "Connection from this device", status: "passed", remedy: null,
    detail: addresses.length === 1
      ? `This device reached the computer at ${addresses[0]}. Other devices and addresses haven’t been checked.`
      : "This device reached the computer using one of its paired addresses. Other devices and addresses haven’t been checked." };
}

const SCOPE: Record<InputScope, string> = {
  output_bound: "Input goes to the chosen screen only",
  focused_target: "Input goes to whatever window has focus",
  none: "No remote input",
};

function account(a: AccountReadiness): { status: EvidenceStatus; text: string } {
  switch (a.state) {
    case "signed_in":
      return { status: "passed", text: [a.account_label, a.plan].filter(Boolean).join(" · ") || "Signed in" };
    case "signed_out":
      return { status: "failed", text: `Signed out. ${a.remedy}` };
    case "unknown":
      return { status: "unknown", text: a.reason };
  }
}

export function fingerprint(sha: string, whole = false): string {
  const groups = sha.toUpperCase().match(/.{4}/g) ?? [];
  return groups.length > 6 && !whole ? `${groups.slice(0, 3).join(" ")} … ${groups.slice(-3).join(" ")}` : groups.join(" ");
}

export function StatusSheet({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { profile } = useApp();
  const host = useHost((s) => s.host);
  // The details follow the clock and every word the computer sends; a
  // closed sheet shows none of them, so they exist only while it is open.
  return (
    <Sheet open={open} onClose={onClose} title={profile.name} subtitle={host ? `${host.os_label} · ${PRODUCT_NAME} ${host.companion_version}` : undefined} size="tall">
      <ConnectionDetails onClose={onClose} />
    </Sheet>
  );
}

function ConnectionDetails({ onClose }: { onClose: () => void }) {
  const { profile, session, ui, forget } = useApp();
  const link = useLink();
  const caughtUp = useHost((s) => s.caughtUp);
  const lastHeardAt = useHost((s) => s.lastHeardAt);
  const caps = useHost((s) => s.capabilities);
  const harnesses = useHost((s) => s.harnesses);
  const readiness = useHost((s) => s.readiness);
  const now = useNow();
  const unanswered = useStore(session.unanswered, (s) => s);
  const f = freshness(link, caughtUp, lastHeardAt, now, profile.name, unanswered);
  const online = link.state === "online";
  const mismatch = link.state === "offline" && link.mismatch === true;

  return (
    <>
      <Section title="Connection">
        <div className="group">
          <EvidenceRow
            status={f.tone === "ok" ? "passed" : f.tone === "stop" ? "failed" : f.tone === "warn" ? "unknown" : "pending"}
            claim={f.label === "Live" ? "Connected" : f.label}
            detail={f.detail}
          />
          {/* The certificate's fingerprint means nothing at a glance, so it
              waits under Details: what the row says is whether the computer
              that answered is the one this device paired with. */}
          <EvidenceRow
            status={online ? "passed" : mismatch ? "failed" : "info"}
            claim={online ? "Computer identity verified" : "Computer identity"}
            detail={online
              ? `${profile.name} showed the certificate it had when you paired`
              : mismatch
                ? `The device that answered didn’t have ${profile.name}’s certificate`
                : `Checked each time this ${deviceKind()} connects`}
            source={`Paired ${new Date(profile.pairedAt).toLocaleDateString()}`}
            reason={`Fingerprint of ${profile.name}’s certificate, saved when you paired: ${fingerprint(profile.certSha256, true)}`}
          />
          <EvidenceRow status="info" claim="Private address" detail={profile.addresses.join(", ")} />
        </div>
        {link.state === "offline" && (
          <Button variant="secondary" block icon={RefreshCw} onClick={() => session.reconnect()}>
            Try again now
          </Button>
        )}
        {mismatch && (
          <Notice
            tone="warn"
            title={`Another device answered at ${profile.name}’s address`}
            action={
              <Button variant="secondary" size="sm" onClick={() => void forget()}>
                Forget {profile.name} and pair again
              </Button>
            }
          >
            It didn’t have {profile.name}’s certificate, so nothing was sent to it. This can happen on unfamiliar Wi-Fi, away
            from your private network. The {deviceKind()} keeps trying and reconnects by itself once {profile.name} answers.
            Pair again only if you reinstalled {PRODUCT_NAME} there yourself.
          </Notice>
        )}
        {link.state === "failed" && (link.reason === "revoked" || link.reason === "not_paired") && (
          <Notice
            tone="stop"
            title="This device needs to pair again"
            action={
              <Button variant="secondary" size="sm" onClick={() => void forget()}>
                Forget {profile.name} and pair again
              </Button>
            }
          >
            {link.detail}
          </Notice>
        )}
      </Section>

      {caps && (
        <Section title="On the computer" >
          <div className="group">
            {(
              [
                ["Watch the desktop", caps.capture],
                ["Remote input", caps.input],
                ["Terminal", caps.terminal],
                ["Library files", caps.library],
                ["File transfer", caps.transfers],
                ["AI computer use", caps.computer_use],
                ["Voice", caps.speech],
              ] as const
            ).map(([label, a]) => {
              const plain = plainAvailability(a);
              return (
                <EvidenceRow
                  key={label}
                  status={availabilityStatus(a)}
                  claim={label}
                  detail={label === "Remote input" && a.state === "available" ? SCOPE[caps.input_scope] : plain.text && <Remedy text={plain.text} />}
                  reason={plain.reason}
                />
              );
            })}
          </div>
        </Section>
      )}

      {caps && <Section title="Start at sign-in">
        <p className="fine">{caps.os === "windows"
          ? "On the computer, register startup for your Windows account. It runs after you sign in to the desktop; Startup Apps can disable it."
          : `On the computer, install the user service from your desktop session. If the desktop does not manage graphical-session.target, use its autostart to run ${PRODUCT_NAME} for desktop access.`}</p>
        <code className="cmd">{PROGRAM_NAME} service install</code>
        <p className="fine">Run this on the computer. Opening this screen changes nothing.</p>
      </Section>}

      {harnesses.length > 0 && (
        <Section title="Agent tools">
          <p className="fine">This version supports Claude Code and Codex.</p>
          <div className="group">
            {harnesses.filter(h => h.kind === "claude_code" || h.kind === "codex").map((h) => {
              const acct = account(h.account);
              const ready = h.capabilities.start_run;
              const plain = plainAvailability(ready);
              const limit = usageLimitNow(h.usage_limit, now);
              return (
                <EvidenceRow
                  key={h.kind}
                  status={ready.state === "available" && limit ? "failed" : availabilityStatus(ready)}
                  claim={h.display_name}
                  detail={ready.state !== "available" ? plain.text : limit ? usageLimitText(limit, now) : checkText("Ready to start tasks", acct.text)}
                  reason={plain.reason}
                  source={h.native_version ?? undefined}
                />
              );
            })}
          </div>
        </Section>
      )}

      <Section
        title="Checks"
        action={
          <ReadinessCheck label="Run checks" />
        }
      >
        {readiness ? (
          <div className="group">
            {readiness.checks.map((check, i) => {
              const c = connectionCheck(check, f.live, profile.addresses);
              // A computer that words its checks for the person says what they can
              // do; the tool's own reason stays on request. Not run then means
              // unsupported or waiting for setup, not a check that failed to run.
              const worded = c.summary != null;
              const setup = c.detail === "Needs setup.";
              const status = c.status === "not_run" && (worded || setup) ? "info" : c.status;
              const text = checkText(worded ? null : c.detail, c.remedy && remedyText(c.remedy));
              return <EvidenceRow
                key={`${c.area}-${i}`}
                status={status}
                claim={c.summary ?? c.label}
                detail={text && <Remedy text={text} />}
                reason={worded && !setup ? c.detail : null}
                at={c.area === "network" && f.live ? lastHeardAt : readiness.checked_at}
              />;
            })}
          </div>
        ) : (
          <p className="muted">No checks have run yet.</p>
        )}
        <p className="fine">Checks look at the computer’s setup. They don’t start a model, sign in or change settings.</p>
      </Section>

      <Section>
        <div className="group">
          <EvidenceRow status="info" claim="Settings" detail="Agents, accounts, this computer and this device" onOpen={() => { onClose(); ui.push({ screen: "settings" }); }} />
          <EvidenceRow
            status="info"
            claim="Connections and readiness"
            detail="Paired devices, chat links and what’s ready to use"
            onOpen={() => {
              onClose();
              ui.push({ screen: "connections" });
            }}
          />
        </div>
      </Section>
    </>
  );
}
