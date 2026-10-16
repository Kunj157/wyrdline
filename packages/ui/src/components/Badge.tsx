import {
  Ban,
  CircleCheck,
  CircleHelp,
  Footprints,
  Hourglass,
  MessageSquare,
  TriangleAlert,
} from "lucide-react";
import type { ComponentType } from "react";

import { cx } from "./cx";

/** Every meaning the palette encodes; colour is never the only signal. */
export type Meaning =
  "intent" | "effect" | "divergence" | "denied" | "irreversible" | "ok" | "unknown";

const style: Record<Meaning, { cls: string; icon: ComponentType<{ className?: string }> }> = {
  intent: { cls: "text-accent bg-accent-subtle", icon: MessageSquare },
  effect: { cls: "text-effect bg-vivid-bone/40", icon: Footprints },
  divergence: { cls: "text-verdict-divergence bg-vivid-vermilion/12", icon: TriangleAlert },
  denied: { cls: "text-verdict-denied bg-vivid-red/12", icon: Ban },
  irreversible: { cls: "text-verdict-irreversible bg-vivid-ochre/20", icon: Hourglass },
  ok: { cls: "text-verdict-ok bg-vivid-green/15", icon: CircleCheck },
  unknown: { cls: "text-verdict-unknown bg-surface-3", icon: CircleHelp },
};

export interface BadgeProps {
  meaning: Meaning;
  /** Visible text; defaults to the meaning's name. */
  children?: string;
  className?: string;
}

export function Badge({ meaning, children, className }: BadgeProps) {
  const { cls, icon: Icon } = style[meaning];
  return (
    <span
      data-meaning={meaning}
      className={cx(
        "inline-flex items-center gap-1 rounded-sm px-1.5 py-0.5 text-xs font-medium",
        cls,
        className,
      )}
    >
      <Icon className="size-3.5 shrink-0" aria-hidden="true" />
      {children ?? meaning}
    </span>
  );
}
