// SPDX-License-Identifier: Apache-2.0
//
// The game's rules and the person's check that the handoff is Clankerhub's
// (CH-016 J5, D-4). One confirmation covers both of the receiver's gates, so
// this screen says both: what trusting the rules means, and that Øphio can't
// yet confirm the handoff with Clankerhub on its own. The person checks the
// listing themselves; that check is what the computer is given, it counts
// for five minutes, and the screen never pretends Øphio made it.

import { useEffect, useId, useState } from "react";
import { useApp, useLink } from "../../app/context";
import { PRODUCT_NAME as APP } from "../../brand";
import { Button } from "../../design/Button";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { repositoryLabel, roleLabel, shortRevision } from "./codes";
import { FLOW } from "./Contribution";
import { CHECK_LIFETIME_S, useContribution } from "./useContribution";
import { PolicyReader } from "./PolicyReader";
import { SaveWarning } from "./SaveWarning";
import "./contribute.css";

/** A check older than this is asked for again before it's sent, leaving the
 * computer time to receive it while it still counts. */
const CHECK_FRESH_MS = 4 * 60_000;

export default function ContributionRules({ route }: RouteProps<"contribution_rules">) {
  const c = useContribution(route.handoffId, { follow: false });
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const [checkedAt, setCheckedAt] = useState<number | null>(null);
  const [stale, setStale] = useState(false);
  const reasonId = useId();
  const ctx = c.view?.context ?? null;
  const awaiting = c.view?.code === "awaiting_local_approval" || c.view?.code === "freshness_unknown" || c.view?.code === "reconcile_required"
    || c.view?.code === "acquisition_failed" || c.view?.code === "busy" || c.view?.code === "disk_full";
  // Anything else means this screen's moment has passed: back to the handoff.
  useEffect(() => { if (c.view && !awaiting && c.pending === null) ui.finish(["contribution_rules"]); }, [c.view, awaiting, c.pending, ui]);
  if (!c.handoff || !ctx) {
    return (
      <Screen title="Rules" footer={<div className="contribute-foot"><Button block onClick={() => ui.finish(["contribution_rules"])}>Back</Button></div>}>
        <div className="contribute"><SaveWarning /><p className="contribute-body">The rules show once {profile.name} has checked the handoff.</p></div>
      </Screen>
    );
  }
  const setup = ctx.policies.some((p) => p.role === "setup");
  const canPreview = c.reach.state === "ready" && c.reach.capabilities.capabilities.includes("policy-preview");
  const confirm = async () => {
    if (checkedAt === null) return;
    if (Date.now() - checkedAt > CHECK_FRESH_MS) { setCheckedAt(null); setStale(true); return; }
    if (await c.prepare(checkedAt)) ui.finish(["contribution_rules"]);
  };
  const reason = !online ? `Can’t reach ${profile.name}.` : checkedAt === null ? "Check the listing on Clankerhub first." : null;
  return (
    <Screen title="Rules" footerFlows footer={
      <div className="contribute-foot">
        {/* Stays focusable while it can't be used, so its reason is met where it is. */}
        <Button variant="primary" block pending={c.pending === "prepare"} aria-disabled={reason ? true : undefined} aria-describedby={reason ? reasonId : undefined}
          onClick={() => { if (!reason) void confirm(); }}>Trust rules and get source</Button>
        {reason && <p className="contribute-foot__reason" id={reasonId}>{reason}</p>}
        <Button variant="quiet" block onClick={() => ui.finish(FLOW)}>Not now</Button>
      </div>
    }>
      <div className="contribute">
        <SaveWarning />
        <h1 className="contribute-head__title">Review the game’s rules</h1>
        <p className="contribute-body">
          These files tell agents how to work on this game. Trusting them lets {APP} give them to agents you start in this project. It doesn’t run anything or change {APP}’s permissions.
        </p>
        <div className="group">
          {[...ctx.policies].sort(byRole).map((p) => (
            canPreview
              ? <PolicyReader key={`${c.hostId} ${c.view!.operation_id} ${p.role} ${p.path} ${p.sha256}`} operationId={c.view!.operation_id} policy={p} />
              : <ListRow key={`${p.role} ${p.path}`} title={<span className="mono">{p.path}</span>} subtitle={roleLabel(p.role)} />
          ))}
        </div>
        <p className="fine">
          {canPreview
            ? `Read a file before trusting it. ${APP} downloads source data to check the file against this handoff; reading it creates no project and runs nothing. Matching the file doesn’t confirm who published it.`
            : c.reach.state === "ready"
              ? `Update ${profile.name} to read rules here before trusting them. This computer can only check the files while getting the source.`
              : `Connect to ${profile.name} to check whether rules can be read here.`}
          {setup ? ` ${APP} won’t run setup steps from these files. If you start setup later, ${APP} asks separately.` : ""}
        </p>
        <p className="contribute-panel">
          Applies to <span className="mono">{repositoryLabel(ctx.repository_url)}</span> at <span className="mono">{shortRevision(ctx.commit)}</span> on {profile.name}.
        </p>
        <Section title="Check it’s from Clankerhub" id={`${reasonId}-check`}>
          <p className="contribute-body">
            {APP} can’t ask Clankerhub yet whether this handoff is genuine and still listed. Open the game’s page on Clankerhub and check it shows this game, release, source, revision and handoff fingerprint.
          </p>
          <p className="contribute-body">
            The fingerprint covers the rule files too. If the page doesn’t show the same fingerprint, don’t trust this handoff: {APP} has no other way to check it yet.
          </p>
          <dl className="group contribute-facts">
            <div className="contribute-facts__row"><dt>Game</dt><dd><span className="mono">{ctx.game_id}</span></dd></div>
            <div className="contribute-facts__row"><dt>Release</dt><dd><span className="mono">{ctx.release_id}</span></dd></div>
            <div className="contribute-facts__row"><dt>Source</dt><dd><span className="mono">{repositoryLabel(ctx.repository_url)}</span></dd></div>
            <div className="contribute-facts__row"><dt>Revision</dt><dd><span className="mono contribute-facts__full">{ctx.commit}</span></dd></div>
            <div className="contribute-facts__row"><dt>Handoff fingerprint</dt><dd><span className="mono contribute-facts__full">{grouped(c.view!.handoff_sha256)}</span></dd></div>
          </dl>
          {stale && <Notice tone="warn">Your check is more than 4 minutes old. Check the listing again, then confirm.</Notice>}
          <label className="contribute-check">
            <input type="checkbox" checked={checkedAt !== null} onChange={(e) => { setStale(false); setCheckedAt(e.target.checked ? Date.now() : null); }} />
            <span>I checked the game’s Clankerhub page just now, and all of it matches</span>
          </label>
          <p className="fine">Your check counts for {CHECK_LIFETIME_S / 60} minutes. If the download needs it again later, {APP} asks again.</p>
        </Section>
        {c.refused && <Notice tone="stop" title="Not started">{c.refused === "permission" ? `This device can’t add projects on ${profile.name}.` : `${profile.name} refused it. Reference: ${c.refused}.`}</Notice>}
        {c.sendError && <Notice tone="warn" title={c.sendError === "offline" ? "Not sent" : "No answer yet"}>
          {c.sendError === "offline" ? `${profile.name} isn’t connected right now. Nothing was downloaded.` : `${profile.name} may have started. Go back to the handoff to see where it is.`}
        </Notice>}
      </div>
    </Screen>
  );
}

/** A fingerprint in groups of four, as people compare one: never shortened,
 * since a shared beginning proves nothing. */
const grouped = (hex: string) => hex.match(/.{1,4}/g)?.join(" ") ?? hex;

const ORDER = ["instructions", "contribution", "model-policy", "design", "setup"];
const byRole = (a: { role: string }, b: { role: string }) => ORDER.indexOf(a.role) - ORDER.indexOf(b.role);
