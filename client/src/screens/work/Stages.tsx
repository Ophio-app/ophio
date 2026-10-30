// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { History, Layers } from "lucide-react";
import { useApp, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { ConnectionNote } from "./shared";
import { StageChange } from "./StageChange";
import { stageName } from "./stages";
import { useStages } from "./useStages";
import "./work.css";
import "./stages.css";

export default function Stages({ route }: RouteProps<"stages">) {
  return <ProjectStages key={route.projectId} projectId={route.projectId} />;
}
function ProjectStages({ projectId }: { projectId: string }) {
  const { profile } = useApp();
  const [feedback, setFeedback] = useState<HTMLDivElement | null>(null);
  const online = useLink().state === "online";
  const { project, cache, current, loading, error, reload, older } = useStages(projectId);
  const name = project?.name ?? cache?.name ?? "Project";
  const history = cache?.history;
  return <Screen title="Stages" subtitle={name} className="work-screen stages-screen"
    footer={<StageChange feedbackTarget={feedback} projectId={projectId} name={name} current={current} history={history} refresh={reload} loading={loading} available={!!history && !error} />}>
    <div className="work-stack">
      <ConnectionNote />
      <div ref={setFeedback} />
      <div className="stages-current">
        <Layers aria-hidden />
        <div>
          <p className="work-caption">Current stage</p>
          <p className="stages-current-name">{project || cache ? stageName(current?.to) : "Not yet known"}</p>
          {current ? <p className="work-muted">{current.actor_name} · <Age at={current.at} suffix=" ago" /></p>
            : <p className="work-muted">{project || cache ? "No stage decision has been recorded." : "Waiting for the computer’s stage record."}</p>}
        </div>
      </div>
      <p className="work-muted">Stage changes are your decisions. Agent results never change the stage.</p>
      {cache && <p className="work-caption">History checked <Age at={cache.observedAt} suffix=" ago" />{!online && " · Last known on this device"}</p>}
      {error && <Notice tone="warn" title="History couldn’t be refreshed" action={<Button size="sm" disabled={!online || loading} onClick={reload}>Try again</Button>}>{error}</Notice>}
      {!history && loading && <p role="status">Loading stage history…</p>}
      {!history && !loading && !error && <Notice>Stage history hasn’t been loaded on this device. Reconnect to load it.</Notice>}
      {history && <Section title="History" id="stage-history">
        {history.transitions.length ? <ol className="stages-history" aria-label="Stage history, newest first">
          {history.transitions.map(t => <li key={t.revision}>
            <p className="stages-transition"><span>{t.from ? stageName(t.from) : "Not set"}</span> <span aria-hidden>→</span><span className="visually-hidden">to</span> <strong>{stageName(t.to)}</strong></p>
            <p className="work-muted">{t.actor_name}{t.principal.kind === "device" && t.principal.device_id === profile.deviceId ? " · This device" : ""}</p>
            <p className="work-caption"><time dateTime={new Date(t.at).toISOString()}>{new Date(t.at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</time></p>
            {t.note && <p className="stages-history-note">{t.note}</p>}
          </li>)}
        </ol> : <Empty icon={History} title="No stage changes yet">Choose a stage and review the change to make the first entry.</Empty>}
        {history.has_more && <Button block disabled={!online} pending={loading} onClick={older}>Load earlier changes</Button>}
      </Section>}
    </div>
  </Screen>;
}
