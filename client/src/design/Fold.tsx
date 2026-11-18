// SPDX-License-Identifier: Apache-2.0
//
// A section folded to one line that says its current value, and opens to the
// controls that change it: the app's own expander, in place of a browser's
// disclosure triangle. The whole line is the target; the chevron turns.

import { ChevronDown } from "lucide-react";
import { type ReactNode, useEffect, useId, useState } from "react";
import "./fold.css";

export interface FoldProps {
  /** What the section is, as a section heading reads it. */
  title: ReactNode;
  /** What it is set to now, shown while it is folded and while it is open. */
  value: ReactNode;
  children: ReactNode;
  /** Open to begin with. */
  defaultOpen?: boolean;
  /** Opens it when this turns true, e.g. when something in it needs the person. */
  openWhen?: boolean;
  className?: string;
}

export function Fold({ title, value, children, defaultOpen = false, openWhen = false, className }: FoldProps) {
  const [open, setOpen] = useState(defaultOpen || openWhen);
  const body = useId();
  useEffect(() => { if (openWhen) setOpen(true); }, [openWhen]);
  return (
    <section className={["section", "fold", open && "is-open", className].filter(Boolean).join(" ")}>
      <button type="button" className="fold__head" aria-expanded={open} aria-controls={body} onClick={() => setOpen((o) => !o)}>
        <span className="fold__text">
          <span className="fold__title">{title}</span>
          <span className="fold__value">{value}</span>
        </span>
        <ChevronDown className="fold__chevron" aria-hidden strokeWidth={2.1} />
      </button>
      <div className="fold__body" id={body} hidden={!open}>{children}</div>
    </section>
  );
}
