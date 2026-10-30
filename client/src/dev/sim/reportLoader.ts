// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../../host/session";
import type { UiState, UiStore } from "../../app/ui";
import type { ReportBundle, StateShape } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import { STATE_SHAPE_VERSION } from "../../diagnostics/stateShape";

const fixtures = import.meta.glob<{ default: StateShape | ReportBundle }>("./reports/*.json");
/** Imported only behind the static sim-mode gate in App. */
export function loadReportState(value: StateShape | ReportBundle, session: Pick<HostSession, "state">, ui: Pick<UiStore, "get" | "set">): void {
  const shape = "schema" in value ? value.state : value;
  if (!shape || shape.version !== STATE_SHAPE_VERSION || !shape.store || typeof shape.store !== "object") throw new Error("This report’s screen state can’t be opened.");
  const store = shape.store as { host?: Partial<HostState>; ui?: Partial<UiState> };
  if (store.host) session.state.set({ ...emptyHostState, ...store.host, caughtUp: true });
  if (store.ui) ui.set({ ...ui.get(), ...store.ui });
}
export async function loadReportFromQuery(session: HostSession, ui: UiStore): Promise<void> {
  const name = new URLSearchParams(location.search).get("report");
  if (!name || !/^[A-Za-z0-9_-]+$/.test(name)) return;
  const loader = fixtures[`./reports/${name}.json`];
  if (!loader) return;
  // The first snapshot must not overwrite the report as soon as it loads.
  if (!session.state.get().caughtUp) await new Promise<void>((resolve) => {
    const off = session.state.subscribe(() => { if (session.state.get().caughtUp) { off(); resolve(); } });
  });
  loadReportState((await loader()).default, session, ui);
}
