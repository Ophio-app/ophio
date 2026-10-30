// SPDX-License-Identifier: Apache-2.0
//
// What every destination component receives: its own route, narrowed.

import type { Route } from "../app/ui";

export type RouteProps<S extends Route["screen"]> = { route: Extract<Route, { screen: S }> };
