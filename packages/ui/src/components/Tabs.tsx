import { type KeyboardEvent, type ReactNode, useId, useRef } from "react";

import { cx } from "./cx";

export interface Tab {
  id: string;
  label: ReactNode;
  panel: ReactNode;
}

export interface TabsProps {
  tabs: Tab[];
  value: string;
  onChange: (id: string) => void;
  label: string;
  className?: string;
}

/** WAI-ARIA tabs: arrow keys, Home, and End move between tabs. */
export function Tabs({ tabs, value, onChange, label, className }: TabsProps) {
  const base = useId();
  const refs = useRef<Array<HTMLButtonElement | null>>([]);
  const index = Math.max(
    0,
    tabs.findIndex((t) => t.id === value),
  );

  const move = (e: KeyboardEvent) => {
    const last = tabs.length - 1;
    const next =
      e.key === "ArrowRight"
        ? (index + 1) % tabs.length
        : e.key === "ArrowLeft"
          ? (index - 1 + tabs.length) % tabs.length
          : e.key === "Home"
            ? 0
            : e.key === "End"
              ? last
              : -1;
    if (next < 0) return;
    e.preventDefault();
    const tab = tabs[next];
    if (!tab) return;
    onChange(tab.id);
    refs.current[next]?.focus();
  };

  const active = tabs[index];
  return (
    <div className={className}>
      <div role="tablist" aria-label={label} className="flex gap-1 border-b border-border">
        {tabs.map((t, i) => (
          <button
            key={t.id}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role="tab"
            id={`${base}-tab-${t.id}`}
            aria-selected={i === index}
            aria-controls={`${base}-panel-${t.id}`}
            tabIndex={i === index ? 0 : -1}
            onKeyDown={move}
            onClick={() => onChange(t.id)}
            className={cx(
              "-mb-px border-b-2 px-3 py-2 text-sm",
              i === index
                ? "border-accent font-medium text-text-primary"
                : "border-transparent text-text-secondary hover:text-text-primary",
            )}
          >
            {t.label}
          </button>
        ))}
      </div>
      {active && (
        <div
          role="tabpanel"
          id={`${base}-panel-${active.id}`}
          aria-labelledby={`${base}-tab-${active.id}`}
          tabIndex={0}
          className="pt-3"
        >
          {active.panel}
        </div>
      )}
    </div>
  );
}
