// SPDX-License-Identifier: Apache-2.0
//
// What an agent showed the person through Ophio, where it appeared in the
// conversation: its picture or video with the way to answer over it, the
// preview it opened, or what it asked the person to do, the checklist too. The agent offered;
// each action here is the person's to take.

import { AudioLines, Bot, Camera, FolderUp, ListChecks, Monitor, Pencil, Play, Mic } from "lucide-react";
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { draftKey } from "../../app/ui";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { clockTime } from "../../design/time";
import type { PersonAction, Shown } from "../../protocol/types";
import { annotationStore } from "../../state/annotations";
import { suggestGoal } from "../control/computerReview";
import { PreviewCard, samePage } from "../control/Previews";
import { PendingMessageError } from "../control/previewAnnotation";
import { transferComposer } from "../transfers/composer";
import { acquirePicture, usePicture } from "../work/pictures";
import { shownAnnotation } from "./shownAnnotation";
import { PreviewAsked, usePreviewAsked } from "./PreviewAsked";
import "./shown.css";

type Media = Extract<Shown, { what: "media" }>;

export function ShownCard({ shown, taskId, at }: { shown: Shown; taskId: string; at: number }) {
  const projectId = useHost((s) => s.tasks[taskId]?.project_id ?? null);
  if (!projectId) return null;
  switch (shown.what) {
    case "media":
      return <ShownMedia shown={shown} taskId={taskId} projectId={projectId} at={at} />;
    case "preview":
      return <ShownPreview shown={shown} taskId={taskId} projectId={projectId} at={at} />;
    case "invitation":
      return <Invitation action={shown.action} note={shown.note} goal={shown.goal} taskId={taskId} projectId={projectId} at={at} />;
    case "checklist":
      return <ShownChecklist shown={shown} projectId={projectId} at={at} />;
  }
}

/** What the agent put on the checklist for the person, opened in place
 * rather than written out here. */
function ShownChecklist({ shown, projectId, at }: { shown: Extract<Shown, { what: "checklist" }>; projectId: string; at: number }) {
  const { ui } = useApp();
  const n = shown.items.length;
  const what = n === 1 ? `“${shown.items[0]}”` : `${n} items`;
  const waits = shown.waiting === 0 ? null
    : shown.waiting === n ? (n === 1 ? "The agent carries on once it’s done." : "The agent carries on as they’re done.")
      : `The agent carries on once ${shown.waiting === 1 ? "one of them is" : `${shown.waiting} of them are`} done.`;
  return (
    <div className="talk-shown talk-shown--ask">
      <p className="talk-shown__caption">
        <span className="talk-shown__text">{`Added ${what} to ${shown.title}, with every step.`}</span>
        {n > 1 && <span className="talk-shown__text">{shown.items.slice(0, 4).join(" · ")}{n > 4 ? ` and ${n - 4} more` : ""}</span>}
        {waits && <span className="talk-shown__text">{waits}</span>}
        <span className="talk-shown__meta">{`The agent asked · ${clockTime(at)}`}</span>
      </p>
      <div className="talk-shown__actions">
        <Button size="sm" icon={ListChecks} variant={shown.waiting ? "primary" : "secondary"}
          onClick={() => ui.openChecklist(projectId)}>Open the checklist</Button>
      </div>
    </div>
  );
}

/** Opens a picture or video the agent showed in the task's drawing. */
export function useAnswerOver(taskId: string, projectId: string) {
  const { platform, profile, session, ui } = useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const open = async (artifact: { artifact_id: string; media_type: string; created_at: number }) => {
    setBusy(true);
    setError(null);
    const handle = acquirePicture({ platform, session, profileId: profile.profileId }, artifact.artifact_id);
    try {
      const picture = await handle.promise;
      const id = await shownAnnotation(ui, profile.profileId, { projectId, taskId }, artifact, picture.blob);
      ui.push({ screen: "annotate", annotationId: id });
    } catch (e) {
      setError(e instanceof PendingMessageError ? e.message : describeError(e, profile.name));
    } finally {
      handle.release();
      setBusy(false);
    }
  };
  return { open, busy, error };
}

function ShownMedia({ shown, taskId, projectId, at }: { shown: Media; taskId: string; projectId: string; at: number }) {
  const video = shown.media_type.startsWith("video/");
  const [watching, setWatching] = useState(false);
  const picture = usePicture(!video || watching ? shown.artifact_id : undefined);
  const answer = useAnswerOver(taskId, projectId);
  const name = shown.path.split("/").at(-1) || shown.path;
  const artifact = { artifact_id: shown.artifact_id, media_type: shown.media_type, created_at: at };
  // A video is answered by talking while it plays; a picture by drawing on it.
  const answerButton = <Button size="sm" icon={video ? Mic : Pencil} variant={shown.invite ? "primary" : "secondary"}
    pending={answer.busy} disabled={answer.busy} onClick={() => void answer.open(artifact)}>{video ? "Talk over it" : "Draw on it"}</Button>;
  return (
    <figure className="talk-shown">
      {picture.error ? (
        <Notice tone="unknown" action={!picture.gone && <Button size="sm" onClick={picture.retry}>Try again</Button>}>{picture.error}</Notice>
      ) : video && !watching ? (
        <button type="button" className="talk-shown__poster" onClick={() => setWatching(true)} aria-label={`Watch ${name}`}>
          <Play aria-hidden strokeWidth={2.1} />
        </button>
      ) : picture.picture ? (
        video
          ? <video className="talk-shown__media" src={picture.picture.url} controls playsInline autoPlay />
          : <img className="talk-shown__media" src={picture.picture.url} alt={shown.caption ?? name} />
      ) : (
        <div className="talk-shown__poster" role="status" aria-label="Loading">
          <span className="stateline__spinner" aria-hidden />
        </div>
      )}
      <figcaption className="talk-shown__caption">
        {shown.caption && <span className="talk-shown__text">{shown.caption}</span>}
        <span className="talk-shown__meta">{`${video ? "Video" : "Picture"} from the agent · ${name} · ${clockTime(at)}`}</span>
      </figcaption>
      {!picture.gone && <div className="talk-shown__actions">{answerButton}</div>}
      {answer.error && <Notice tone="stop">{answer.error}</Notice>}
    </figure>
  );
}

