// SPDX-License-Identifier: Apache-2.0
import { Channel, invoke as nativeInvoke } from "@tauri-apps/api/core";
import type { Picked } from "../design/ApprovalQuestions";
import type { ApprovalQuestion } from "../protocol/types";

async function invoke<T>(
  command: string,
  args: Record<string, unknown>,
): Promise<T> {
  try {
    return await nativeInvoke<T>(command, args);
  } catch (error) {
    if (
      error &&
      typeof error === "object" &&
      "message" in error &&
      typeof error.message === "string"
    ) {
      // The code says why the computer refused, e.g. an unchecked preview.
      const code = "code" in error && typeof error.code === "string" ? error.code : undefined;
      throw Object.assign(new Error(error.message), code && { code });
    }
    throw error;
  }
}
export type PreviewState =
  | { state: "open"; connected_address: string }
  | { state: "closed"; reason: string };
export interface PreviewPicture {
  id?: string;
  taskId?: string | null;
  capturedAt?: number;
  blob: Blob;
  /** The person chose Annotate for this picture before the preview closed. */
  annotate?: boolean;
  acknowledge(): Promise<void>;
}
export interface PreviewHandle {
  /** Resolves on close with all captures; older handles returned one picture. */
  show(): Promise<PreviewPicture[] | PreviewPicture | null>;
  close(): Promise<void>;
  /** A fixture may display a local document in the screen’s unprivileged frame. */
  document?: string;
  picture?(): Promise<PreviewPicture>;
}
/** What the agent asked about the page, answered beside it. */
export interface PreviewAsk {
  questions: ApprovalQuestion[];
  /** The answers so far, to carry on from. */
  answers: Picked[];
  /** The answers as the preview closed; `submitted` when the person finished. */
  answered(answers: Picked[], submitted: boolean): void;
}
/** Only answers shaped like the questions are taken back. */
export function pickedFrom(value: unknown, questions: ApprovalQuestion[]): Picked[] | null {
  if (!Array.isArray(value) || value.length !== questions.length) return null;
  const picked: Picked[] = [];
  for (const [i, a] of value.entries()) {
    if (!a || typeof a !== "object" || !Array.isArray(a.labels) || typeof a.own !== "string") return null;
    const offered = questions[i]!.options.map(o => o.label);
    const labels = a.labels.filter((l: unknown): l is string => typeof l === "string" && offered.includes(l));
    picked.push({ labels: questions[i]!.multi_select ? labels : labels.slice(0, 1), own: a.own.slice(0, 4000) });
  }
  return picked;
}
interface PictureDescriptor {
  id: string;
  size: number;
  taskId?: string | null;
  capturedAt?: number | null;
}
async function readPicture(
  picture: PictureDescriptor,
): Promise<PreviewPicture> {
  if (
    !Number.isSafeInteger(picture.size) ||
    picture.size <= 0 ||
    picture.size > 32 * 1024 * 1024
  )
    throw new Error("The preview picture is too large to read.");
  const chunks: ArrayBuffer[] = [];
  let offset = 0;
  while (offset < picture.size) {
    const bytes = await invoke<ArrayBuffer>("preview_picture_read", {
      id: picture.id,
      offset,
    });
    if (
      !bytes.byteLength ||
      bytes.byteLength > Math.min(256 * 1024, picture.size - offset)
    )
      throw new Error("The preview picture could not be read completely.");
    chunks.push(bytes);
    offset += bytes.byteLength;
  }
  return {
    id: picture.id,
    taskId: picture.taskId,
    capturedAt: picture.capturedAt ?? undefined,
    blob: new Blob(chunks, { type: "image/png" }),
    acknowledge: () => invoke<void>("preview_picture_ack", { id: picture.id }),
  };
}
export async function pendingPreviews(
  profileId: string,
  previewId: string,
): Promise<PreviewPicture[]> {
  const response = await invoke<{ pictures: PictureDescriptor[] }>(
    "preview_pending",
    { profileId, previewId },
  );
  return Promise.all(response.pictures.map(readPicture));
}
export async function nativePreview(
  profileId: string,
  previewId: string,
  path: string,
  changed: (state: PreviewState) => void,
  taskId?: string | null,
  ask?: PreviewAsk,
): Promise<PreviewHandle> {
  const events = new Channel<PreviewState>();
  events.onmessage = changed;
  const id = await invoke<string>("preview_open", {
    profileId,
    previewId,
    path,
    events,
  });
  return {
    close: () => invoke<void>("preview_close", { id }),
    async show() {
      const result = await invoke<{ pictures: PictureDescriptor[]; annotate?: string | null; answers?: unknown; submitted?: boolean } | PictureDescriptor | null>("preview_show", {
        id,
        profileId,
        previewId,
        taskId: taskId ?? null,
        questions: ask?.questions.length ? ask.questions : null,
        answers: ask?.questions.length ? ask.answers : null,
      });
      const answers = ask && result && "pictures" in result ? pickedFrom(result.answers, ask.questions) : null;
      if (ask && answers) ask.answered(answers, result && "pictures" in result ? result.submitted === true : false);
      const pictures = !result ? [] : "pictures" in result ? result.pictures : [result];
      const chosen = result && "pictures" in result ? result.annotate : null;
      return Promise.all(pictures.map(async picture => ({
        ...await readPicture({ ...picture, taskId: taskId ?? null }),
        annotate: picture.id === chosen,
      })));
    },
  };
}
