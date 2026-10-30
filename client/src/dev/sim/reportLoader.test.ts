// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { loadReportState } from "./reportLoader";
import { HostSession } from "../../host/session";
import type { ControlLink } from "../../host/link";
import { UiStore } from "../../app/ui";
import { stateShape } from "../../diagnostics/stateShape";
import { emptyHostState } from "../../state/projection";

afterEach(() => { vi.useRealTimers(); localStorage.clear(); });
it("loads a redacted store and reopens the same logical screen", async () => {
  vi.useFakeTimers(); const ui = new UiStore("fake"); ui.push({ screen: "settings" });
  const shape = await stateShape({ host: { ...emptyHostState, tasks: {} }, ui: ui.get() }, "fake-salt");
  ui.push({ screen: "system" });
  const session = new HostSession("fake", {} as ControlLink);
  loadReportState(shape, session, ui);
  expect(ui.get().stacks[ui.get().tab].at(-1)?.screen).toBe("settings");
  expect(session.state.get().caughtUp).toBe(true);
  expect(() => loadReportState({ version: 500, store: {} }, session, ui)).toThrow("can’t be opened");
});
