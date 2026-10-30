// SPDX-License-Identifier: Apache-2.0

import type { LucideIcon } from "lucide-react";
import { useLayoutEffect, useRef, type ButtonHTMLAttributes, type ReactNode, type RefObject } from "react";

export type Tone = "neutral" | "accent" | "ok" | "warn" | "stop";

/** A selectable chip. Selection is announced (aria-pressed) and shown by its fill and heavier ring, which leave its width alone. */
export function Chip({
  selected,
  icon: Icon,
  children,
  className,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { selected?: boolean; icon?: LucideIcon; children: ReactNode }) {
  return (
    <button type="button" className={["chip", selected && "is-selected", className].filter(Boolean).join(" ")} aria-pressed={selected} {...rest}>
      {Icon && <Icon aria-hidden className="chip__icon" strokeWidth={2.1} />}
      <span className="chip__label">{children}</span>
    </button>
  );
}

/** A static label with a semantic tone. The text always carries the meaning. */
export function Pill({ tone = "neutral", icon: Icon, children, className }: { tone?: Tone; icon?: LucideIcon; children: ReactNode; className?: string }) {
  return (
    <span className={["pill", `pill--${tone}`, className].filter(Boolean).join(" ")}>
      {Icon && <Icon aria-hidden className="pill__icon" strokeWidth={2.2} />}
      {children}
    </span>
  );
}

export interface SegmentOption<T extends string> {
  value: T;
  label: string;
  icon?: LucideIcon;
  disabled?: boolean;
}

/**
 * Keeps a row of labels on one line as the text size grows. Where they don't
 * fit, the options take widths by their labels and the text shrinks just
 * enough, as a segmented control does on iOS, but never below its size at the
 * default text size; past that they wrap.
 */
function useFitRow(ref: RefObject<HTMLElement | null>, labels: string) {
  useLayoutEffect(() => {
    const row = ref.current;
    if (!row || typeof ResizeObserver === "undefined") return;
    let width = -1;
    const fit = () => {
      // Its height follows the fit; only a new width asks for another.
      if (row.clientWidth === width) return;
      width = row.clientWidth;
      const spans = [...row.querySelectorAll<HTMLElement>(".segmented__label")];
      row.dataset.fit = "fit";
      row.style.removeProperty("--fit");
      // Labels stay free to wrap, so they never widen what holds the row; one
      // that wraps, or has a word too long to, doesn't fit.
      const oneLine = (s: HTMLElement) => s.scrollWidth <= s.clientWidth && s.offsetHeight < parseFloat(getComputedStyle(s).fontSize) * 1.8;
      if (spans.every(oneLine)) return;
      const style = getComputedStyle(row);
      let room = width - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight) - (spans.length - 1) * (parseFloat(style.columnGap) || 0);
      // Each option as wide as its label on one line, to see how much of the
      // row is text. That can widen the row for this moment; the room is
      // taken from before.
      row.dataset.fit = "measure";
      let text = 0;
      for (const s of spans) {
        text += s.offsetWidth;
        room -= s.parentElement!.offsetWidth - s.offsetWidth;
      }
      const floor = Math.min(1, 16 / (parseFloat(getComputedStyle(document.documentElement).fontSize) || 16));
      const scale = text > 0 ? (room / text) * 0.98 : 1;
      row.dataset.fit = scale < floor ? "wrap" : "shrink";
      row.style.setProperty("--fit", String(Math.min(1, Math.max(scale, floor))));
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(row);
    return () => observer.disconnect();
  }, [ref, labels]);
}

/** Single choice among a few options (radiogroup), kept to one row; see useFitRow. */
export function Segmented<T extends string>({
  label,
  value,
  options,
  onChange,
  className,
}: {
  label: string;
  value: T;
  options: SegmentOption<T>[];
  onChange: (v: T) => void;
  className?: string;
}) {
  const row = useRef<HTMLDivElement>(null);
  useFitRow(row, options.map((o) => o.label).join("\n"));
  const move = (dir: 1 | -1) => {
    const enabled = options.filter((o) => !o.disabled);
    const i = enabled.findIndex((o) => o.value === value);
    const next = enabled[(i + dir + enabled.length) % enabled.length];
    if (next) onChange(next.value);
  };
  return (
    <div
      ref={row}
      role="radiogroup"
      aria-label={label}
      className={["segmented", className].filter(Boolean).join(" ")}
      onKeyDown={(e) => {
        if (e.key === "ArrowRight" || e.key === "ArrowDown") (e.preventDefault(), move(1));
        if (e.key === "ArrowLeft" || e.key === "ArrowUp") (e.preventDefault(), move(-1));
      }}
    >
      {options.map((o) => {
        const Icon = o.icon;
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on ? 0 : -1}
            disabled={o.disabled}
            className={["segmented__opt", on && "is-on"].filter(Boolean).join(" ")}
            onClick={() => onChange(o.value)}
          >
            {Icon && <Icon aria-hidden strokeWidth={2.1} />}
            <span className="segmented__label">{o.label}</span>
          </button>
        );
      })}
    </div>
  );
}
