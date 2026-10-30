// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../../app/ui";
import { Store } from "../../state/store";

/** What Control shows under its segments. Kept outside the screen so other
 * screens can open it at one of them. */
export type ControlPart = "screens" | "terminals" | "previews";
export const controlPart = new Store<ControlPart>("screens");

/** Control, at its Previews: where previews are added and removed. */
export function showPreviews(ui: UiStore): void {
  controlPart.set("previews");
  ui.reset("control");
}
