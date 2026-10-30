// SPDX-License-Identifier: Apache-2.0
//
// One project's Needs you on its own, where the note that counts the
// project's alerts leads: everything it alerted about that nobody has looked
// at yet.

import { useEffect } from "react";
import { useApp, useHost } from "../../app/context";
import { Empty } from "../../design/ListRow";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { NeedsYouList } from "./WorkRoot";
import "./work.css";

export default function NeedsYou({ route }: RouteProps<"needs_you">) {
  const { ui } = useApp();
  const project = useHost((s) => s.projects[route.projectId]);
  const known = !!project;
  // Back from here is the same project's Work.
  useEffect(() => { if (known) ui.selectProject(route.projectId); }, [ui, route.projectId, known]);
  return (
    <Screen title="Needs you" subtitle={project?.name}>
      <div className="work-part">
        {project ? <NeedsYouList projectId={route.projectId} /> : <Empty title="This project isn’t on the computer now." />}
      </div>
    </Screen>
  );
}
