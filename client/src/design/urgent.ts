// SPDX-License-Identifier: Apache-2.0
//
// The urgent-control slot. A modal sheet makes the rest of the app inert, so
// the shell hands sheets whatever must stay reachable above them: the control
// safety strip. Design components render it without knowing what it is.
// `dismiss` closes the sheet when the urgent control navigates elsewhere;
// `except` names a target or task the sheet already offers controls for.

import { createContext, type ComponentType } from "react";

export interface UrgentExcept {
  targetId?: string;
  taskId?: string;
}

export const UrgentSlot = createContext<ComponentType<{ dismiss?: () => void; except?: UrgentExcept }> | null>(null);
