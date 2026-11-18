// SPDX-License-Identifier: Apache-2.0
//
// Sheets use the native modal layer in single view. Split view contains each
// sheet in its own pane so the other side and its safety controls stay usable.
// Closing a sheet never approves, resumes or sends anything.

import { X } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useId, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { usePane } from "../shell/pane";
import { useBackLayer } from "./back";
import { IconButton } from "./Button";
import { UrgentSlot, type UrgentExcept } from "./urgent";
import { SurfaceActive } from "./surface";
import "./paneLayers.css";

interface OpenSheet {
  id: string;
  owner: string | null;
  nativeModal: boolean;
  dialog: HTMLDialogElement;
  root: HTMLElement | null;
  /** When the sheet was asked to open: later sheets, and sheets opened from
   *  inside a sheet, stay above earlier ones however often they move. */
  order: number;
}

// Keep lower slots mounted: Force stop can itself live inside a lower sheet.
const openSheets: OpenSheet[] = [];
const listeners = new Set<() => void>();
const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
// Native modals cover all panes in opening order. Contained split sheets can
// own separate Stop slots only when no native modal covers the document.
const topSheet = (owner: string | null) =>
  openSheets.findLast((sheet) => sheet.nativeModal)?.id ??
  openSheets.findLast((sheet) => sheet.owner === owner)?.id;
const inerted = new Map<Element, boolean>();
/** A boxless element that holds one sheet's dialog for the sheet's whole life. */
const HOST = "sheet-host";
const FIELD = "input:not([type=checkbox],[type=radio],[type=button]), textarea, select, [contenteditable]:not([contenteditable=false])";

// Opening order. React re-attaches and re-shows sheets innermost first when
// the window folds or widens, so the order things happen in then is not the
// order the sheets are in: a confirmation opened from inside another sheet
// must stay above it. Each sheet takes a number when it is asked to open, and
// takes it after the sheet it was opened from, even in the same render.
let opened = 0;
const SheetOrder = createContext<(() => number) | null>(null);
const hostOrder = new WeakMap<Element, () => number | null>();

/** Whether `host` sits out of opening order among the open sheets in `place`. */
function misplaced(place: Element, host: Element, at: number): boolean {
  let seen = false;
  for (const child of place.children) {
    if (child === host) { seen = true; continue; }
    const other = hostOrder.get(child)?.();
    if (other == null) continue;
    if (seen ? other < at : other > at) return true;
  }
  return false;
}

/** The first open sheet in `place` that was opened after `at`. */
function laterHost(place: Element, host: Element, at: number): Element | null {
  for (const child of place.children) {
    const other = child === host ? null : hostOrder.get(child)?.();
    if (other != null && other > at) return child;
  }
  return null;
}

type Scroll = readonly [Element, number, number];

// A nested sheet moves first, sometimes into its parent while the parent is
// still inert below it, where its focused field cannot keep focus. The
// parent's own move, later in the same update, then keeps that field.
let lostFocus: HTMLElement | null = null;

/** Move or re-show what is inside `within` without losing its place: moving
 *  an element out of the document drops its focus and resets its scrolling. */
function keepPlace(within: readonly Element[], change: () => void): { focused: HTMLElement | null; scrolled: Scroll[] } {
  const active = document.activeElement;
  const focus = active instanceof HTMLElement && active !== document.body ? active : lostFocus;
  const focused = focus && within.some((el) => el.contains(focus)) ? focus : null;
  const scrolled = within.flatMap((el) => [...el.querySelectorAll("*")])
    .filter((el) => el.scrollTop || el.scrollLeft)
    .map((el) => [el, el.scrollTop, el.scrollLeft] as const);
  change();
  rescroll(scrolled);
  if (focused) {
    focused.focus({ preventScroll: true });
    lostFocus = document.activeElement === focused ? null : focused;
  }
  return { focused, scrolled };
}

function rescroll(scrolled: readonly Scroll[]): void {
  for (const [el, top, left] of scrolled) { el.scrollTop = top; el.scrollLeft = left; }
}

/** Show a native modal dialog under any open sheet opened after it, then
 *  focus `focus` if it is given. The top layer stacks in showModal order, so
 *  those sheets are taken out and shown again; nothing in a closed dialog can
 *  take focus, so focus waits until they are all shown. */
function showModalAt(dialog: HTMLDialogElement, at: number, focus: HTMLElement | null): void {
  const above = openSheets.filter((sheet) => sheet.nativeModal && sheet.order > at && sheet.dialog.open);
  keepPlace(above.map((sheet) => sheet.dialog), () => {
    for (const sheet of [...above].reverse()) sheet.dialog.close();
    dialog.showModal();
    for (const sheet of above) sheet.dialog.showModal();
  });
  // A sheet under a later one leaves focus in the later one.
  if (!above.length || above.some((sheet) => sheet.dialog.contains(focus))) focus?.focus({ preventScroll: true });
}

