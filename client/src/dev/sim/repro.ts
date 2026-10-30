// SPDX-License-Identifier: Apache-2.0
// Browser-only authored footage. No screen, microphone, network or agent capture.
import type { App } from "../../app/context";
import { annotationStore } from "../../state/annotations";
import { webm, type VideoPacket } from "../../screens/annotate/webm";

export async function fixtureClip(): Promise<File> {
  const canvas = document.createElement("canvas");
  canvas.width = 480;
  canvas.height = 270;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Fixture canvas unavailable");
  const packets: VideoPacket[] = [];
  let failure: Error | null = null;
  const encoder = new VideoEncoder({
    output: (chunk) => {
      const bytes = new Uint8Array(chunk.byteLength);
      chunk.copyTo(bytes);
      const timestamp = Math.round(chunk.timestamp / 1000);
      packets.push({
        timestamp,
        duration:
          Math.min(8000, Math.round(((packets.length + 1) * 1000) / 15)) -
          timestamp,
        bytes,
        key: chunk.type === "key",
      });
    },
    error: (error) => {
      failure = error;
    },
  });
  try {
    encoder.configure({
      codec: "vp8",
      width: 480,
      height: 270,
      bitrate: 500_000,
      framerate: 15,
    });
    for (let i = 0; i < 120; i++) {
      const ms = Math.round((i * 1000) / 15),
        next = Math.round(((i + 1) * 1000) / 15);
      ctx.fillStyle = "#f7f6f2";
      ctx.fillRect(0, 0, 480, 270);
      ctx.fillStyle = "#19283b";
      ctx.font = "bold 24px sans-serif";
      ctx.fillText("Project activity", 24, 45);
      ctx.font = "16px sans-serif";
      ctx.fillText(`Frame ${i + 1} · ${(ms / 1000).toFixed(2)} s`, 24, 76);
      ctx.fillStyle = "#d3dce6";
      ctx.fillRect(24, 104, 432, 136);
      ctx.fillStyle = ms >= 3500 ? "#99513e" : "#345f7c";
      ctx.fillRect(38 + (Math.round(ms / 80) % 150), 135, 160, 48);
      ctx.fillStyle = "#ffffff";
      ctx.fillText(
        ms >= 3500 ? "Menu hidden" : "Open menu",
        55 + (Math.round(ms / 80) % 150),
        165,
      );
      const frame = new VideoFrame(canvas, {
        timestamp: ms * 1000,
        duration: (next - ms) * 1000,
      });
      encoder.encode(frame, { keyFrame: i % 15 === 0 });
      frame.close();
      if (encoder.encodeQueueSize >= 4) await encoder.flush();
    }
    await encoder.flush();
    if (failure) throw failure;
    return new File([webm(packets, 480, 270, 8000)], "activity.webm", {
      type: "video/webm",
    });
  } finally {
    if (encoder.state !== "closed") encoder.close();
  }
}

export async function openDraft(app: App): Promise<string> {
  await waitFor(
    () => Object.keys(app.session.state.get().projects).length > 0,
    app.session.state.subscribe,
    "Fixture project did not arrive",
  );
  const project = Object.values(app.session.state.get().projects)[0];
  if (!project) throw new Error("Fixture project unavailable");
  const draft = annotationStore(app.profile.profileId).create(
    project.project_id,
    null,
  );
  annotationStore(app.profile.profileId).record(draft.id, (d) => ({
    ...d,
    mode: "steps",
  }));
  app.ui.selectTask(project.project_id, null);
  app.ui.push({ screen: "annotate", annotationId: draft.id });
  await clipPicker();
  return draft.id;
}
export async function importFixture(): Promise<void> {
  const input = await clipPicker();
  const transfer = new DataTransfer();
  transfer.items.add(await fixtureClip());
  input.files = transfer.files;
  input.dispatchEvent(new Event("change", { bubbles: true }));
}

async function clipPicker(): Promise<HTMLInputElement> {
  const selector = 'input[aria-label="Clip file"]';
  await waitFor(
    () => !!document.querySelector(selector),
    (listener) => {
      const observer = new MutationObserver(listener);
      observer.observe(document.body, { childList: true, subtree: true });
      return () => observer.disconnect();
    },
    "Clip picker did not open",
  );
  const input = document.querySelector<HTMLInputElement>(selector);
  if (!input) throw new Error("Clip picker unavailable");
  return input;
}
export function undecodableFixture(): void {
  const input = document.querySelector<HTMLInputElement>(
    'input[aria-label="Clip file"]',
  );
  if (!input) throw new Error("Clip picker unavailable");
  const transfer = new DataTransfer();
  transfer.items.add(
    new File(["invalid video bytes"], "broken.mp4", { type: "video/mp4" }),
  );
  input.files = transfer.files;
  input.dispatchEvent(new Event("change", { bubbles: true }));
}
export async function mutateClip(app: App, id: string): Promise<void> {
  const store = annotationStore(app.profile.profileId);
  const source = store
    .get()
    [id]?.sources.filter((s) => s.derivation?.kind === "trim")
    .at(-1);
  if (!source) throw new Error("No exported clip to mutate");
  await new Promise<void>((resolve, reject) => {
    const request = indexedDB.open("annotation-sources", 1);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      const transaction = db.transaction("blobs", "readwrite");
      transaction
        .objectStore("blobs")
        .put(
          new Blob(["changed after review"], { type: source.mediaType }),
          source.blobKey ?? "missing",
        );
      transaction.oncomplete = () => {
        db.close();
        resolve();
      };
      transaction.onerror = () => {
        db.close();
        reject(transaction.error);
      };
    };
  });
}
export async function assertions(app: App, id: string): Promise<void> {
  const store = annotationStore(app.profile.profileId);
  await waitFor(
    () => !!store.get()[id]?.sources.some((s) => s.derivation?.kind === "trim"),
    store.subscribe,
    "Clip creation did not finish",
  );
  const draft = annotationStore(app.profile.profileId).get()[id];
  const clip = draft?.sources.find((s) => s.derivation?.kind === "trim");
  if (
    !draft ||
    !clip ||
    !clip.sha256 ||
    clip.durationMs !== 3000 ||
    !clip.byteSize ||
    clip.clipTransfer
  )
    throw new Error("Expected an unsent three-second clip with hash and size");
  const frames = draft.sources.filter(
    (s) => s.reproduction?.method === "frame",
  );
  if (
    frames.length !== 3 ||
    frames.some(
      (s) =>
        s.derivation?.kind !== "frame_from_clip" ||
        s.derivation.at_ms < 2000 ||
        s.derivation.at_ms >= 5000 ||
        !s.sha256 ||
        !s.reproduction?.sourceSha256 ||
        !s.reproduction.sourceSize,
    )
  )
    throw new Error("Frame identities or selected times are missing");
}

/** Wait for actual fixture state, including on a loaded machine. */
function waitFor(
  ready: () => boolean,
  subscribe: (listener: () => void) => () => void,
  message: string,
): Promise<void> {
  if (ready()) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      stop();
      reject(new Error(message));
    }, 20_000);
    const stop = subscribe(() => {
      if (ready()) {
        clearTimeout(timeout);
        stop();
        resolve();
      }
    });
  });
}
