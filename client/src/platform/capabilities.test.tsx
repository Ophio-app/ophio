// SPDX-License-Identifier: Apache-2.0
import { act, render } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { Platform } from "../host/link";
import type { HostSession } from "../host/session";
import type { AttentionItem } from "../protocol/types";
import { emptyHostState } from "../state/projection";
import { Store } from "../state/store";
import { InboxStore } from "../screens/work/inbox";
import { blobs } from "../state/annotations";
import { UiStore } from "../app/ui";
import {
  ShareIntake,
  PHONE_INBOX,
  MAX_SHARE_BYTES,
  MAX_SHARE_TEXT_BYTES,
} from "./shares";
import {
  platformProblems,
  PlatformNotices,
  reportPlatformError,
} from "./errors";
import {
  notificationDetail,
  notificationFor,
  watchAttention,
  parseAttentionRoute,
  openNotification,
} from "./notifications";
import { holdAwake, useKeepAwake } from "./awake";
import { PlatformEvents } from "./PlatformEvents";
import { simulatedDevice } from "../dev/sim/device";
function visibility(value: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}
beforeEach(() => {
  localStorage.clear();
  platformProblems.set([]);
  notificationDetail.set("minimal");
  visibility("visible");
  vi.restoreAllMocks();
});
function intake() {
  const stores = new Map<string, InboxStore>();
  const store = (id: string) => {
    let s = stores.get(id);
    if (!s) {
      s = new InboxStore(id);
      stores.set(id, s);
    }
    return s;
  };
  return { store, shares: new ShareIntake(store) };
}
const share = {
  id: "intake-1",
  name: "Idea",
  mime: "text/plain",
  text: "https://example.invalid/never-fetch",
};
it("deduplicates concurrent/replayed intake across reload, profile switches and discard", async () => {
  const { store, shares } = intake();
  await Promise.all([
    shares.receive([share], "a"),
    shares.receive([share], "a"),
  ]);
  expect(store("a").get().items).toHaveLength(1);
  expect(store("a").get().items[0]?.kind).toBe("link");
  await store("a").discard(store("a").get().items[0]!.id);
  const next = intake();
  await next.shares.receive([share], "b");
  expect(next.store("b").get().items).toHaveLength(0);
});
it("holds unpaired shares, then moves them locally to the first profile", async () => {
  const { store, shares } = intake();
  await shares.receive([share], null);
  expect(store(PHONE_INBOX).get().items).toHaveLength(1);
  await shares.paired("first");
  expect(store(PHONE_INBOX).get().items).toHaveLength(0);
  expect(store("first").get().items[0]?.projectId).toBeNull();
  await shares.paired("second");
  expect(store("second").get().items).toHaveLength(0);
});
it("reports each refused item and limit without losing good text or blob bytes", async () => {
  const put = vi.spyOn(blobs, "put").mockResolvedValue("blob-1");
  const { store, shares } = intake();
  await shares.receive(
    [
      {
        id: "bad",
        name: "Denied",
        mime: "image/png",
        error: "Read permission expired",
      },
      {
        id: "large",
        name: "Large",
        mime: "application/octet-stream",
        bytes: new Uint8Array(MAX_SHARE_BYTES + 1),
      },
      {
        id: "text-large",
        name: "Text",
        mime: "text/plain",
        text: "x".repeat(MAX_SHARE_TEXT_BYTES + 1),
      },
      share,
      {
        id: "image",
        name: "Picture",
        mime: "image/png",
        bytes: new Uint8Array([1, 2, 3]),
      },
    ],
    "a",
  );
  expect(store("a").get().items).toHaveLength(2);
  expect(store("a").get().items[0]).toMatchObject({
    kind: "image",
    blobKey: "blob-1",
    size: 3,
  });
  expect(put).toHaveBeenCalledOnce();
  expect(platformProblems.get()).toHaveLength(3);
});
it("retains source when destination storage fails; retries without duplicates", async () => {
  const { store, shares } = intake();
  await shares.receive([share], null);
  const setItem = Storage.prototype.setItem;
  const spy = vi
    .spyOn(Storage.prototype, "setItem")
    .mockImplementation(function (this: Storage, key, value) {
      if (key === "capture-inbox.a")
        throw new DOMException("Full", "QuotaExceededError");
      setItem.call(this, key, value);
    });
  await shares.paired("a");
  expect(store(PHONE_INBOX).get().items).toHaveLength(1);
  spy.mockRestore();
  await shares.paired("a");
  expect(new InboxStore("a").get().items).toHaveLength(1);
  expect(store(PHONE_INBOX).get().items).toHaveLength(0);
});
function attention(
  id: string,
  kind: AttentionItem["kind"] = "approval",
): AttentionItem {
  return {
    attention_id: id,
    kind,
    task_id: "task",
    run_id: "run",
    approval_id: "approval",
    created_at: 1,
    resolved_at: null,
    title: "SECRET prompt",
    body: "SECRET command",
    priority: 10,
  };
}
function host() {
  return {
    ...emptyHostState,
    caughtUp: true,
    projects: { project: { project_id: "project", name: "Garden" } },
    tasks: {
      task: {
        task_id: "task",
        project_id: "project",
        title: "Private task title",
      },
    },
    attention: { old: attention("old") },
  } as unknown as typeof emptyHostState;
}
it("notifies only new eligible hidden attention with minimal/default or task-title detail", () => {
  const state = new Store(host()),
    notify = vi.fn();
  const stop = watchAttention(
    { notify } as unknown as Platform,
    { profileId: "a", state, link: new Store({ state: "offline" }) } as unknown as HostSession,
    "Studio",
  );
  state.set((s) => ({
    ...s,
    attention: { ...s.attention, foreground: attention("foreground") },
  }));
  visibility("hidden");
  state.set((s) => ({
    ...s,
    attention: { ...s.attention, next: attention("next") },
  }));
  expect(notify).toHaveBeenCalledExactlyOnceWith(
    expect.objectContaining({ title: "Approval waiting", body: "Studio · Garden", category: "needs_you" }),
  );
  state.set((s) => ({
    ...s,
    attention: {
      ...s.attention,
      next: { ...attention("next"), title: "Changed" },
      connection: attention("connection", "connection_issue"),
    },
  }));
  expect(notify).toHaveBeenCalledOnce();
  notificationDetail.set("task_title");
  state.set((s) => ({
    ...s,
    attention: { ...s.attention, done: attention("done", "completed") },
  }));
  expect(notify).toHaveBeenLastCalledWith(
    expect.objectContaining({
      title: "Task finished",
      body: "Studio · Garden · Private task title",
      category: "finished",
      group: "a:finished",
      groupTitle: "Studio",
    }),
  );
  expect(JSON.stringify(notify.mock.calls)).not.toContain("SECRET");
  stop();
});
it("skips resolved items, connection issues and the initial snapshot/replay", () => {
  const notify = vi.fn(),
    state = new Store({ ...host(), caughtUp: false });
  const stop = watchAttention(
    { notify } as unknown as Platform,
    { profileId: "a", state, link: new Store({ state: "offline" }) } as unknown as HostSession,
    "Studio",
  );
  visibility("hidden");
  state.set(host());
  expect(notify).not.toHaveBeenCalled();
  expect(
    notificationFor(
      { ...attention("done"), resolved_at: 2 },
      host(),
      { profileId: "a", name: "Studio" },
      "minimal",
    ),
  ).toBeNull();
  expect(
    notificationFor(
      attention("connection", "connection_issue"),
      host(),
      { profileId: "a", name: "Studio" },
      "minimal",
    ),
  ).toBeNull();
  stop();
});
it("opens an exact validated reference without task effects or cross-profile confusion", () => {
  const ui = new UiStore("a"),
    route = { kind: "attention", profileId: "a", attentionId: "approval-1" };
  expect(parseAttentionRoute("https://example.invalid/approve")).toBeNull();
  const parsed = parseAttentionRoute(route)!;
  expect(openNotification(parsed, "b", ui)).toBe(false);
  expect(openNotification(parsed, "a", ui)).toBe(true);
  expect(ui.route()).toEqual({
    screen: "attention",
    attentionId: "approval-1",
  });
  expect(ui.get().drafts).toEqual({});
});
it("subscribes once at app level across profile changes, and releases on unmount", () => {
  const device = simulatedDevice(),
    platform = { ...device.capabilities } as Platform;
  const shared = vi.spyOn(platform, "onShared"),
    open = vi.fn();
  const view = render(
    <PlatformEvents platform={platform} profileId={null} onOpen={open} />,
  );
  view.rerender(
    <PlatformEvents platform={platform} profileId="profile" onOpen={open} />,
  );
  expect(shared).toHaveBeenCalledOnce();
  platform.notify?.({
    id: "one",
    title: "Approval waiting",
    body: "Garden",
    route: { kind: "attention", profileId: "profile", attentionId: "one" },
  });
  device.controls.open("one");
  expect(open).toHaveBeenCalledOnce();
  view.unmount();
  device.controls.open("one");
  expect(open).toHaveBeenCalledOnce();
});
it("opens the app alone for a notification that leads nowhere, and reports a bad one", () => {
  let tap: ((n: { id: string; route?: unknown }) => void) | undefined;
  const platform = {
      onNotificationOpen(listener: typeof tap) {
        tap = listener;
        return () => {};
      },
    } as unknown as Platform,
    open = vi.fn();
  render(<PlatformEvents platform={platform} profileId="a" onOpen={open} />);
  act(() => tap!({ id: "more", route: null }));
  expect(open).not.toHaveBeenCalled();
  expect(platformProblems.get()).toEqual([]);
  act(() => tap!({ id: "odd", route: "https://example.invalid" }));
  expect(open).not.toHaveBeenCalled();
  expect(platformProblems.get()).toHaveLength(1);
});
it("keeps awake for overlapping owners and releases on background/leave/stop/unmount", () => {
  const keepAwake = vi.fn(),
    platform = { keepAwake } as unknown as Platform;
  const first = holdAwake(platform),
    second = holdAwake(platform);
  expect(keepAwake.mock.calls).toEqual([[true]]);
  first();
  expect(keepAwake.mock.calls).toEqual([[true]]);
  visibility("hidden");
  visibility("visible");
  second();
  expect(keepAwake.mock.calls).toEqual([[true], [false], [true], [false]]);
  function Watch({ active }: { active: boolean }) {
    useKeepAwake(platform, active);
    return null;
  }
  const view = render(<Watch active />);
  view.rerender(<Watch active={false} />);
  view.rerender(<Watch active />);
  view.unmount();
  expect(keepAwake.mock.calls.slice(-4)).toEqual([
    [true],
    [false],
    [true],
    [false],
  ]);
});
it("deduplicates and dismisses visible platform errors", () => {
  const view = render(<PlatformNotices />);
  act(() => {
    reportPlatformError({ operation: "Camera", message: "Unavailable" });
    reportPlatformError({ operation: "Camera", message: "Unavailable" });
  });
  expect(platformProblems.get()).toHaveLength(1);
  act(() => view.getByRole("button", { name: "Dismiss" }).click());
  expect(platformProblems.get()).toHaveLength(0);
  view.unmount();
});
it("supports platforms without any optional capabilities", () => {
  const platform = {} as Platform;
  expect(() => holdAwake(platform)()).not.toThrow();
  const view = render(
    <PlatformEvents platform={platform} profileId={null} onOpen={() => {}} />,
  );
  view.unmount();
});