/** Whether `el` is outside what can be seen of its scrolling containers. */
function outOfSight(el: HTMLElement): boolean {
  const r = el.getBoundingClientRect();
  if (r.top < 0 || r.left < 0 || r.bottom > window.innerHeight || r.right > window.innerWidth) return true;
  for (let box = el.parentElement; box; box = box.parentElement) {
    if (box.scrollHeight <= box.clientHeight && box.scrollWidth <= box.clientWidth) continue;
    const b = box.getBoundingClientRect();
    // A pinned footer covers the bottom of what scrolls (see components.css).
    const style = getComputedStyle(box);
    const top = b.top + (parseFloat(style.scrollPaddingTop) || 0);
    const bottom = b.bottom - (parseFloat(style.scrollPaddingBottom) || 0);
    if (r.top < top || r.bottom > bottom || r.left < b.left || r.right > b.right) return true;
  }
  return false;
}

function changed(): void {
  // Inert only the covered pane's background and lower sheets, never the
  // document or the other pane. Preserve any pre-existing inert attributes.
  const roots = new Map<HTMLElement, HTMLDialogElement>();
  for (const sheet of openSheets) if (sheet.root) roots.set(sheet.root, sheet.dialog);
  const covered = new Set<Element>();
  for (const [root, dialog] of roots) {
    for (const child of root.children) {
      // A sheet's host has no box of its own: cover the dialog it holds.
      const element = child.classList.contains(HOST) ? child.firstElementChild : child;
      if (element && element !== dialog && !element.contains(dialog)) covered.add(element);
    }
  }
  for (const [element, wasInert] of inerted) {
    if (covered.has(element)) continue;
    if (!wasInert) element.removeAttribute("inert");
    inerted.delete(element);
  }
  for (const element of covered) {
    if (!inerted.has(element)) inerted.set(element, element.hasAttribute("inert"));
    element.setAttribute("inert", "");
  }
  for (const listener of listeners) listener();
}

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
  /**
   * Holds something typed or chosen and not yet sent. When another screen
   * covers this one (an agent sheet's "Defaults in Settings", a tapped
   * notification), the sheet steps aside without `onClose` and is there
   * again, as it was left, on Back. Without it the sheet is dismissed.
   */
  keepWhenHidden?: boolean;
}

