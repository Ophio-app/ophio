// SPDX-License-Identifier: Apache-2.0
import { createContext } from "react";

/** Kept screens stay mounted, but their dialogs must leave the browser's top
 * layer when the screen is covered or its tab is hidden. */
export const SurfaceActive = createContext(true);
