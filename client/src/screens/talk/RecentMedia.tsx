// SPDX-License-Identifier: Apache-2.0
//
// The task's pictures and videos in one place, newest first: what the agent
// showed, what the person sent and what was captured along the way. One
// opens large, to draw or talk over, or to delete from the computer.

import { ArrowLeft, Film, Mic, Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { COLLABORATION_FEATURE } from "../../app/collaborationFeature";
import { describeError, shallowEqual, useApp, useHost } from "../../app/context";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { ArtifactMeta } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { usePicture } from "../work/pictures";
import { useTaskArtifacts } from "../work/useTaskArtifacts";
import { useAnswerOver } from "./Shown";
import "./shown.css";

const PAGE = 12;

function taskMedia(s: HostState, taskId: string): ArtifactMeta[] {
  return Object.values(s.artifacts)
    .filter((a) => (a.task_id === taskId || a.links?.some((l) => l.task_id === taskId)) && /^(image|video)\//.test(a.media_type))
    .sort((a, b) => b.created_at - a.created_at);
}

/** Whether the agent showed it to the person, wherever in the conversation. */
function fromAgent(a: ArtifactMeta) {
  return !!a.links?.some((l) => l.role === "shown");
}

export function useMediaPermitted(): boolean {
  return useHost((s) => s.grants === null || (s.grants.tasks && s.grants.transfers));
}

export function MediaSheet({ open, onClose, taskId, projectId }: { open: boolean; onClose: () => void; taskId: string; projectId: string }) {
  return (
    <Sheet open={open} onClose={onClose} title="Pictures and videos" subtitle="In this task, newest first" size="tall">
      {open && <MediaList taskId={taskId} projectId={projectId} />}
    </Sheet>
  );
}

function MediaList({ taskId, projectId }: { taskId: string; projectId: string }) {
  const [retry, setRetry] = useState(0);
  const loading = useTaskArtifacts(taskId, true, retry);
  const media = useHost((s) => taskMedia(s, taskId), shallowEqual);
  const [shownCount, setShownCount] = useState(PAGE);
  const [selected, setSelected] = useState<string | null>(null);
  const picked = media.find((a) => a.artifact_id === selected);
  if (picked) return <MediaDetail artifact={picked} taskId={taskId} projectId={projectId} onBack={() => setSelected(null)} />;
  return (
    <div className="talk-media">
      {loading.error && <Notice tone="stop" action={<Button size="sm" onClick={() => setRetry((n) => n + 1)}>Try again</Button>}>{loading.error}</Notice>}
      {!media.length && (loading.loading
        ? <p className="fine" role="status">Reading this task’s pictures…</p>
        : !loading.error && <p className="fine">No pictures or videos in this task yet. When the agent shows you one, it’s kept here too.</p>)}
      <ul className="talk-media__grid">
        {media.slice(0, shownCount).map((a) => (
          <li key={a.artifact_id}>
            <button type="button" className="talk-media__tile" onClick={() => setSelected(a.artifact_id)}
              aria-label={`${provenance(a)}: ${a.label ?? kind(a)}`}>
              <Thumbnail artifact={a} />
              <span className="talk-media__tag">{fromAgent(a) ? "Agent" : provenance(a)}</span>
            </button>
          </li>
        ))}
      </ul>
      {media.length > shownCount && <Button block onClick={() => setShownCount((n) => n + PAGE)}>Show more</Button>}
    </div>
  );
}

function kind(a: ArtifactMeta) {
  return a.media_type.startsWith("video/") ? "Video" : "Picture";
}

function provenance(a: ArtifactMeta) {
  if (fromAgent(a)) return "From the agent";
  if (a.links?.some((l) => l.role === "source")) return "You sent";
  return "Captured";
}

export function Thumbnail({ artifact: a }: { artifact: ArtifactMeta }) {
  const video = a.media_type.startsWith("video/");
  const picture = usePicture(video ? undefined : a.artifact_id, a.sha256);
  if (video) return <span className="talk-media__thumb talk-media__thumb--video"><Film aria-hidden strokeWidth={2.1} /></span>;
  return picture.picture
    ? <img className="talk-media__thumb" src={picture.picture.url} alt="" />
    : <span className="talk-media__thumb" />;
}

export function MediaDetail({ artifact: a, taskId, projectId, onBack }: { artifact: ArtifactMeta; taskId: string; projectId: string; onBack: () => void }) {
  const { session, profile } = useApp();
  const deletes = useHost((s) => !!s.capabilities?.features.includes(COLLABORATION_FEATURE));
  const video = a.media_type.startsWith("video/");
  const sent = !!a.links?.some((l) => l.role === "source");
  const picture = usePicture(a.artifact_id, a.sha256);
  const answer = useAnswerOver(taskId, projectId);
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const remove = async () => {
    setDeleting(true);
    setError(null);
    try {
      await session.command({ command: "delete_artifact", artifact_id: a.artifact_id }, { label: "Delete picture" });
      onBack();
    } catch (e) {
      setError(describeError(e, profile.name));
      setConfirming(false);
    } finally {
      setDeleting(false);
    }
  };
  return (
    <div className="talk-media talk-shown">
      <Button size="sm" variant="quiet" icon={ArrowLeft} onClick={onBack} className="talk-media__back">All pictures and videos</Button>
      {picture.error
        ? <Notice tone="unknown" action={<Button size="sm" onClick={picture.retry}>Try again</Button>}>{picture.error}</Notice>
        : picture.picture
          ? video
            ? <video className="talk-shown__media" src={picture.picture.url} controls playsInline />
            : <img className="talk-shown__media" src={picture.picture.url} alt={a.label ?? kind(a)} />
          : <div className="talk-shown__poster" role="status" aria-label="Loading"><span className="stateline__spinner" aria-hidden /></div>}
      <p className="talk-shown__caption">
        <span className="talk-shown__meta">
          {`${provenance(a)}${a.label ? ` · ${a.label}` : ""} · `}<Age at={a.created_at} suffix=" ago" />
        </span>
      </p>
      {confirming ? (
        <Notice tone="warn" title={`Delete this ${video ? "video" : "picture"} from ${profile.name}?`}
          action={<div className="talk-shown__actions">
            <Button size="sm" variant="stop" icon={Trash2} pending={deleting} disabled={deleting} onClick={() => void remove()}>Delete</Button>
            <Button size="sm" disabled={deleting} onClick={() => setConfirming(false)}>Keep</Button>
          </div>}>
          It can’t be brought back.
        </Notice>
      ) : (
        <div className="talk-shown__actions">
          {/* Only what the agent showed goes back to it as what it showed. */}
          {fromAgent(a) && <Button size="sm" icon={video ? Mic : Pencil} variant="primary" pending={answer.busy} disabled={answer.busy}
            onClick={() => void answer.open(a)}>{video ? "Talk over it" : "Draw on it"}</Button>}
          {/* What the person sent stays with their message. */}
          {deletes && !sent && <Button size="sm" variant="stop-outline" icon={Trash2} onClick={() => setConfirming(true)}>Delete</Button>}
        </div>
      )}
      {(answer.error ?? error) && <Notice tone="stop">{answer.error ?? error}</Notice>}
    </div>
  );
}