export function Sheet({ open: requestedOpen, onClose, title, subtitle, children, footer, size = "auto", leading, head, className, closeLabel = "Close", urgentExcept, keepWhenHidden = false }: SheetProps) {
  const active = useContext(SurfaceActive);
  const pane = usePane();
  const paneRef = useRef(pane);
  paneRef.current = pane;
  const contained = pane?.split ?? false;
  const root = contained ? pane?.root ?? null : null;
  const owner = pane?.id ?? null;
  // A hidden pane retains its sheet and draft. Unlike navigation away from
  // a screen, hiding that pane is not a request to dismiss its dialogs.
  const open = requestedOpen && active && (pane?.visible ?? true);
  // Navigation hides kept screens without unmounting them. Dismiss their
  // modal state too, so Back never resurrects an old confirmation or viewer;
  // a sheet holding unsent work only leaves the top layer until it is back.
  useEffect(() => {
    if (requestedOpen && !active && !keepWhenHidden) onClose();
  }, [requestedOpen, active, onClose, keepWhenHidden]);
  const parentOrder = useContext(SheetOrder);
  const order = useRef<number | null>(null);
  const ensureOrder = useCallback(() => {
    if (order.current == null) {
      parentOrder?.();
      order.current = ++opened;
    }
    return order.current;
  }, [parentOrder]);
  const ref = useRef<HTMLDialogElement>(null);
  const stillOpen = useRef(open);
  stillOpen.current = open;
  const titleRef = useRef<HTMLHeadingElement>(null);
  const backdropField = useRef<HTMLElement | null>(null);
  const footerRef = useRef<HTMLElement>(null);
  // The dialog always renders into this one element, which moves between the
  // pane (split view) and the sheet's own place in the screen (single view).
  // Folding, rotating or narrowing the window then moves the sheet rather than
  // remounting it, so what is typed in it and not yet saved survives.
  const [host] = useState(() => {
    const element = Object.assign(document.createElement("div"), { className: HOST });
    hostOrder.set(element, () => order.current);
    return element;
  });
  const inlineRef = useRef<HTMLDivElement>(null);
  const movedFocus = useRef<HTMLElement | null>(null);
  const movedScroll = useRef<Scroll[]>([]);
  const opener = useRef<HTMLElement | null>(null);
  const id = useId();
  const top = useSyncExternalStore(subscribe, () => topSheet(owner));
  const Urgent = useContext(UrgentSlot);
  useBackLayer(open, onClose, { nativeModal: !contained, order: () => order.current ?? Infinity });

  useLayoutEffect(() => {
    if (requestedOpen) ensureOrder();
    else order.current = null;
    const place = contained ? root : inlineRef.current;
    if (!place) { host.remove(); return; }
    // Sheets in a pane all sit on one level, so the later one in the pane is
    // drawn on top: keep them in opening order.
    const at = order.current;
    if (host.parentNode === place && !(contained && at != null && misplaced(place, host, at))) return;
    // Keep the field being typed in focused, and the sheet scrolled where it
    // was being read, across the move and through reopening below.
    const kept = keepPlace([host], () => place.insertBefore(host, contained && at != null ? laterHost(place, host, at) : null));
    if (kept.focused) movedFocus.current = kept.focused;
    movedScroll.current = kept.scrolled;
  }, [contained, root, host, requestedOpen, ensureOrder]);
  useLayoutEffect(() => () => host.remove(), [host]);

  useEffect(() => {
    backdropField.current = null;
    // Every move in this update is done; whatever focus was lost is decided below.
    lostFocus = null;
    const kept = movedFocus.current;
    const scrolled = movedScroll.current;
    movedFocus.current = null;
    movedScroll.current = [];
    const d = ref.current;
    if (!d || !open) return;
    const at = order.current ?? Infinity;
    // After a move between views, focus is already inside the sheet: the
    // opener to return to on closing is still the one it was opened from.
    const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const moved = !!kept && d.contains(kept);
    const previousFocus = moved ? opener.current : focus;
    opener.current = previousFocus;
    if (contained) {
      // show() also performs browser focus steps. Set open directly instead,
      // so an asynchronous sheet in the other pane cannot steal focus.
      d.open = true;
    } else if (!d.open) {
      showModalAt(d, at, moved ? kept : titleRef.current);
    }
    const entry: OpenSheet = { id, owner, nativeModal: !contained, dialog: d, root, order: at };
    const later = openSheets.findIndex((sheet) => sheet.order > at);
    openSheets.splice(later === -1 ? openSheets.length : later, 0, entry);
    changed();
    // A lower sheet may have made this previously closed sibling inert.
    // Release that state before attempting to announce the new sheet.
    const scope = paneRef.current;
    if (moved) {
      kept.focus({ preventScroll: true });
      // Where the sheet was scrolled to wins. Only a field being typed in that
      // the new, narrower or shorter view has put out of sight is scrolled to.
      rescroll(scrolled);
      if (kept.matches(FIELD) && outOfSight(kept)) kept.scrollIntoView?.({ block: "nearest" });
    }
    else if (contained && scope?.active && (previousFocus === document.body || (previousFocus && root?.contains(previousFocus)))) {
      titleRef.current?.focus({ preventScroll: true });
    }
    let focusWithinPane = !!root?.contains(document.activeElement);
    const trackFocus = (event: FocusEvent) => { focusWithinPane = event.target instanceof Node && !!root?.contains(event.target); };
    if (contained) document.addEventListener("focusin", trackFocus);
    return () => {
      document.removeEventListener("focusin", trackFocus);
      const index = openSheets.indexOf(entry);
      if (index !== -1) openSheets.splice(index, 1);
      // Removing the open attribute does not invoke native focus restoration.
      // Only restore a pane opener if no other pane has since received focus,
      // and only on closing: a sheet that is moving keeps its own focus. (An
      // unmounted sheet's dialog ref is already empty here.)
      const focus = document.activeElement;
      const moving = stillOpen.current && ref.current === d;
      const restore = !moving && contained && paneRef.current?.active && paneRef.current.visible &&
        ((focus instanceof Node && root?.contains(focus)) || (focus === document.body && focusWithinPane)) &&
        previousFocus && root?.contains(previousFocus);
      if (contained) d.open = false;
      else if (d.open) d.close();
      changed();
      if (restore && previousFocus.isConnected && !previousFocus.closest("[inert]")) previousFocus.focus({ preventScroll: true });
    };
  }, [open, id, owner, contained, root, host]);

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

  const dialog = (
    <dialog
      ref={ref}
      className={["sheet", `sheet--${size}`, contained && "pane-sheet", className].filter(Boolean).join(" ")}
      aria-labelledby={`${id}-t`}
      data-urgent-host={top === id}
      onKeyDown={(e) => {
        if (contained && e.key === "Escape" && pane?.active && pane.visible && top === id) {
          e.preventDefault();
          e.stopPropagation();
          onClose();
        }
      }}
      onCancel={(e) => {
        // React passes a sheet's cancel on to the sheet it was opened from;
        // the browser cancels only the topmost.
        if (e.target !== e.currentTarget) return;
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
      {(open || (pane && requestedOpen && active)) && (
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
  // The sheet's place in the screen stays rendered in split view too: removing
  // it while it holds the dialog would drop focus before the move.
  return <>
    <div ref={inlineRef} className={HOST} />
    <SheetOrder.Provider value={ensureOrder}>{createPortal(dialog, host)}</SheetOrder.Provider>
  </>;
}

function focusedField(dialog: HTMLDialogElement | null): HTMLElement | null {
  const active = document.activeElement;
  return active instanceof HTMLElement && dialog?.contains(active) &&
    active.matches(FIELD)
    ? active : null;
}
