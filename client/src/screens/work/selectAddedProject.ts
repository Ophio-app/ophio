// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../../app/ui";
import type { HostSession } from "../../host/session";

/** A receipt can precede the project's event. Wait until the host knows the
 * project before choosing it, or the shell will replace it with Home. */
export function selectAddedProject(session: HostSession, ui: UiStore, projectId: string): void {
  if (session.state.get().projects[projectId]) {
    ui.selectProject(projectId);
    return;
  }
  const previous = ui.get().projectId;
  let offState = () => {};
  let offUi = () => {};
  const stop = () => { offState(); offUi(); };
  const arrived = () => {
    if (!session.state.get().projects[projectId]) return;
    const unchanged = ui.get().projectId === previous;
    stop();
    // Do not override a different project the person chose while waiting.
    if (unchanged) ui.selectProject(projectId);
  };
  offState = session.state.subscribe(arrived);
  offUi = ui.subscribe(() => { if (ui.get().projectId !== previous) stop(); });
  // Cover an event delivered between the first check and the subscription.
  arrived();
}
