// SPDX-License-Identifier: Apache-2.0
//
// The computer's load on a screen of its own, a door from Control's Screens.

import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { ConnectionNote } from "./Feedback";
import System from "./System";
import "./control.css";

export default function SystemScreen({ route: _route }: RouteProps<"system">) {
  return (
    <Screen className="control-screen" title="System">
      <ConnectionNote />
      <System />
    </Screen>
  );
}
