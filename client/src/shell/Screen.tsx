// SPDX-License-Identifier: Apache-2.0
//
// The frame every destination renders into. A tab's root shows the context
// header; a pushed destination shows a bar with Back, or Close for a focused
// one (see focus.ts). The body is the one scrolling region, so each
// destination keeps its own scroll position.

import { createContext, useContext, useLayoutEffect, useRef, type ReactNode, type Ref } from "react";
import { useApp, useUi } from "../app/context";
import type { Route, Tab } from "../app/ui";
import { ContextHeader, ScreenBar } from "./ContextHeader";
import { useTightLayout } from "./keyboard";
import { isFocused } from "./focus";

export interface RouteInfo {
  tab: Tab;
  route: Route;
  /** Position in the tab's stack; 0 is the tab root. */
  depth: number;
  /** On screen now: the current tab's top destination. */
  active: boolean;
}

const RouteCtx = createContext<RouteInfo | null>(null);
export const RouteProvider = RouteCtx.Provider;

export function useRoute(): RouteInfo {
  const r = useContext(RouteCtx);
  if (!r) throw new Error("useRoute outside a route");
  return r;
}

/** Whether what calls it is on screen now; outside any route, it is. */
export function useOnScreen(): boolean {
  return useContext(RouteCtx)?.active ?? true;
}

export interface ScreenProps {
  /** Title for pushed destinations. Tab roots show the context header instead. */
  title?: ReactNode;
  /** The body leads with the screen's own h1, as a receipt's task title;
   * the bar's title is then a label, so a screen reader hears one title. */
  bodyHeading?: boolean;
  /** Specific accessible label for the single back control. */
  backLabel?: string;
  /** A step back within the screen, such as folding an open editor, taken
   * by the bar's Back before leaving, as system Back does. Close leaves. */
  stepBack?: (() => void) | null;
  subtitle?: ReactNode;
  /** Trailing actions in the bar of pushed destinations. */
  actions?: ReactNode;
  children: ReactNode;
  /** Pinned below the scrolling body, e.g. a composer or a safety strip. */
  footer?: ReactNode;
  /**
   * The footer is a confirm-style action, so on a screen that is short for
   * its text size it scrolls at the end of the content instead of staying
   * pinned. Never for composers or safety controls.
   */
  footerFlows?: boolean;
  /** Pinned above the scrolling body, below the header. */
  top?: ReactNode;
  /** A tab root's own header in place of the context header, as Talk's
   * conversation header. */
  header?: ReactNode;
  bodyRef?: Ref<HTMLDivElement>;
  /** Let the body use the full width, e.g. a desktop image or terminal. */
  wide?: boolean;
  /** The body does not scroll; the child manages its own layout. */
  fixed?: boolean;
  /** No bar: a display shown full screen draws its own Back and title over
   * the picture. */
  bare?: boolean;
  className?: string;
}

export function Screen({ title, bodyHeading = false, backLabel, stepBack, subtitle, actions, children, footer, footerFlows, top, header, bodyRef, wide, fixed, bare, className }: ScreenProps) {
  const { ui } = useApp();
  const { tab, route, depth, active } = useRoute();
  const below = useUi((s) => s.stacks[tab]?.[depth - 1]);
  const focused = isFocused(route, depth);
  const close = focused && !isFocused(below, depth - 1);
  const tight = useTightLayout();
  const flow = tight && footerFlows === true && !fixed;
  const pinned = useRef<HTMLDivElement>(null);
  const hasPinned = !flow && !!footer;
  // Toasts rise above the pinned footer on screen, so they never cover its
  // composer or Stop.
  useLayoutEffect(() => {
    const el = pinned.current;
    if (!active || !hasPinned || !el) return;
    const root = document.documentElement.style;
    const publish = () => root.setProperty("--screen-footer-h", `${el.offsetHeight}px`);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(publish);
    observer?.observe(el);
    publish();
    return () => { observer?.disconnect(); root.removeProperty("--screen-footer-h"); };
  }, [active, hasPinned]);
  return (
    <div className={`screen${className ? ` ${className}` : ""}`}>
      {bare ? null : depth === 0 ? (
        header ?? <ContextHeader />
      ) : (
        <ScreenBar backLabel={backLabel} title={title} heading={!bodyHeading} subtitle={subtitle} actions={actions} onBack={() => (!close && stepBack ? stepBack() : ui.back())}
          close={close} rail={active} />
      )}
      {top}
      <div ref={bodyRef} className={`screen__body${fixed ? " screen__body--fixed" : ""}`}>
        {wide || fixed ? children : <div className="screen__inner">{children}</div>}
        {flow && footer && <div className="screen__footer screen__footer--flow">{footer}</div>}
      </div>
      {hasPinned && <div ref={pinned} className="screen__footer">{footer}</div>}
    </div>
  );
}

/** Large heading at the top of a tab root's body. */
export function ScreenTitle({ children, meta }: { children: ReactNode; meta?: ReactNode }) {
  return (
    <div className="screen__head">
      <h1 className="screen__title">{children}</h1>
      {meta && <div className="screen__meta">{meta}</div>}
    </div>
  );
}
