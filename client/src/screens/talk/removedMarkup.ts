// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../../app/ui";
import { toast } from "../../design/Toast";
import { contentIdentity, holdsWork, type AnnotationStore } from "../../state/annotations";

/** Markup watched once per computer, and how to stop watching each without
 * changing what its shared drafts hold. */
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
  ui = ui.sharedStore();
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
  ui = ui.sharedStore();
  for (const id of Object.keys(ui.get().removedMarkup))
    if (!watched.get(ui)?.has(id)) watch(ui, store, id, false)();
}

/** Stops watching for an interface whose computer was forgotten. Its removed
 * markup goes with the rest of that interface; a switch away keeps watching. */
export function forgetRemovedMarkup(ui: UiStore): void {
  ui = ui.sharedStore();
  for (const end of watched.get(ui)?.values() ?? []) end();
  watched.delete(ui);
}

const drafted = (ui: UiStore, markup: string) => {
  const { drafts, savedDrafts } = ui.get();
  return [...Object.values(drafts), ...Object.values(savedDrafts).flat()].some((d) => d.annotationId === markup);
};
const shown = (ui: UiStore, markup: string) => ui.retainedNavigationStores().some((view) =>
  Object.values(view.get().stacks).some((stack) => stack.some((r) => r.screen === "annotate" && r.annotationId === markup)));
const inUse = (ui: UiStore, store: AnnotationStore, markup: string) => {
  const d = store.get()[markup];
  return !!d && (holdsWork(d) || shown(ui, markup));
};

/** Watches removed markup until it is on a message again or deleted, and
 * returns what ends `offered`: the time Undo may put it back. */
function watch(ui: UiStore, store: AnnotationStore, id: string, offered: boolean): () => void {
  // A stale second-pane Remove must not leave two cleanup observers behind.
  watched.get(ui)?.get(id)?.();
  let watching = true;
  const navigation = new Map<UiStore, () => void>();
  const observeViews = () => {
    const views = new Set(ui.navigationStores());
    for (const [view, off] of navigation) if (!views.has(view)) { off(); navigation.delete(view); }
    for (const view of views) if (!navigation.has(view)) {
      const offPosition = view.subscribe(check);
      const offRetained = view.navigationRetained.subscribe(check);
      navigation.set(view, () => { offPosition(); offRetained(); });
    }
  };
  const check = () => {
    if (!watching) return;
    // A side can be created while Undo or an existing editor holds the image.
    // Subscribe before deciding whether any of those editors still needs it.
    observeViews();
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
  observeViews();
  const offStore = store.subscribe(check);
  const running = watched.get(ui) ?? new Map<string, () => void>();
  watched.set(ui, running);
  const end = () => {
    watching = false;
    for (const off of navigation.values()) off();
    navigation.clear();
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
