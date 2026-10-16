import type { ReactNode } from "react";

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded-sm border border-border-strong bg-surface-2 px-1.5 py-0.5 font-mono text-[0.75em] text-text-secondary">
      {children}
    </kbd>
  );
}
