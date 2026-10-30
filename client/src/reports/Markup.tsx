// SPDX-License-Identifier: Apache-2.0
import { useMemo, useRef, useState } from "react";
import { Button } from "../design/Button";
import { Sheet } from "../design/Sheet";
import { MAX_SCREENSHOT_BYTES } from "../diagnostics/constants";
import { uuidv7 } from "../protocol/ids";
import type { Mark, ReportScreenshot } from "../protocol/types";
import type { SourceDraft } from "../state/annotations";
import { Canvas } from "../screens/annotate/Canvas";
import { SHAPE_WORD, type Tool } from "../screens/annotate/canvas";
import "../screens/annotate/annotate.css";

export const pictureUrl = (picture: ReportScreenshot) => `data:${picture.media_type};base64,${picture.data}`;
const loadImage = (url: string) => new Promise<HTMLImageElement>((resolve, reject) => { const image = new Image(); image.onload = () => resolve(image); image.onerror = () => reject(new Error("The picture couldn’t be opened.")); image.src = url; });
export async function flattenPicture(picture: ReportScreenshot, svg: SVGSVGElement): Promise<ReportScreenshot> {
  const clone = svg.cloneNode(true) as SVGSVGElement;
  const image = clone.querySelector("image")!;
  const x = Number(image.getAttribute("x")), y = Number(image.getAttribute("y"));
  const width = Number(image.getAttribute("width")), height = Number(image.getAttribute("height"));
  clone.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  clone.setAttribute("viewBox", `${x} ${y} ${width} ${height}`);
  clone.setAttribute("width", String(picture.width)); clone.setAttribute("height", String(picture.height));
  clone.querySelectorAll("g").forEach(g => { g.style.color = "#b34228"; });
  clone.querySelectorAll("[fill='var(--surface)']").forEach(g => g.setAttribute("fill", "#ffffff"));
  clone.querySelectorAll("[stroke='var(--surface)']").forEach(g => g.setAttribute("stroke", "#ffffff"));
  const bitmap = await loadImage(`data:image/svg+xml;charset=utf-8,${encodeURIComponent(new XMLSerializer().serializeToString(clone))}`);
  const canvas = document.createElement("canvas");
  let size = Math.min(1, 1600 / Math.max(picture.width, picture.height));
  for (let attempt = 0; attempt < 8; attempt++, size *= 0.75) {
    canvas.width = Math.max(1, Math.round(picture.width * size)); canvas.height = Math.max(1, Math.round(picture.height * size));
    const ctx = canvas.getContext("2d"); if (!ctx) throw new Error("This phone couldn’t save the marks.");
    ctx.fillStyle = "#ffffff"; ctx.fillRect(0, 0, canvas.width, canvas.height); ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    const data = canvas.toDataURL("image/jpeg", 0.82).split(",")[1]!;
    if (data && atob(data).length <= MAX_SCREENSHOT_BYTES) return { media_type: "image/jpeg", data, width: canvas.width, height: canvas.height, marked: true };
  }
  throw new Error("The marked picture is too large. Try fewer marks.");
}
export function Markup({ picture, onSave, onClose }: { picture: ReportScreenshot; onSave(p: ReportScreenshot): void; onClose(): void }) {
  const [tool, setTool] = useState<Tool>("stroke"), [marks, setMarks] = useState<Mark[]>([]), [selected, setSelected] = useState<string[]>([]);
  const [viewport, setViewport] = useState({ zoom: 1, panX: 0, panY: 0 }), [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const source: SourceDraft = useMemo(() => ({ key: "S1", blobKey: null, artifactId: null, sha256: null, mediaType: picture.media_type, width: picture.width, height: picture.height, durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 0, derivedFrom: null, derivation: null }), [picture]);
  return <Sheet open title="Mark this screen" size="full" onClose={onClose} footer={<Button variant="primary" onClick={() => {
    const svg = root.current?.querySelector("svg"); if (!svg) return;
    void flattenPicture(picture, svg).then(onSave).catch((e: Error) => setError(e.message));
  }}>Keep marks</Button>}>
    <div className="report-markup" ref={root}>
      <div className="report-tools" role="group" aria-label="Drawing tools">{(Object.keys(SHAPE_WORD) as Tool[]).map(t => <Button key={t} size="sm" aria-pressed={tool === t} onClick={() => setTool(t)}>{SHAPE_WORD[t]}</Button>)}</div>
      <Canvas source={source} imageUrl={pictureUrl(picture)} imageLabel={`Ophio’s screen with ${marks.length} marks. Use the drawing tools to add marks.`} marks={marks} tool={tool} selected={selected} viewport={viewport} onViewport={setViewport} clock={0}
        onAdd={shape => setMarks(m => [...m, { mark_id: uuidv7(), label: String.fromCharCode(65 + m.length), source_key: "S1", shape, color: 0, created_at: 0, deleted: false }])}
        onMove={(id, shape) => setMarks(m => m.map(mark => mark.mark_id === id ? { ...mark, shape } : mark))} onSelect={id => setSelected(id ? [id] : [])} onDelete={id => setMarks(m => m.filter(mark => mark.mark_id !== id))} />
      {marks.length > 0 && <ul aria-label="Marks">{marks.map(m => <li key={m.mark_id}><Button size="sm" aria-pressed={selected.includes(m.mark_id)} onClick={() => setSelected([m.mark_id])}>Select mark {m.label}</Button><Button size="sm" variant="quiet" aria-label={`Remove mark ${m.label}`} onClick={() => setMarks(all => all.filter(mark => mark !== m))}>Remove</Button></li>)}</ul>}
    </div>
    {error && <p role="status" className="error-text">{error}</p>}
  </Sheet>;
}
