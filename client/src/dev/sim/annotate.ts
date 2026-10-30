// SPDX-License-Identifier: Apache-2.0
import type { SimHost } from "./host";
import type { CommandResult } from "../../protocol/types";
import { sha256Hex } from "../../host/transfer";
/** Explicit capture of the same simulated image used by the media link. */
export async function captureTarget(
  host: SimHost,
  id: string,
): Promise<CommandResult> {
  const target = host.targets.find((t) => t.target_id === id);
  if (!target)
    throw {
      code: "not_found",
      message: "That capture target no longer exists.",
    };
  if (target.capture.state !== "available")
    throw {
      code: "unavailable",
      message: "Capture isn’t available for that target.",
    };
  const canvas = document.createElement("canvas");
  canvas.width = target.geometry.width;
  canvas.height = target.geometry.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("The simulated image could not be rendered.");
  paintDesktop(ctx, canvas.width, canvas.height, 7, target.label, false);
  const blob = await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("PNG encoding failed."))),
      "image/png",
    ),
  );
  const bytes = new Uint8Array(await blob.arrayBuffer());
  return {
    kind: "artifact_captured",
    artifact: host.addArtifact(
      bytes,
      "image/png",
      await sha256Hex(blob),
      target.label,
      null,
      { target_id: id, geometry: { ...target.geometry }, captured_at: Date.now() },
    ),
  };
}

