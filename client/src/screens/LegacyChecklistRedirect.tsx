// SPDX-License-Identifier: Apache-2.0
// An old saved/external checklist route must never show the retired screen.
import { useEffect } from "react";
import { useApp } from "../app/context";
import type { RouteProps } from "./types";

export default function LegacyChecklistRedirect({ route }: RouteProps<"checklist">) {
  const { ui } = useApp();
  useEffect(() => { ui.openChecklist(route.projectId, route.itemId); }, [ui, route.projectId, route.itemId]);
  return null;
}
