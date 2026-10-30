// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { useApp } from "../../app/context";
import { PROGRAM_NAME } from "../../brand";
import { Button } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { Sheet } from "../../design/Sheet";
import type { ServiceKind, ServiceProfile } from "../../protocol/types";

type Kind = ServiceKind["kind"];
const BLANK = { name: "", variable: "", check: "", manage: "", authorize: "", token: "", scopes: "", signIn: "", status: "", signOut: "" };
type Form = typeof BLANK;
const optional = (v: string) => v.trim() || null;

/** What the form describes, or null while a required field is empty. The
 * computer checks the rest and says what is wrong. */
export function describedService(kind: Kind, f: Form): ServiceProfile | null {
  const name = f.name.trim(), variable = f.variable.trim();
  if (!name || !variable) return null;
  switch (kind) {
    case "api_key":
      return { name, service: { kind, variable, check_url: optional(f.check), manage_url: optional(f.manage) } };
    case "oauth": {
      const scopes = f.scopes.split(/[\s,]+/).filter(Boolean);
      if (!f.authorize.trim() || !f.token.trim()) return null;
      return { name, service: { kind, authorize_url: f.authorize.trim(), token_url: f.token.trim(), scopes, variable, manage_url: optional(f.manage) } };
    }
    case "cli":
      if (!f.signIn.trim() || !f.status.trim()) return null;
      return { name, service: { kind, sign_in: f.signIn.trim(), status: f.status.trim(), sign_out: optional(f.signOut), config_variable: variable, manage_url: optional(f.manage) } };
  }
}

export function AddService({ open, online, onClose, onAdd }: {
  open: boolean; online: boolean; onClose: () => void; onAdd: (service: ServiceProfile) => Promise<boolean>;
}) {
  const { profile } = useApp();
  const [kind, setKind] = useState<Kind>("api_key");
  const [form, setForm] = useState<Form>(BLANK);
  const [pending, setPending] = useState(false);
  const patch = (p: Partial<Form>) => setForm((f) => ({ ...f, ...p }));
  const field = (key: keyof Form, label: string, extra: { hint?: string; placeholder?: string; inputMode?: "url" } = {}) => (
    <TextField label={label} value={form[key]} onChange={(v) => patch({ [key]: v })} autoComplete="off" autoCapitalize="off" spellCheck={false} {...extra} />
  );
  const service = describedService(kind, form);
  const close = () => { setForm(BLANK); setKind("api_key"); onClose(); };
  const add = async () => {
    if (!service) return;
    setPending(true);
    try { if (await onAdd(service)) close(); } finally { setPending(false); }
  };
  return (
    <Sheet open={open} onClose={close} title="Add a service" size="tall"
      footer={<>
        <Button variant="quiet" onClick={close}>Cancel</Button>
        <Button variant="primary" disabled={!online || !service} pending={pending} onClick={() => void add()}>Add</Button>
      </>}>
      <form className="settings-stack" onSubmit={(e) => { e.preventDefault(); void add(); }}>
        <Segmented label="How it signs in" value={kind} onChange={setKind} options={[
          { value: "api_key", label: "API key" }, { value: "oauth", label: "OAuth app" }, { value: "cli", label: "CLI sign-in" },
        ]} />
        {field("name", "Name", { placeholder: "Groq" })}
        {kind === "api_key" && <>
          {field("variable", "Environment variable", { placeholder: "GROQ_API_KEY", hint: "Tasks receive the key in this variable." })}
          {field("check", "Check address (optional)", { inputMode: "url", hint: "An https address that answers 200 when given the key." })}
        </>}
        {kind === "oauth" && <>
          {field("authorize", "Sign-in address", { inputMode: "url" })}
          {field("token", "Token address", { inputMode: "url" })}
          {field("scopes", "Scopes", { hint: "Separated by spaces." })}
          {field("variable", "Environment variable", { hint: "Tasks receive a fresh access token in this variable." })}
          <p className="settings-meta">Then give it your OAuth client at the computer: <code className="cmd cmd--inline">{PROGRAM_NAME} accounts set-client ACCOUNT FILE</code>.</p>
        </>}
        {kind === "cli" && <>
          {field("signIn", "Sign-in command", { placeholder: "gcloud auth login" })}
          {field("status", "Status command", { placeholder: "gcloud auth list", hint: "Its output names who is signed in." })}
          {field("signOut", "Sign-out command (optional)")}
          {field("variable", "Config folder variable", { placeholder: "CLOUDSDK_CONFIG", hint: `Each account gets its own folder on ${profile.name}; tasks receive this variable pointing at it.` })}
          <p className="settings-meta">Commands run on {profile.name} without a shell.</p>
        </>}
        {field("manage", "Security page (optional)", { inputMode: "url", hint: "Where you end sessions or revoke keys at the provider." })}
      </form>
    </Sheet>
  );
}
