// SPDX-License-Identifier: Apache-2.0
import { Layers } from "lucide-react";
import { useApp } from "../../app/context";
import { Age } from "../../design/Age";
import { ListRow } from "../../design/ListRow";
import type { Project } from "../../protocol/types";
import { stageName } from "./stages";

export function StageRow({ project }: { project: Project }) {
  const { ui } = useApp();
  const stage = project.stage;
  // Named by what it shows, so a screen reader hears the stage itself. The
  // actor and separator stay one text node beside the age: one stop, not two.
  return <ListRow icon={Layers}
    title={stage ? <><span className="visually-hidden">Project stage:</span>{` ${stageName(stage.to)}`}</> : stageName(undefined)}
    subtitle={stage ? <>{`${stage.actor_name} · `}<Age at={stage.at} suffix=" ago" /></> : "Choose the project’s stage"}
    chevron onClick={() => ui.push({ screen: "stages", projectId: project.project_id })} />;
}
