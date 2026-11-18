// SPDX-License-Identifier: Apache-2.0
//
// Checks are read from the computer. Local approvals use the same expected
// state as other instant controls, and disappear if the computer refuses.

import { useCallback, useEffect, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "./context";
import { CHECKS_FEATURE } from "./checksFeature";
import { toast } from "../design/Toast";
import { commandResult } from "../host/commandResult";
import { CommandError } from "../host/session";
import { settle } from "../host/expected";
import type { Command } from "../protocol/types";
import type { CheckEdit } from "../protocol/generated/CheckEdit";
import type { ProjectChecks } from "../protocol/generated/ProjectChecks";
import type { ProjectChecksView } from "../protocol/generated/ProjectChecksView";
import type { ChecksDraftView } from "../protocol/generated/ChecksDraftView";

export function useChecks(projectId: string | null) {
  const { session, profile } = useApp();
  const supported = useHost((s) => !!s.capabilities?.features.includes(CHECKS_FEATURE));
  const allowed = useHost((s) => !!s.grants?.tasks);
  const online = useLink().state === "online";
  const revision = useHost((s) => s.journalHead);
  const projects = useHost((s) => Object.values(s.checksViews ?? {}).filter((p) => !projectId || p.project_id === projectId), shallowEqual);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [withoutChecks, setWithoutChecks] = useState(0);
  const reload = useCallback(async () => {
    if (!supported || !allowed || !session.online) return;
    try {
      const r = await session.query({ kind: "checks_list", project_id: projectId });
      session.state.set((s) => {
        const views = { ...s.checksViews };
        if (!projectId) for (const [id, old] of Object.entries(views)) {
          if (!r.projects.some((p) => p.project_id === id) && (old.checks.length || old.drafts.length)) delete views[id];
        }
        for (const p of r.projects) if (!views[p.project_id] || views[p.project_id]!.approved.version <= p.approved.version) views[p.project_id] = p;
        return { ...s, checksViews: views };
      });
      setWithoutChecks(r.projects_without_checks);
      setError(null);
    } catch (e) { setError(describeError(e, profile.name)); }
    finally { setLoading(false); }
  }, [session, supported, allowed, projectId, profile.name]);
  useEffect(() => { setLoading(true); void reload(); }, [reload, online]);
  useEffect(() => {
    if (!online) return;
    const t = setTimeout(() => { void reload(); }, 350);
    return () => clearTimeout(t);
  }, [revision, reload, online]);
  const proving = projects.some((p) => p.drafts.some((d) => d.draft.state === "proving"));
  useEffect(() => {
    if (!online || !proving) return;
    const timer = setInterval(() => { void reload(); }, 2000);
    return () => clearInterval(timer);
  }, [online, proving, reload]);
  return { supported, allowed, online, projects, loading, error, withoutChecks, reload };
}

export function useCheckActions() {
  const { session, profile, ui, platform } = useApp();
  const send = async (body: Command, label: string) => {
    platform.haptic?.("tap");
    return commandResult(session, await session.command(body, { label }));
  };
  const fail = (e: unknown) => toast(e instanceof CommandError && e.error.code === "stale_revision" ? e.error.message : describeError(e, profile.name), { tone: "stop" });
  const refresh = async (projectId: string) => {
    const r = await session.query({ kind: "checks_list", project_id: projectId });
    session.state.set((s) => {
      const views = { ...s.checksViews };
      for (const p of r.projects) if (!views[p.project_id] || views[p.project_id]!.approved.version <= p.approved.version) views[p.project_id] = p;
      return { ...s, checksViews: views };
    });
  };
  const optimistic = (projectId: string, next: ProjectChecksView) => {
    const before = session.state.get().checksViews?.[projectId];
    return session.state.expect((s) => {
      if ((s.checksViews?.[projectId]?.approved.version ?? 0) > (before?.approved.version ?? 0)) return s;
      return { ...s, checksViews: { ...s.checksViews, [projectId]: next } };
    });
  };
  const save = async (projectId: string, baseVersion: number, edit: CheckEdit, restored?: ProjectChecks) => {
    const view = session.state.get().checksViews?.[projectId];
    if (!view) return false;
    let checks = view.checks;
    if (edit.kind === "set_enabled") checks = checks.map((v) => v.check.check_id === edit.check_id ? { ...v, check: { ...v.check, enabled: edit.enabled } } : v);
    if (edit.kind === "delete") checks = checks.filter((v) => v.check.check_id !== edit.check_id);
    // Facts belong to a definition. Until the computer returns fresh facts,
    // never attach the old version's factual lines to an edited definition.
    const updating = { when: "Updating…", internet: "", time_limit: "Updating…", typical_time: null };
    if (edit.kind === "save") checks = checks.map((v) => v.check.check_id === edit.check.check_id ? { ...v, check: edit.check, facts: updating } : v);
    if (edit.kind === "keep_inputs") checks = checks.map((v) => v.check.check_id === edit.check_id ? { ...v, inputs_changed: [] } : v);
    if (restored) checks = restored.checks.map((check) => ({ ...(view.checks.find((v) => v.check.check_id === check.check_id) ?? { last_result: null, last_task_id: null, last_task_title: null, last_finished_at: null, inputs_changed: [] }), check, facts: updating }));
    // A stale editor still sends its original version for the computer to
    // refuse, but must not temporarily replace a newer approval on the phone.
    const drop = view.approved.version === baseVersion
      ? optimistic(projectId, { ...view, approved: { ...view.approved, version: baseVersion + 1, checks: checks.map((v) => v.check) }, checks })
      : () => {};
    try {
      const r = await send({ command: "checks_save", project_id: projectId, base_version: baseVersion, edit }, "Change checks");
      if (r?.kind !== "checks_saved") throw new Error("The computer did not confirm this change. Open checks again before trying again.");
      // An accepted approval stays saved even if reading its fresh facts fails.
      session.state.set((s) => (s.checksViews?.[projectId]?.approved.version ?? 0) > r.approved.version ? s : { ...s, checksViews: { ...s.checksViews, [projectId]: { ...view, approved: r.approved, checks } } });
      try { await refresh(projectId); } catch (e) { fail(e); }
      settle(session.state, drop, (s) => (s.checksViews?.[projectId]?.approved.version ?? 0) >= r.approved.version);
      return r;
    } catch (e) { drop(); fail(e); return false; }
  };
  const ask = async (projectId: string, checkId: string | null, request: string) => {
    try {
      const r = await send({ command: "checks_ask", project_id: projectId, check_id: checkId, request }, "Ask for checks");
      if (r?.kind !== "task_created") throw new Error("The computer did not confirm the task. Check Tasks before trying again.");
      ui.openTask(projectId, r.task_id);
      return true;
    } catch (e) { fail(e); return false; }
  };
  const draftAction = async (projectId: string, draft: ChecksDraftView, keep: number[] | null) => {
    const view = session.state.get().checksViews?.[projectId];
    let next = view;
    if (view) {
      let checks = view.checks;
      if (keep) for (const i of keep) {
        const item = draft.draft.items[i];
        if (!item) continue;
        if (item.kind === "remove") checks = checks.filter((v) => v.check.check_id !== item.check_id);
        else {
          // A proof belongs to its suggestion, never to approved result history.
          const previous = view.checks.find((v) => v.check.check_id === item.check.check_id);
          const v = { check: item.check, facts: draft.facts[i]!, last_result: previous?.last_result ?? null, last_task_id: previous?.last_task_id ?? null, last_task_title: previous?.last_task_title ?? null, last_finished_at: previous?.last_finished_at ?? null, inputs_changed: [] };
          checks = item.kind === "add" ? [...checks, v] : checks.map((old) => old.check.check_id === item.check.check_id ? v : old);
        }
      }
      next = { ...view, checks, drafts: view.drafts.filter((d) => d.draft.id !== draft.draft.id), approved: { ...view.approved, version: view.approved.version + (keep ? 1 : 0), checks: checks.map((v) => v.check) } };
    }
    const drop = next ? optimistic(projectId, next) : () => {};
    try {
      const r = await send(keep ? { command: "checks_apply_draft", draft_id: draft.draft.id, base_version: draft.draft.base_version, keep_items: keep }
        : { command: "checks_discard_draft", draft_id: draft.draft.id }, keep ? "Use suggested checks" : "Discard suggested checks");
      if (!r || (keep ? r.kind !== "checks_saved" : r.kind !== "checks_draft")) throw new Error("The computer did not confirm this change. Open checks again before trying again.");
      if (next) {
        const accepted = { ...next, approved: r.kind === "checks_saved" ? r.approved : next.approved };
        session.state.set((s) => (s.checksViews?.[projectId]?.approved.version ?? 0) > accepted.approved.version ? s : { ...s, checksViews: { ...s.checksViews, [projectId]: accepted } });
      }
      try { await refresh(projectId); } catch (e) { fail(e); }
      drop();
      return true;
    } catch (e) { drop(); fail(e); return false; }
  };
  const run = async (projectId: string, checkId: string) => {
    try {
      const r = await send({ command: "checks_run_now", project_id: projectId, check_ids: [checkId] }, "Run check");
      return r?.kind === "check_job" ? r.job : null;
    } catch (e) { fail(e); return null; }
  };
  return { save, ask, draftAction, run, refresh, fail };
}
