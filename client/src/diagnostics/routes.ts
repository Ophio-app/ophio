// SPDX-License-Identifier: Apache-2.0
import type { Route, UiState } from "../app/ui";
import { enumValues } from "./stateSchema";
const screens = new Set([...(enumValues.screen ?? []), "welcome", "startup", "unknown"]);
export function routePattern(route: Route | string): string {
  if (typeof route !== "string") return screens.has(route.screen) ? `/${route.screen}` : "/unknown";
  const pattern = route.startsWith("/") ? route : `/${route}`;
  return /^\/[a-z_]+(?:\/:\w+)*$/.test(pattern) && screens.has(pattern.split("/")[1]!) ? pattern : "/unknown";
}
export const screenOn = (ui: UiState): Route => ui.stacks[ui.tab]?.at(-1) ?? { screen: ui.tab };