function ShownPreview({ shown, taskId, projectId, at }: { shown: Extract<Shown, { what: "preview" }>; taskId: string; projectId: string; at: number }) {
  const { session } = useApp();
  const asked = usePreviewAsked(taskId, at, shown.questions ?? []);
  const link = useLink();
  const loaded = useHost((s) => projectId in s.previews);
  // A found preview whose server stopped a while is found anew when it starts.
  const endpoint = useHost((s) => {
    const previews = s.previews[projectId];
    return previews?.find((p) => p.preview_id === shown.preview_id) ?? previews?.find((p) => samePage(p.url, shown.url));
  });
  // Talk may open before Control has read this project's previews.
  useEffect(() => {
    if (loaded || link.state !== "online") return;
    let alive = true;
    void session.query({ kind: "previews", project_id: projectId }).then((r) => {
      if (alive) session.state.set((s) => projectId in s.previews ? s : { ...s, previews: { ...s.previews, [projectId]: r.previews } });
    }, () => undefined);
    return () => { alive = false; };
  }, [loaded, link.state, session, projectId]);
  return (
    <div className="talk-shown talk-shown--preview">
      <p className="talk-shown__caption">
        {shown.note && <span className="talk-shown__text">{shown.note}</span>}
        <span className="talk-shown__meta">{`Preview from the agent · ${shown.label} · ${clockTime(at)}`}</span>
      </p>
      {endpoint ? <PreviewCard preview={endpoint} ask={asked.ask} /> : loaded && (
        <p className="fine">{`${shown.url} is no longer one of this project’s previews.`}</p>
      )}
      {asked.questions.length > 0 && <PreviewAsked asked={asked} taskId={taskId} label={shown.label} />}
    </div>
  );
}

const ASKED: Record<PersonAction, { label: string; icon: typeof Camera }> = {
  watch_desktop: { label: "Watch the computer", icon: Monitor },
  capture: { label: "Capture the screen", icon: Camera },
  send_files: { label: "Send files", icon: FolderUp },
  computer_use: { label: "Start computer use", icon: Bot },
  voice_control: { label: "Control by voice", icon: AudioLines },
};

function Invitation({ action, note, goal, taskId, projectId, at }: { action: PersonAction; note: string; goal?: string; taskId: string; projectId: string; at: number }) {
  const { profile, ui } = useApp();
  const targets = useHost((s) => s.targets);
  const home = useHost((s) => !!s.projects[projectId]?.home);
  const [error, setError] = useState<string | null>(null);
  const { label, icon } = ASKED[action];
  const go = () => {
    setError(null);
    switch (action) {
      case "watch_desktop": {
        const target = targets.find((t) => t.kind === "desktop") ?? targets.find((t) => t.kind === "output");
        ui.push(target ? { screen: "desktop", targetId: target.target_id } : { screen: "control" }, "control");
        return;
      }
      case "capture": {
        const store = annotationStore(profile.profileId);
        const draft = ui.draft(projectId, taskId);
        if (draft?.attempt) {
          setError(new PendingMessageError(draftKey(projectId, taskId)).message);
          return;
        }
        let id = draft?.annotationId ?? null;
        if (!id || !store.get()[id]) {
          id = store.create(projectId, taskId).id;
          ui.saveDraft(projectId, taskId, { annotationId: id });
        }
        ui.requestSource(id, { kind: "screen" });
        ui.push({ screen: "annotate", annotationId: id });
        return;
      }
      case "send_files":
        // Home can't receive files from the phone: what a task there asks for goes to the Inbox.
        transferComposer(profile.profileId).update({
          destination: home ? { kind: "inbox" } : { kind: "project_dir", project_id: projectId, dir: "" },
        });
        ui.push({ screen: "transfers" });
        return;
      case "computer_use":
        if (goal) suggestGoal(profile.profileId, projectId, goal);
        ui.push({ screen: "computer_use_review", projectId }, "control");
        return;
      case "voice_control":
        ui.push({ screen: "voice" }, "control");
        return;
    }
  };
  return (
    <div className="talk-shown talk-shown--ask">
      <p className="talk-shown__caption">
        <span className="talk-shown__text">{note}</span>
        {goal && <span className="talk-shown__text">{`Goal: ${goal}`}</span>}
        <span className="talk-shown__meta">{`The agent asked · ${clockTime(at)}`}</span>
      </p>
      <div className="talk-shown__actions">
        <Button size="sm" icon={icon} onClick={go}>{label}</Button>
      </div>
      {error && <Notice tone="stop">{error}</Notice>}
    </div>
  );
}
