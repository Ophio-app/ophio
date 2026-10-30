// SPDX-License-Identifier: Apache-2.0
import type { Terminal } from "@xterm/xterm";

/**
 * Open `term` so it sizes its cells the way it draws its letters.
 *
 * xterm measures a letter on a canvas when it can, but draws letters as page
 * text. Android's text size zooms page text and not a canvas, so at any size
 * but the default the letters outgrow their cells and xterm squeezes them on
 * top of one another. With no canvas to measure on, it measures a letter in
 * the page, zoomed like the letters it draws.
 */
export function openMeasuringText(term: Terminal, parent: HTMLElement): void {
  const canvas = Object.getOwnPropertyDescriptor(globalThis, "OffscreenCanvas");
  if (canvas) Object.defineProperty(globalThis, "OffscreenCanvas", { value: undefined, configurable: true, writable: true });
  try {
    term.open(parent);
  } finally {
    if (canvas) Object.defineProperty(globalThis, "OffscreenCanvas", canvas);
  }
}

/** The phone's text size as a multiple of the default. */
export function textZoom(): number {
  return (parseFloat(getComputedStyle(document.documentElement).fontSize) || 16) / 16;
}

/**
 * Re-measure `term`'s cells whenever the phone's text size changes while it
 * is open, then call `resized`. xterm measures again only when its own font
 * changes, so a new text size would leave letters in cells sized for the old.
 */
export function remeasureOnTextZoom(term: Terminal, resized: () => void): () => void {
  // xterm's own measuring letters: page text, so they resize with the zoom.
  const letters = term.element?.querySelector(".xterm-helpers > .xterm-char-measure-element");
  if (!letters) return () => {};
  let zoom = textZoom();
  const observer = new ResizeObserver(() => {
    const next = textZoom();
    if (next === zoom) return;
    zoom = next;
    const size = term.options.fontSize ?? 14;
    term.options.fontSize = size + 1;
    term.options.fontSize = size;
    resized();
  });
  observer.observe(letters);
  return () => observer.disconnect();
}
