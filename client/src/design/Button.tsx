// SPDX-License-Identifier: Apache-2.0

import type { LucideIcon } from "lucide-react";
import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export type ButtonVariant = "primary" | "secondary" | "quiet" | "stop" | "stop-outline" | "warn";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: LucideIcon;
  trailingIcon?: LucideIcon;
  block?: boolean;
  /** Shows that the phone is waiting; the label should say for what. */
  pending?: boolean;
  children?: ReactNode;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", icon: Icon, trailingIcon: Trailing, block, pending, className, children, disabled, type, ...rest },
  ref,
) {
  const cls = ["btn", `btn--${variant}`, `btn--${size}`, block && "btn--block", pending && "is-pending", className]
    .filter(Boolean)
    .join(" ");
  return (
    <button ref={ref} type={type ?? "button"} className={cls} disabled={disabled || pending} aria-busy={pending || undefined} {...rest}>
      {pending ? <span className="btn__spinner" aria-hidden /> : Icon ? <Icon className="btn__icon" aria-hidden strokeWidth={2.1} /> : null}
      {children != null && <span className="btn__label">{children}</span>}
      {Trailing && <Trailing className="btn__icon btn__icon--trailing" aria-hidden strokeWidth={2.1} />}
    </button>
  );
});

export interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  icon: LucideIcon;
  /** Required: icon-only controls always have an accessible name. */
  label: string;
  variant?: "plain" | "tonal" | "primary" | "stop";
  size?: "md" | "lg";
  badge?: number | boolean;
  pressed?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { icon: Icon, label, variant = "plain", size = "md", badge, pressed, className, type, ...rest },
  ref,
) {
  const cls = ["iconbtn", `iconbtn--${variant}`, `iconbtn--${size}`, pressed && "is-pressed", className].filter(Boolean).join(" ");
  return (
    <button
      ref={ref}
      type={type ?? "button"}
      className={cls}
      aria-label={label}
      title={label}
      aria-pressed={pressed}
      {...rest}
    >
      <Icon aria-hidden strokeWidth={2} />
      {badge ? <span className="iconbtn__badge num">{typeof badge === "number" ? (badge > 9 ? "9+" : badge) : ""}</span> : null}
    </button>
  );
});
