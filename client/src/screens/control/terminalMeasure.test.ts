// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { openMeasuringText, remeasureOnTextZoom } from "./terminalMeasure";

/** Android's text size: it zooms page text, and a canvas not at all. */
let zoom = 1;
const pageWidth = (el: HTMLElement) => (parseFloat(el.style.fontSize) || 0) * 0.6 * (el.textContent?.length ?? 0) * zoom;
let canvases = 0;
class Canvas {
  constructor() { canvases++; }
  getContext() {
    let size = 0;
    return {
      set font(font: string) { size = parseFloat(font); },
      measureText: () => ({ width: size * 0.6, fontBoundingBoxAscent: size, fontBoundingBoxDescent: size * 0.2 }),
    };
  }
}
let observed: (() => void)[];
let parent: HTMLElement;
let term: Terminal;

beforeEach(() => {
  zoom = 1; canvases = 0; observed = [];
  document.documentElement.style.fontSize = "";
  vi.stubGlobal("OffscreenCanvas", Canvas);
  vi.stubGlobal("ResizeObserver", class { constructor(private readonly changed: () => void) {} observe() { observed.push(this.changed); } disconnect() { observed = observed.filter(c => c !== this.changed); } });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }));
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) { return pageWidth(this); });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) { return (parseFloat(this.style.fontSize) || 0) * 1.2 * zoom; });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  parent = document.body.appendChild(document.createElement("div"));
  parent.style.width = "600px"; parent.style.height = "400px";
  term = new Terminal({ fontSize: 14, fontFamily: "monospace", scrollback: 0 });
});
afterEach(() => { term.dispose(); parent.remove(); document.documentElement.style.fontSize = ""; vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const columns = (fit: FitAddon) => fit.proposeDimensions()?.cols ?? 0;
const setZoom = (next: number) => { zoom = next; document.documentElement.style.fontSize = `${16 * next}px`; };
/** The columns a terminal opened at text size `z` fits in the same space. */
function fitsAt(z: number, open: (term: Terminal) => void): number {
  const other = new Terminal({ fontSize: 14, fontFamily: "monospace", scrollback: 0 });
  const fit = new FitAddon(); other.loadAddon(fit);
  setZoom(z); open(other);
  const cols = columns(fit);
  other.dispose(); setZoom(1);
  return cols;
}

it("sizes cells from page text, which the phone's text size zooms, rather than a canvas, which it doesn't", () => {
  const normal = fitsAt(1, t => openMeasuringText(t, parent));
  canvases = 0;
  const large = fitsAt(2, t => openMeasuringText(t, parent));
  expect(canvases).toBe(0);
  expect(globalThis.OffscreenCanvas).toBe(Canvas);
  expect(normal).toBeGreaterThan(40);
  expect(Math.abs(large * 2 - normal)).toBeLessThanOrEqual(2);
});

it("unaided, xterm measures on the canvas, so double text would fit as many columns as normal", () => {
  const normal = fitsAt(1, t => openMeasuringText(t, parent));
  const large = fitsAt(2, t => t.open(parent));
  expect(canvases).toBeGreaterThan(0);
  expect(large).toBe(normal);
});

it("measures again when the text size changes while the terminal is open, and only then", () => {
  const fit = new FitAddon(); term.loadAddon(fit);
  openMeasuringText(term, parent);
  const resized = vi.fn();
  const stop = remeasureOnTextZoom(term, resized);
  expect(observed).toHaveLength(1);
  const before = columns(fit);
  observed[0]!();
  expect(resized).not.toHaveBeenCalled();
  expect(columns(fit)).toBe(before);
  setZoom(2);
  observed[0]!();
  expect(resized).toHaveBeenCalledOnce();
  expect(term.options.fontSize).toBe(14);
  expect(Math.abs(columns(fit) * 2 - before)).toBeLessThanOrEqual(2);
  stop();
  expect(observed).toHaveLength(0);
});
