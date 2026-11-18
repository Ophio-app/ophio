// SPDX-License-Identifier: Apache-2.0
import { createContext, useContext, useLayoutEffect, useState, type CSSProperties, type ReactNode } from "react";
import { useKeyboardOpen, usePaneLayout } from "./viewport";

export interface PaneScope {
  id: string;
  active: boolean;
  visible: boolean;
  split: boolean;
  root: HTMLElement | null;
  width: number;
  height: number;
}

export const PaneContext = createContext<PaneScope | null>(null);

/** The containing app surface, or null in the legacy single-view shell. */
export function usePane(): PaneScope | null {
  return useContext(PaneContext);
}

export function PaneFrame({ id, label, active, visible, split, onActivate, children }: {
  id: string;
  label: string;
  active: boolean;
  visible: boolean;
  split: boolean;
  onActivate(): void;
  children: ReactNode;
}) {
  const [root, setRoot] = useState<HTMLElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    if (!root) return;
    const measure = () => {
      const { width, height } = root.getBoundingClientRect();
      // A display:none slot keeps its last real size until it is shown again.
      if (width <= 0 || height <= 0) return;
      setSize((old) => old.width === width && old.height === height ? old : { width, height });
    };
    measure();
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(measure);
      observer.observe(root);
      return () => observer.disconnect();
    }
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [root, visible]);
  const pane: PaneScope = { id, active, visible, split, root, ...size };
  const layout = usePaneLayout(pane);
  const keyboardOpen = useKeyboardOpen();
  const classes = ["pane", active && "pane-active", split && "pane-split", keyboardOpen && "kb-open",
    layout.tight && "app-tight", layout.narrow && "app-narrow",
    layout.cramped && "app-cramped", layout.sideways && "app-sideways"].filter(Boolean).join(" ");
  const viewport = size.height > 0 && size.width > 0 ? {
    "--pane-vh": `${size.height / 100}px`,
    "--pane-vw": `${size.width / 100}px`,
  } as CSSProperties : undefined;
  return <section ref={setRoot} className={classes} role="region" aria-label={label} style={viewport}
    data-pane-id={id} inert={!visible} aria-hidden={!visible || undefined}
    onPointerDownCapture={() => { if (visible) onActivate(); }}
    onFocusCapture={() => { if (visible) onActivate(); }}>
    <PaneContext.Provider value={pane}>{children}</PaneContext.Provider>
  </section>;
}
