// SPDX-License-Identifier: Apache-2.0
//
// The capture inbox's part the agents fill: the pictures and videos they
// showed in the project, newest first, and its pages. Each stays a week
// after the person last opened it; a page whose server stopped can be
// started again from its card.

import { Image as ImageIcon } from "lucide-react";
import { useState } from "react";
import { useApp, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { NotAllowed } from "../../shell/NotAllowed";
import { PreviewCard } from "../control/Previews";
import { MediaDetail, Thumbnail } from "../talk/RecentMedia";
import { shownIn, useProjectPages, useProjectShown } from "./useProjectShown";
import "../talk/shown.css";

const PAGE = 24;

export function InboxShown({ projectId }: { projectId: string }) {
  const { profile } = useApp();
  const project = useHost((s) => s.projects[projectId]?.name ?? "this project");
  const [retry, setRetry] = useState(0);
  const shown = useProjectShown(projectId, retry);
  const pages = useProjectPages(projectId);
  const [count, setCount] = useState(PAGE);
  const [selected, setSelected] = useState<string | null>(null);
  const picked = shown.media.find((a) => a.artifact_id === selected);
  return (
    <Section title={`From agents in ${project}`} id="inbox-from-agents">
      <div className="work-stack">
        <p className="work-muted">
          What agents showed you. {profile.name} keeps each one for a week after
          you last open it.
        </p>
        {!shown.supported && (
          <Notice tone="unknown">
            Update Øphio on {profile.name} to list pictures and videos from
            tasks that have ended.
          </Notice>
        )}
        {shown.permitted === false && (
          <NotAllowed what="load task pictures" permission="tasks transfers" />
        )}
        {shown.error && (
          <Notice tone="stop" action={<Button size="sm" onClick={() => setRetry((n) => n + 1)}>Try again</Button>}>
            {shown.error}
          </Notice>
        )}
        {!shown.media.length && !pages.length && (shown.loading
          ? <p className="fine" role="status">Reading what agents showed…</p>
          : (
            <Empty icon={ImageIcon} title="Nothing from agents yet">
              When an agent shows you a picture, a video or a page in this
              project, it’s kept here.
            </Empty>
          ))}
        {shown.media.length > 0 && (
          <div className="work-inbox-part">
            <h3>Pictures and videos</h3>
            <ul className="talk-media__grid" aria-label="Pictures and videos from agents">
              {shown.media.slice(0, count).map((a) => (
                <li key={a.artifact_id}>
                  <button type="button" className="talk-media__tile" onClick={() => setSelected(a.artifact_id)}
                    aria-label={a.label ?? (a.media_type.startsWith("video/") ? "Video" : "Picture")}>
                    <Thumbnail artifact={a} />
                  </button>
                </li>
              ))}
            </ul>
            {shown.media.length > count && <Button block onClick={() => setCount((n) => n + PAGE)}>Show more</Button>}
          </div>
        )}
        {pages.length > 0 && (
          <div className="work-inbox-part">
            <h3>Pages</h3>
            <p className="fine">
              Pages an agent showed you, kept a week after you last open one,
              and pages you added. Open checks the page’s server first; when it
              has stopped, the card says how to start it again.
            </p>
            <div className="group">
              {pages.map((p) => <PreviewCard key={p.preview_id} preview={p} compact />)}
            </div>
          </div>
        )}
      </div>
      <Sheet open={!!picked} onClose={() => setSelected(null)} title={picked?.media_type.startsWith("video/") ? "Video" : "Picture"}
        subtitle={picked?.label ?? undefined} size="tall">
        {picked && <MediaDetail artifact={picked} taskId={shownIn(picked)} projectId={projectId} onBack={() => setSelected(null)} />}
      </Sheet>
    </Section>
  );
}
