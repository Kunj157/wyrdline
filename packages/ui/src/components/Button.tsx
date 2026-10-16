import type { ButtonHTMLAttributes, ReactNode, Ref } from "react";

import { cx } from "./cx";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

const variants: Record<ButtonVariant, string> = {
  primary: "bg-accent text-on-accent hover:bg-accent-hover",
  secondary: "border border-border-strong bg-surface-1 text-text-primary hover:bg-surface-2",
  ghost: "text-text-secondary hover:bg-surface-2 hover:text-text-primary",
  danger: "bg-verdict-denied text-on-accent hover:opacity-90",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  icon?: ReactNode;
  ref?: Ref<HTMLButtonElement>;
}

export function Button({
  variant = "secondary",
  size = "md",
  icon,
  className,
  children,
  type = "button",
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={cx(
        "inline-flex items-center justify-center gap-1.5 rounded-sm font-medium",
        "transition-colors duration-[var(--duration-fast)] ease-[var(--ease-out)]",
        "disabled:cursor-not-allowed disabled:opacity-50",
        size === "sm" ? "h-7 px-2.5 text-xs" : "h-9 px-3.5 text-sm",
        variants[variant],
        className,
      )}
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}
