// SPDX-License-Identifier: Apache-2.0
import { createContext, useContext } from "react";
import { Columns2, PanelLeftClose, PanelRightClose } from "lucide-react";
import { Button, IconButton } from "../design/Button";
import type { Side } from "./split";

export interface SplitControlsValue { side: Side; available: boolean; opened: boolean; twoVisible: boolean; open(): void; close(): void }
export const SplitControls = createContext<SplitControlsValue | null>(null);
/** Header controls also exist when a foldable is too narrow for the wide rail. */
export function SplitControl({ rail = false, focused = false }: { rail?: boolean; focused?: boolean }) {
  const split = useContext(SplitControls);
  if (!split || (!split.available && !split.opened)) return null;
  if (rail && (split.twoVisible || split.opened)) return null;
  const label = split.opened ? `Close ${split.side} side` : "Open split view";
  const icon = split.opened ? (split.side === "left" ? PanelLeftClose : PanelRightClose) : Columns2;
  return (
    <span className={`split-control${rail ? " split-control--rail" : " split-control--header"}${split.opened ? " split-control--close" : ""}${focused ? " split-control--focused" : ""}`}>
      {split.opened ? <IconButton icon={icon} label={label} onClick={split.close} /> : rail ? (
        <button type="button" className="nav__tab" aria-label={label} onClick={split.open}>
          <span className="nav__pill"><Columns2 className="nav__icon" aria-hidden strokeWidth={2} /></span>
          <span className="nav__label">Split view</span>
        </button>
      ) : <Button size="sm" icon={Columns2} aria-label={label} className="split-control__open" onClick={split.open}>Split view</Button>}
    </span>
  );
}
