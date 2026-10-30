// SPDX-License-Identifier: Apache-2.0
// What the receipt task's agent showed through Ophio: a picture of its work,
// a short walk-through video to talk over, the preview it started and a
// request for a capture. All of it is drawn here; nothing is real.

import type { ArtifactMeta, Run, Shown } from "../../protocol/types";
import { webm, type VideoPacket } from "../../screens/annotate/webm";
import type { SimHost } from "./host";
import { paintDesktop } from "./annotate";

export const CHECKOUT_PREVIEW = "sim-preview-checkout";

export async function seedShown(
  host: SimHost,
  run: Run,
  projectId: string,
  picture: Blob,
): Promise<void> {
  const add = async (blob: Blob, name: string) => {
    const bytes = new Uint8Array(await blob.arrayBuffer());
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
    const sha = [...digest].map((b) => b.toString(16).padStart(2, "0")).join("");
    return host.addArtifact(bytes, blob.type, sha, name, run.task_id, undefined, [
      { task_id: run.task_id, run_id: run.run_id, annotation: null, role: "shown" },
    ]);
  };
  const show = (shown: Shown, at: number) => host.emit({
    type: "timeline_appended",
    task_id: run.task_id,
    item: { seq: host.seq + 1, at, run_id: run.run_id, item: { kind: "shown", shown } },
  });
  const media = (a: ArtifactMeta, path: string, caption: string, invite: "draw" | "talk" | null): Shown =>
    ({ what: "media", artifact_id: a.artifact_id, media_type: a.media_type, path, caption, invite });
  const at = (minutes: number) => run.admitted_at + minutes * 60_000;

  const after = await add(picture, "checkout-after.png");
  show(media(after, "screenshots/checkout-after.png", "The heading has room above it now. Mark anything that still looks cramped.", "draw"), at(14));

  const clip = await walkthrough().catch(() => null);
  if (clip) {
    const video = await add(clip, "checkout-walkthrough.webm");
    show(media(video, "screenshots/checkout-walkthrough.webm", "Here’s the flow from cart to confirmation. Talk over anything that feels off.", "talk"), at(15));
  }

  host.previews = [
    ...host.previews.filter((p) => p.preview_id !== CHECKOUT_PREVIEW),
    {
      preview_id: CHECKOUT_PREVIEW,
      project_id: projectId,
      label: "Checkout",
      url: "http://127.0.0.1:5173",
      reachable: true,
      checked_at: Date.now(),
    },
  ];
  show({
    what: "preview", preview_id: CHECKOUT_PREVIEW, label: "Checkout", url: "http://127.0.0.1:5173", note: "It’s running, so you can try the new flow yourself.",
    questions: [
      {
        header: "Pay button", question: "Where should the Pay button sit?",
        options: [
          { label: "Under the total", description: "Where it is now, after the order summary." },
          { label: "Pinned to the bottom", description: "Always in reach while the summary scrolls." },
        ],
      },
      { header: "Anything else", question: "Anything else feel off as you go through it?", options: [] },
    ],
  }, at(16));
  show({ what: "invitation", action: "capture", note: "Could you capture the checkout page as it looks on your screen? I’d like to check the spacing at your window size." }, at(17));
  show({ what: "invitation", action: "computer_use", note: "Want an agent to try checkout in a real browser on your screen while you watch?", goal: "Buy the cheapest item with the test card and confirm the receipt page shows it" }, at(18));
}

/** A four-second walk-through with a finite duration, as an agent's
 * recording would have; MediaRecorder's has none. */
async function walkthrough(): Promise<Blob | null> {
  if (typeof VideoEncoder === "undefined") return null;
  const width = 640, height = 360, fps = 10, durationMs = 4000;
  const config: VideoEncoderConfig = { codec: "vp8", width, height, bitrate: 1_000_000, framerate: fps };
  if (!(await VideoEncoder.isConfigSupported(config)).supported) return null;
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  const packets: VideoPacket[] = [];
  const step = 1000 / fps;
  let failure: unknown = null;
  const encoder = new VideoEncoder({
    output: (chunk) => {
      const bytes = new Uint8Array(chunk.byteLength);
      chunk.copyTo(bytes);
      packets.push({ timestamp: Math.round(chunk.timestamp / 1000), duration: step, key: chunk.type === "key", bytes });
    },
    error: (e) => {
      failure = e;
    },
  });
  encoder.configure(config);
  for (let i = 0; i * step < durationMs; i++) {
    paintDesktop(ctx, width, height, i / fps, "Checkout walk-through", false);
    ctx.fillStyle = "#ffffff";
    ctx.font = "bold 22px sans-serif";
    ctx.fillText(["Cart", "Shipping", "Payment", "Confirmation"][Math.floor(i / fps)] ?? "", 24, height - 24);
    const frame = new VideoFrame(canvas, { timestamp: i * step * 1000, duration: step * 1000 });
    try {
      encoder.encode(frame, { keyFrame: i % fps === 0 });
    } finally {
      frame.close();
    }
  }
  await encoder.flush();
  encoder.close();
  if (failure) return null;
  return webm(packets, width, height, durationMs);
}