/** Paints a plausible desktop: a dashboard window with a moving chart. */
export function paintDesktop(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  t: number,
  label: string,
  agentActive: boolean,
): { x: number; y: number } {
  const u = w / 100;
  ctx.fillStyle = "#1d2330";
  ctx.fillRect(0, 0, w, h);
  // Top bar
  ctx.fillStyle = "#11151d";
  ctx.fillRect(0, 0, w, 2.4 * u);
  ctx.fillStyle = "#9aa4b8";
  ctx.font = `${1.3 * u}px sans-serif`;
  ctx.fillText(`${label}   1  2  3`, 1.2 * u, 1.65 * u);
  const clock = new Date();
  ctx.fillText(
    clock.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
    w - 7 * u,
    1.65 * u,
  );
  // Browser window
  const bx = 3 * u,
    by = 4 * u,
    bw = 62 * u,
    bh = h - 7 * u;
  ctx.fillStyle = "#f6f4ef";
  ctx.fillRect(bx, by, bw, bh);
  ctx.fillStyle = "#e3e0d8";
  ctx.fillRect(bx, by, bw, 3 * u);
  ctx.fillStyle = "#56514a";
  ctx.font = `${1.2 * u}px sans-serif`;
  ctx.fillText("staging.tidewater.dev/dashboard", bx + 2 * u, by + 1.9 * u);
  ctx.fillStyle = "#23201c";
  ctx.font = `bold ${1.9 * u}px sans-serif`;
  ctx.fillText("Latency (1h)", bx + 3 * u, by + 7.5 * u);
  // Chart
  const cx = bx + 3 * u,
    cy = by + 10 * u,
    cw = bw - 6 * u,
    ch = bh * 0.42;
  ctx.strokeStyle = "#d8d3c8";
  ctx.lineWidth = Math.max(1, u * 0.08);
  for (let i = 0; i <= 4; i++) {
    const y = cy + (ch * i) / 4;
    ctx.beginPath();
    ctx.moveTo(cx, y);
    ctx.lineTo(cx + cw, y);
    ctx.stroke();
  }
  const series = (phase: number, amp: number, base: number, color: string) => {
    ctx.strokeStyle = color;
    ctx.lineWidth = Math.max(1.5, u * 0.28);
    ctx.beginPath();
    for (let i = 0; i <= 60; i++) {
      const x = cx + (cw * i) / 60;
      const v =
        base +
        amp * Math.sin(i / 6 + phase + t * 0.6) +
        amp * 0.4 * Math.sin(i / 2.3 + t);
      const y = cy + ch - (v / 100) * ch;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();
  };
  series(0, 8, 30, "#3b4fd0");
  series(1.3, 14, 62, "#b26a00");
  ctx.font = `${1.2 * u}px sans-serif`;
  ctx.fillStyle = "#3b4fd0";
  ctx.fillText("p50", cx, cy + ch + 3 * u);
  ctx.fillStyle = "#b26a00";
  ctx.fillText("p95", cx + 5 * u, cy + ch + 3 * u);
  // Table rows
  ctx.fillStyle = "#56514a";
  for (let i = 0; i < 5; i++) {
    const y = cy + ch + 7 * u + i * 3.2 * u;
    if (y > by + bh - 2 * u) break;
    ctx.fillStyle = i % 2 ? "#efece5" : "#f6f4ef";
    ctx.fillRect(cx, y - 2 * u, cw, 3.2 * u);
    ctx.fillStyle = "#3b3833";
    ctx.fillText(
      ["/orders", "/payments", "/refunds", "/health", "/auth"][i]!,
      cx + u,
      y,
    );
    ctx.fillText(
      `${(20 + i * 7 + Math.round(3 * Math.sin(t + i))).toString()} ms`,
      cx + cw * 0.6,
      y,
    );
  }
  // Terminal window
  const tx = 67 * u,
    ty = 4 * u,
    tw = 30 * u,
    th = h - 7 * u;
  ctx.fillStyle = "#0f1115";
  ctx.fillRect(tx, ty, tw, th);
  ctx.font = `${1.15 * u}px monospace`;
  const lines = [
    "$ cargo test -p payments-client",
    "   Compiling payments-client",
    "running 64 tests",
    "test retry::backoff ... ok",
    "test retry::retries_on_503 ... ok",
    "test orders::create ... ok",
    "",
    "test result: ok. 64 passed",
    "$ _",
  ];
  const shown = Math.min(lines.length, 1 + Math.floor(t % 12));
  for (let i = 0; i < shown; i++) {
    ctx.fillStyle = lines[i]!.startsWith("$") ? "#7bd389" : "#c9d1d9";
    ctx.fillText(lines[i]!, tx + 1.2 * u, ty + 2.6 * u + i * 2 * u);
  }
  // Cursor path
  const x = bx + bw * (0.35 + 0.25 * Math.sin(t * 0.7));
  const y = cy + ch * (0.5 + 0.3 * Math.sin(t * 0.9 + 1));
  if (agentActive) {
    ctx.strokeStyle = "rgba(142,155,255,0.55)";
    ctx.lineWidth = Math.max(2, u * 0.25);
    ctx.beginPath();
    ctx.arc(x, y, 2.2 * u, 0, Math.PI * 2);
    ctx.stroke();
  }
  return { x, y };
}

/** Deterministic local drafts for headless layout and interaction checks. */
export async function seedAnnotation(
  host: SimHost,
  kind: "empty" | "marks" | "comments" | "clip" | "missing" = "comments",
): Promise<string> {
  const { annotationStore, blobs, markLabel } =
    await import("../../state/annotations");
  const { ids } = await import("./scenario");
  const store = annotationStore("sim-studio"),
    draft = store.create(ids.pTide, null);
  if (kind === "empty") return draft.id;
  const result = await captureTarget(host, ids.tgDp1);
  if (result.kind !== "artifact_captured")
    throw new Error("Expected capture fixture");
  const artifact = result.artifact,
    bytes = host.artifactBytes.get(artifact.artifact_id);
  if (!bytes) throw new Error("Missing capture fixture bytes");
  const blob = new Blob([new Uint8Array(bytes)], { type: "image/png" });
  const blobKey = await blobs.put(blob);
  const source = {
    key: "S1",
    blobKey: kind === "missing" ? "missing-source" : blobKey,
    artifactId: null,
    sha256: artifact.sha256,
    mediaType: "image/png",
    width: 2560,
    height: 1440,
    durationMs: null,
    origin: {
      kind: "host_capture" as const,
      target_id: ids.tgDp1,
      geometry_revision: 3,
    },
    capturedAt: Date.now() - 45000,
    derivedFrom: null,
    derivation: null,
  };
  store.update(draft.id, (d) => ({
    ...d,
    sources: [source],
    nextSource: 2,
    selectedSource: "S1",
    marks: [
      {
        kind: "ellipse" as const,
        rect: { x: 0.07, y: 0.13, w: 0.22, h: 0.09 },
      },
      { kind: "rect" as const, rect: { x: 0.65, y: 0.11, w: 0.25, h: 0.2 } },
      { kind: "rect" as const, rect: { x: 0.07, y: 0.68, w: 0.5, h: 0.2 } },
    ].map((shape, i) => ({
      mark_id: `fixture-${i}`,
      label: markLabel(i),
      source_key: "S1",
      shape,
      color: i,
      created_at: Date.now() - 1000 + i,
      deleted: false,
    })),
  }));
  if (kind === "comments")
    store.update(draft.id, (d) => ({
      ...d,
      comments: [
        {
          comment_id: "space",
          text: "Give these more breathing room.",
          mark_ids: ["fixture-0", "fixture-2"],
          origin: { kind: "typed" },
          link_origins: ["manual", "manual"],
        },
        {
          comment_id: "contrast",
          text: "Keep the color; increase contrast against the background.",
          mark_ids: ["fixture-1"],
          origin: { kind: "typed" },
          link_origins: ["manual"],
        },
      ],
    }));
  if (kind === "clip") {
    const canvas = document.createElement("canvas");
    canvas.width = 640;
    canvas.height = 360;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("Clip fixture needs canvas");
    const stream = canvas.captureStream(30),
      recorder = new MediaRecorder(stream, { mimeType: "video/webm" }),
      parts: Blob[] = [];
    const video = await new Promise<Blob>((resolve, reject) => {
      recorder.ondataavailable = (e) => parts.push(e.data);
      recorder.onerror = () => reject(new Error("Fixture recording failed"));
      recorder.onstop = () => resolve(new Blob(parts, { type: "video/webm" }));
      recorder.start();
      let frame = 0;
      const timer = setInterval(() => {
        paintDesktop(ctx, 640, 360, ++frame / 30, "Clip fixture", false);
        if (frame >= 45) {
          clearInterval(timer);
          recorder.stop();
          stream.getTracks().forEach((t) => t.stop());
        }
      }, 1000 / 30);
    });
    const key = await blobs.put(video);
    store.update(draft.id, (d) => ({
      ...d,
      marks: [],
      sources: [
        {
          ...source,
          key: "S1",
          blobKey: key,
          mediaType: "video/webm",
          artifactId: null,
          sha256: null,
          width: 640,
          height: 360,
          durationMs: 1500,
          included: false,
          origin: { kind: "clip", duration_ms: 1500, target_id: null },
        },
      ],
      mode: "steps",
    }));
  }
  return draft.id;
}
