// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../../app/ui";
import { toast } from "../../design/Toast";
import { contentIdentity, holdsWork, type AnnotationStore } from "../../state/annotations";

/** Markup watched now, per interface, and how to stop watching each without
 * changing what the interface holds. */
const watched = new WeakMap<UiStore, Map<string, () => void>>();

/** Takes the markup off a draft. It stays on this device while the toast
 * offers Undo, which puts back exactly what was removed. After that it is
 * deleted with its images once nothing holds it: no draft, and no Annotate
 * screen where it may still be edited. Changed there after it was removed, it
 * becomes the message's markup again if the message has none. Undo never
 * replaces markup the message has gained since: only an empty one that is not
 * open for editing gives way. What it was removed from is kept with the
 * interface, so a restart carries on from there. */
export function removeMarkup(ui: UiStore, store: AnnotationStore, projectId: string, taskId: string | null, id: string): void {
  const removed = store.get()[id];
  ui.set((s) => ({ ...s, removedMarkup: { ...s.removedMarkup, [id]: { projectId, taskId, removedAs: removed ? contentIdentity(removed) : null } } }));
  ui.saveDraft(projectId, taskId, { annotationId: null });
  const letGo = watch(ui, store, id, true);
  toast("Markup removed", {
    action: {
      label: "Undo",
      run: () => {
        const now = ui.draft(projectId, taskId)?.annotationId ?? null;
        if (now && now !== id && inUse(ui, store, now)) {
          toast("Markup not put back: the message has newer markup");
          letGo();
          return;
        }
        ui.saveDraft(projectId, taskId, { annotationId: id });
        if (now && now !== id && !drafted(ui, now)) void store.remove(now);
      },
    },
    expire: letGo,
  });
}

/** Carries on watching markup removed before the app last closed, or before
 * this interface was last shown. Undo is no longer offered for it. */
export function resumeRemovedMarkup(ui: UiStore, store: AnnotationStore): void {
  for (const id of Object.keys(ui.get().removedMarkup))
    if (!watched.get(ui)?.has(id)) watch(ui, store, id, false)();
}

/** Stops watching for an interface whose computer was forgotten. Its removed
 * markup goes with the rest of that interface; a switch away keeps watching. */
export function forgetRemovedMarkup(ui: UiStore): void {
  for (const end of watched.get(ui)?.values() ?? []) end();
  watched.delete(ui);
}

const drafted = (ui: UiStore, markup: string) => {
  const { drafts, savedDrafts } = ui.get();
  return [...Object.values(drafts), ...Object.values(savedDrafts).flat()].some((d) => d.annotationId === markup);
};
const shown = (ui: UiStore, markup: string) =>
  Object.values(ui.get().stacks).some((stack) => stack.some((r) => r.screen === "annotate" && r.annotationId === markup));
const inUse = (ui: UiStore, store: AnnotationStore, markup: string) => {
  const d = store.get()[markup];
  return !!d && (holdsWork(d) || shown(ui, markup));
};

/** Watches removed markup until it is on a message again or deleted, and
 * returns what ends `offered`: the time Undo may put it back. */
function watch(ui: UiStore, store: AnnotationStore, id: string, offered: boolean): () => void {
  let watching = true;
  const check = () => {
    if (!watching) return;
    const removed = ui.get().removedMarkup[id];
    const d = store.get()[id];
    if (!removed || !d || drafted(ui, id)) return stop();
    const { projectId, taskId } = removed;
    if (shown(ui, id) && contentIdentity(d) !== removed.removedAs && !ui.draft(projectId, taskId)?.annotationId) {
      stop();
      ui.saveDraft(projectId, taskId, { annotationId: id });
      return;
    }
    if (!offered && !shown(ui, id)) {
      stop();
      void store.remove(id);
    }
  };
  const offUi = ui.subscribe(check),
    offStore = store.subscribe(check);
  const running = watched.get(ui) ?? new Map<string, () => void>();
  watched.set(ui, running);
  const end = () => {
    watching = false;
    offUi();
    offStore();
  };
  const stop = () => {
    end();
    running.delete(id);
    if (ui.get().removedMarkup[id])
      ui.set((s) => {
        const { [id]: _, ...rest } = s.removedMarkup;
        return { ...s, removedMarkup: rest };
      });
  };
  running.set(id, end);
  return () => {
    offered = false;
    check();
  };
}
