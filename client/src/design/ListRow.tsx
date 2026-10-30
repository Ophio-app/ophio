// SPDX-License-Identifier: Apache-2.0

import { Check, ChevronRight, type LucideIcon } from "lucide-react";
import { type ReactNode, useId } from "react";

export interface ListRowProps {
  icon?: LucideIcon;
  /** Custom leading visual instead of an icon. */
  leading?: ReactNode;
  title: ReactNode;
  subtitle?: ReactNode;
  meta?: ReactNode;
  trailing?: ReactNode;
  onClick?: () => void;
  /** Show a chevron: the row opens something. */
  chevron?: boolean;
  selected?: boolean;
  /** Makes the row one option of a radiogroup, checked or not. */
  checked?: boolean;
  /** Makes the row a checkbox, ticked or not: one of several that can be picked together. */
  ticked?: boolean;
  /** Keeps the row visible but not tappable, e.g. while the phone is busy. */
  disabled?: boolean;
  className?: string;
  /** Replaces the row's name; its subtitle is still read, as its description. */
  ariaLabel?: string;
}

/** A tappable row. Title and subtitle wrap at large text; nothing is truncated to illegibility. */
export function ListRow({ icon: Icon, leading, title, subtitle, meta, trailing, onClick, chevron, selected, checked, ticked, disabled, className, ariaLabel }: ListRowProps) {
  const subtitleId = useId(),
    described = onClick && ariaLabel && subtitle ? subtitleId : undefined;
  const radio = checked !== undefined;
  const box = !radio && ticked !== undefined;
  const body = (
    <>
      {leading ?? (Icon && <span className="row__icon" aria-hidden><Icon strokeWidth={2} /></span>)}
      <span className="row__main">
        <span className="row__text">
          <span className="row__title">{title}</span>
          {subtitle && <span className="row__subtitle" id={described}>{subtitle}</span>}
        </span>
        {meta && <span className="row__meta">{meta}</span>}
        {trailing}
      </span>
      {/* Which option is the one shows as a mark, not only as a tint a pressed row also has. */}
      {radio && <span className="row__radio" aria-hidden />}
      {box && <span className="row__tick" aria-hidden><Check strokeWidth={3} /></span>}
      {chevron && <ChevronRight className="row__chevron" aria-hidden />}
    </>
  );
  const cls = ["row", onClick && "row--action", (selected || checked || ticked) && "is-selected", className].filter(Boolean).join(" ");
  return onClick ? (
    <button type="button" className={cls} onClick={onClick} disabled={disabled} role={radio ? "radio" : box ? "checkbox" : undefined} aria-checked={radio ? checked : box ? ticked : undefined}
      aria-current={(!radio && !box && selected) || undefined} aria-label={ariaLabel} aria-describedby={described}>
      {body}
    </button>
  ) : radio ? (
    // An option that can't be chosen now stays readable, with its reason.
    <div className={cls} role="radio" aria-checked={checked} aria-disabled>{body}</div>
  ) : (
    <div className={cls}>{body}</div>
  );
}

export function Section({ title, action, children, className, id }: { title?: ReactNode; action?: ReactNode; children: ReactNode; className?: string; id?: string }) {
  return (
    <section className={["section", className].filter(Boolean).join(" ")} aria-labelledby={title && id ? id : undefined}>
      {(title || action) && (
        <div className="section__head">
          {title && <h2 className="section__title" id={id}>{title}</h2>}
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

export function Empty({ icon: Icon, title, children, action }: { icon?: LucideIcon; title: ReactNode; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty">
      {Icon && <span className="empty__icon" aria-hidden><Icon strokeWidth={1.6} /></span>}
      <p className="empty__title">{title}</p>
      {children && <div className="empty__body">{children}</div>}
      {action && <div className="empty__action">{action}</div>}
    </div>
  );
}
