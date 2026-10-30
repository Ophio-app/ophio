// SPDX-License-Identifier: Apache-2.0
import { useApp, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import { Section } from "../../design/ListRow";
import { Notice, availabilityText } from "../../design/Notice";
import type { HarnessCapabilities } from "../../protocol/types";
import { Age } from "../../design/Age";
import { AgentPreferences } from "./AgentPreferences";
import { AgentUpdates } from "./AgentUpdates";
const CAPABILITY: Record<keyof HarnessCapabilities, string> = {
  start_run: "Start tasks",
  steer_running: "Take messages while it works",
  interrupt: "Stop partway through",
  native_approvals: "Ask you before acting",
  resume_session: "Continue an earlier task",
  child_agents: "Show its helper agents",
  computer_use_delegation: "Use the computer’s screen",
  model_selection: "Choose a model",
  effort_selection: "Choose effort",
  cancel_queued_message: "Take back a message not yet sent",
};
export function AgentsAndAccounts() {
  const { ui } = useApp();
  const harnesses = useHost((s) => s.harnesses);
  const accounts = () => ui.push({ screen: "accounts" });
  return (
    <>
    <Section title="Agent apps" id="settings-agents">
      <div className="settings-stack">
        <AgentPreferences />
        <AgentUpdates />
        {!harnesses.length && (
          <Notice tone="unknown">
            The computer hasn’t reported its agents yet.
          </Notice>
        )}
        {harnesses.map((h) => {
          const installed = availabilityText(h.installed);
          return (
            <article className="settings-card" key={h.kind}>
              <h3>{h.display_name}</h3>
              <p className="settings-meta">
                {h.native_version
                  ? `Version ${h.native_version}`
                  : "Version unknown"}{" "}
                ·{" "}
                {h.integration === "managed"
                  ? "Full control"
                  : h.integration === "terminal_only"
                    ? "Terminal only"
                    : "Not supported yet"}
              </p>
              {h.integration !== "not_integrated" && <p>
                {installed.ok
                  ? "Installed"
                  : `${installed.label} · ${installed.detail ?? ""}`}
              </p>}
              {h.integration === "terminal_only" && (
                <p className="settings-meta">
                  Use this agent in a terminal. Task controls, approvals and
                  progress aren’t connected to the app.
                </p>
              )}
              {h.integration === "not_integrated" && (
                <p className="settings-meta">
                  {h.installed.state === "available" ? "Installed, not supported by this version of the app yet." : "Not found on this computer."}
                </p>
              )}
              {h.integration === "not_integrated" ? null : h.account.state === "signed_in" ? (
                <p>
                  Signed in
                  {h.account.account_label
                    ? ` · ${h.account.account_label}`
                    : ""}
                  {h.account.plan ? ` · ${h.account.plan}` : ""}
                </p>
              ) : (
                <Notice
                  tone={h.account.state === "signed_out" ? "warn" : "unknown"}
                  action={<Button size="sm" onClick={accounts}>Open Accounts</Button>}
                  title={
                    h.account.state === "signed_out"
                      ? "Signed out"
                      : "Account state unknown"
                  }
                >
                  {h.account.state === "signed_out"
                    ? h.account.remedy
                    : h.account.reason}
                </Notice>
              )}
              {h.catalog && <p className="settings-meta">{"Model list checked "}<Age at={h.catalog.checked_at} suffix=" ago" /></p>}
              {h.integration !== "not_integrated" && <details>
                <summary>Models, and what works from the phone<span className="visually-hidden"> for {h.display_name}</span></summary>
                {h.models.length ? <ul className="model-catalog">{h.models.map((m) => <li key={m.id}>
                  <strong>{m.label}</strong>{m.is_default ? " · Default" : ""}
                  <p>{m.details?.efforts.length ? `Effort: ${m.details.efforts.map((e) => e.label).join(" · ")}` : "No effort choices"}</p>
                </li>)}</ul> : <p className="settings-meta">The computer hasn’t listed this agent’s models.</p>}
                <ul className="settings-capabilities">
                  {(
                    Object.keys(CAPABILITY) as (keyof HarnessCapabilities)[]
                  ).map((key) => {
                    const a = availabilityText(h.capabilities[key] ?? { state: "unsupported", reason: "Not reported by this computer" });
                    return (
                      <li key={key}>
                        <strong>{CAPABILITY[key]}</strong>
                        <span>
                          {a.label}
                          {a.detail ? ` · ${a.detail}` : ""}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              </details>}
            </article>
          );
        })}
      </div>
    </Section>
    </>
  );
}
