import type { HTMLAttributes } from "react";

import { cx } from "./cx";

export function Card({ className, ...rest }: HTMLAttributes<HTMLElement>) {
  return (
    <section
      className={cx("rounded-card border border-border bg-surface-1 p-4 shadow-2", className)}
      {...rest}
    />
  );
}
