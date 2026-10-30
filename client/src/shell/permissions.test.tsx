// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore, type Route, type Tab } from "../app/ui";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import type { Grants } from "../protocol/types";
import { Shell } from "./Shell";

let app: App;
let pendingQueries: Set<Promise<unknown>>;
const realNow = Date.now.bind(Date);
const realTimeout = globalThis.setTimeout.bind(globalThis);
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });

beforeAll(async () => {
  // Compile the visited screens before fake timers and the first case's budget.
  // Navigation still goes through Shell's real lazy routes.
  await Promise.all([
    import("../screens/talk/Talk"),
    import("../screens/control/Control"),
    import("../screens/work/Work"),
    import("../screens/work/Agents"),
    import("../screens/work/Inbox"),
    import("../screens/work/NextUp"),
    import("../screens/transfers/Transfers"),
    import("../screens/settings/Settings"),
    import("../screens/settings/Connections"),
    import("../screens/library/Library"),
    import("../screens/library/Folder"),
  ]);
}, 60_000);

beforeEach(async () => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(
    profile.profileId,
    platform.control(profile.profileId),
  );
  // A query's refusal can arrive after the screen first renders.
  const pending = new Set<Promise<unknown>>();
  pendingQueries = pending;
  const query = session.query.bind(session);
  vi.spyOn(session, "query").mockImplementation((q) => {
    const result = query(q);
    pending.add(result);
    void result.then(() => pending.delete(result), () => pending.delete(result));
    return result;
  });
  app = {
    platform,
    profile,
    profiles: [profile],
    session,
    ui: new UiStore(profile.profileId),
    switchTo: vi.fn(),
    pairAnother: vi.fn(),
    rename: vi.fn(),
    forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(() => {
  try {
    cleanup();
    app?.session.stop();
    window.__sim?.host.dispose();
  } finally {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  }
});

const front = () => document.querySelector<HTMLElement>(
  "section.tab:not(.is-hidden) > .route:not(.is-hidden)",
);

async function routeText(): Promise<string> {
  // Fake time cannot finish import(). Commit the screen first, then wait for
  // its queries instead of running seconds of unrelated background activity.
  await act(async () => { await vi.dynamicImportSettled(); });
  const deadline = realNow() + 2000;
  for (;;) {
    await advance(50);
    const route = front();
    try {
      expect(pendingQueries.size).toBe(0);
      expect(route).not.toBeNull();
      expect(route!.querySelector(".screen")).not.toBeNull();
      expect(route!.querySelector(".screen__loading")).toBeNull();
      return route!.textContent ?? "";
    } catch (error) {
      if (realNow() >= deadline) throw error;
    }
    // Let real work finish, then deliver its simulated replies before reading.
    await act(() => new Promise<void>((resolve) => realTimeout(resolve, 10)));
  }
}

const GRANTS: (keyof Grants)[] = [
  "view",
  "control_input",
  "terminal",
  "tasks",
  "approvals",
  "computer_use",
  "library_read",
  "library_write",
  "transfers",
  "admin",
];
const VISITS: [Tab, Route][] = [
  ["talk", { screen: "talk" }],
  ["control", { screen: "control" }],
  ["work", { screen: "work" }],
  ["work", { screen: "agents" }],
  ["work", { screen: "inbox" }],
  ["work", { screen: "next_up" }],
  ["work", { screen: "transfers" }],
  ["work", { screen: "settings" }],
  ["work", { screen: "connections" }],
  ["library", { screen: "library" }],
  ["library", { screen: "folder", rootId: ids.rootNotes, path: "" }],
];

/** Where each missing permission is explained, with the words used. */
const EXPLAINED: Partial<Record<keyof Grants, [Route["screen"], string]>> = {
  terminal: ["control", "open terminals"],
  tasks: ["next_up", "start, steer or stop tasks"],
  library_read: ["folder", "read the Library"],
  transfers: ["transfers", "send or receive files"],
  admin: ["connections", "manage access"],
};

it.each(GRANTS)(
  "without %s, no screen shows the computer's bare refusal and the gap is explained",
  async (grant) => {
    window.__sim!.permissions({ [grant]: false });
    await advance(3000);
    render(
      <AppProvider app={app}>
        <Shell />
      </AppProvider>,
    );
    const refused: string[] = [];
    for (const [tab, route] of VISITS) {
      act(() => app.ui.reset(tab, route.screen === tab ? [] : [route]));
      // Each of a screen's segments (Control's Screens, Terminals and
      // Previews) is a view of its own.
      const texts = [await routeText()];
      const parts = [...(front()?.querySelectorAll<HTMLElement>('[role="radiogroup"][aria-label="Show"] [role="radio"]') ?? [])];
      for (const part of parts.slice(1)) {
        fireEvent.click(part);
        texts.push(await routeText());
      }
      for (const text of texts) {
        const shown = `${text} ${document.querySelector(".toasts")?.textContent ?? ""}`;
        if (/given permission for that/.test(shown)) refused.push(route.screen);
      }
      if (EXPLAINED[grant]?.[0] === route.screen)
        expect(texts.join(" ")).toContain(
          `This device may not ${EXPLAINED[grant][1]}`,
        );
    }
    expect(refused).toEqual([]);
  },
);
