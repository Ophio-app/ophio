// SPDX-License-Identifier: Apache-2.0
//
// Modal sheet on a native <dialog>: the platform supplies the focus trap,
// inert background and Escape. System back closes it through the back stack.
// Closing a sheet never approves, resumes or sends anything.

import { X } from "lucide-react";
import { useContext, useEffect, useId, useRef, useSyncExternalStore, type ReactNode } from "react";
import { useBackLayer } from "./back";
import { IconButton } from "./Button";
import { UrgentSlot, type UrgentExcept } from "./urgent";

// Registration follows showModal order, including sibling sheets. Keep lower
// slots mounted: an open Force stop dialog can itself live inside one of them.
const openSheets: string[] = [];
const listeners = new Set<() => void>();
const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
const topSheet = () => openSheets.at(-1);
const changed = () => { for (const listener of listeners) listener(); };

export interface SheetProps {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  /** Short line under the title, e.g. the object the sheet acts on. */
  subtitle?: ReactNode;
  children: ReactNode;
  /** Sticky action area. */
  footer?: ReactNode;
  /** Full height for editors and long lists; `drop` hangs from the top of
   *  the screen, as a list opened from the header. */
  size?: "auto" | "tall" | "full" | "drop";
  /** Leading control in the header, e.g. Back inside a multi-step sheet. */
  leading?: ReactNode;
  /** Shown in place of the title, as a search field. The title is still
   *  what is announced and focused on opening, so no keyboard comes up. */
  head?: ReactNode;
  className?: string;
  closeLabel?: string;
  /** The target or task this sheet already has safety controls for. With a
   *  task, the footer is that task's Stop and stays in view. */
  urgentExcept?: UrgentExcept;
}

export function Sheet({ open, onClose, title, subtitle, children, footer, size = "auto", leading, head, className, closeLabel = "Close", urgentExcept }: SheetProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const backdropField = useRef<HTMLElement | null>(null);
  const footerRef = useRef<HTMLElement>(null);
  const id = useId();
  const top = useSyncExternalStore(subscribe, topSheet);
  const Urgent = useContext(UrgentSlot);
  useBackLayer(open, onClose);

  useEffect(() => {
    backdropField.current = null;
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      d.showModal();
      titleRef.current?.focus({ preventScroll: true });
    }
    if (!open) return;
    openSheets.push(id);
    changed();
    return () => {
      const index = openSheets.indexOf(id);
      if (index !== -1) openSheets.splice(index, 1);
      if (d.open) d.close();
      changed();
    };
  }, [open, id]);

  // A short screen pins the footer; what is scrolled to or pinned above it
  // offsets by its height (see components.css).
  const hasFooter = !!footer;
  useEffect(() => {
    const f = footerRef.current;
    if (!f || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => f.parentElement?.style.setProperty("--sheet-footer", `${f.offsetHeight}px`));
    observer.observe(f, { box: "border-box" });
    return () => observer.disconnect();
  }, [open, hasFooter]);

  return (
    <dialog
      ref={ref}
      className={["sheet", `sheet--${size}`, className].filter(Boolean).join(" ")}
      aria-labelledby={`${id}-t`}
      data-urgent-host={top === id}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onPointerDown={(e) => {
        backdropField.current = e.target === ref.current ? focusedField(ref.current) : null;
      }}
      onPointerCancel={() => { backdropField.current = null; }}
      onClick={(e) => {
        const field = backdropField.current ?? focusedField(ref.current);
        backdropField.current = null;
        if (e.target !== ref.current) return;
        // The browser may focus the dialog between pointerdown and click.
        // Preserve the intent of the tap from before that default action.
        if (field) {
          field.blur();
          return;
        }
        onClose();
      }}
    >
      {open && (
        <div className="sheet__panel">
          <div className="sheet__main">
            <div className="sheet__grabber" aria-hidden />
            <header className="sheet__header">
              {leading}
              <div className={head ? "visually-hidden" : "sheet__titles"}>
                <h2 id={`${id}-t`} ref={titleRef} tabIndex={-1} className="sheet__title">
                  {title}
                </h2>
                {subtitle && <p className="sheet__subtitle">{subtitle}</p>}
              </div>
              {head}
              <IconButton icon={X} label={closeLabel} onClick={onClose} />
            </header>
            <div className="sheet__body">{children}</div>
            {footer && <footer ref={footerRef} className={urgentExcept?.taskId ? "sheet__footer sheet__footer--stop" : "sheet__footer"}>{footer}</footer>}
          </div>
          {Urgent && (
            <div className="sheet__urgent">
              <Urgent dismiss={onClose} except={urgentExcept} />
            </div>
          )}
        </div>
      )}
    </dialog>
  );
}

function focusedField(dialog: HTMLDialogElement | null): HTMLElement | null {
  const active = document.activeElement;
  return active instanceof HTMLElement && dialog?.contains(active) &&
    active.matches("input:not([type=checkbox],[type=radio],[type=button]), textarea, select, [contenteditable]:not([contenteditable=false])")
    ? active : null;
}
