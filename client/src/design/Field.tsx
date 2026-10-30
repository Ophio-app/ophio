// SPDX-License-Identifier: Apache-2.0

import { forwardRef, useEffect, useId, useLayoutEffect, useRef, type InputHTMLAttributes, type TextareaHTMLAttributes } from "react";

interface TextAreaProps extends Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, "onChange" | "value"> {
  label: string;
  hideLabel?: boolean;
  value: string;
  onChange: (v: string) => void;
  hint?: string;
  /** Grow with content up to this many rows. */
  maxRows?: number;
}

/** Sizes a field to its text, up to `maxRows` lines. */
function fitRows(el: HTMLTextAreaElement, maxRows: number) {
  el.style.height = "auto";
  const cs = getComputedStyle(el);
  const lh = parseFloat(cs.lineHeight) || 22;
  const border = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);
  const pad = parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom);
  // border-box: scrollHeight covers content and padding, not borders.
  el.style.height = `${Math.min(el.scrollHeight + border, lh * maxRows + pad + border)}px`;
}

/**
 * Auto-growing multi-line field with a visible label, or with `hideLabel` a name
 * only a screen reader hears. A hidden name is carried by the field itself: a
 * phone screen reader would stop on a hidden label as a place of its own.
 */
export const TextArea = forwardRef<HTMLTextAreaElement, TextAreaProps>(function TextArea(
  { label, hideLabel, value, onChange, hint, maxRows = 8, rows = 1, className, ...rest },
  outer,
) {
  const id = useId();
  const inner = useRef<HTMLTextAreaElement | null>(null);
  const rowsLimit = useRef(maxRows);
  useLayoutEffect(() => {
    rowsLimit.current = maxRows;
    if (inner.current) fitRows(inner.current, maxRows);
  }, [value, maxRows]);
  // The same text takes fewer lines in a wider field and more in a narrower
  // one, so the height follows the width too (the next frame, after layout).
  useEffect(() => {
    const el = inner.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let width = el.clientWidth;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => fitRows(el, rowsLimit.current));
    });
    observer.observe(el);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, []);
  return (
    <div className={["field", className].filter(Boolean).join(" ")}>
      {!hideLabel && (
        <label htmlFor={id} className="field__label">
          {label}
        </label>
      )}
      <textarea
        id={id}
        aria-label={hideLabel ? label : undefined}
        ref={(el) => {
          inner.current = el;
          if (typeof outer === "function") outer(el);
          else if (outer) outer.current = el;
        }}
        className="field__input field__input--area"
        value={value}
        rows={rows}
        onChange={(e) => onChange(e.target.value)}
        aria-describedby={hint ? `${id}-h` : undefined}
        {...rest}
      />
      {hint && (
        <p className="field__hint" id={`${id}-h`}>
          {hint}
        </p>
      )}
    </div>
  );
});

interface TextFieldProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "onChange" | "value"> {
  label: string;
  hideLabel?: boolean;
  value: string;
  onChange: (v: string) => void;
  hint?: string;
  error?: string | null;
}

export const TextField = forwardRef<HTMLInputElement, TextFieldProps>(function TextField(
  { label, hideLabel, value, onChange, hint, error, className, ...rest },
  ref,
) {
  const id = useId();
  const described = [hint && `${id}-h`, error && `${id}-e`].filter(Boolean).join(" ") || undefined;
  return (
    <div className={["field", error && "has-error", className].filter(Boolean).join(" ")}>
      {!hideLabel && (
        <label htmlFor={id} className="field__label">
          {label}
        </label>
      )}
      <input
        id={id}
        aria-label={hideLabel ? label : undefined}
        ref={ref}
        className="field__input"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        aria-invalid={error ? true : undefined}
        aria-describedby={described}
        {...rest}
      />
      {hint && (
        <p className="field__hint" id={`${id}-h`}>
          {hint}
        </p>
      )}
      {error && (
        <p className="field__error" id={`${id}-e`} role="alert">
          {error}
        </p>
      )}
    </div>
  );
});
