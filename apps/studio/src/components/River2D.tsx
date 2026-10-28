import { type KeyboardEvent, useMemo, useRef } from "react";

import type { Knot, RiverLayout } from "../lib/model";

const LANE_Y = { intent: 36, spine: 96, effect: 156 } as const;
const HEIGHT = 196;

const fill: Record<Knot["meaning"], string> = {
  intent: "var(--color-accent)",
  effect: "var(--color-effect)",
  divergence: "var(--color-verdict-divergence)",
  denied: "var(--color-verdict-denied)",
  irreversible: "var(--color-verdict-irreversible)",
  ok: "var(--color-verdict-ok)",
  unknown: "var(--color-verdict-unknown)",
};

export interface River2DProps {
  layout: RiverLayout;
  selected: number | null;
  onSelect: (seq: number) => void;
  /** Horizontal zoom (1 = one step per event). */
  zoom: number;
}

/**
 * The Causal River in SVG: tool calls above, checkpoints on the spine,
 * effects below, and threads from each call to its effects. The SVG is
 * one focusable widget; arrow keys move between knots, `n`/`p` jump to
 * the next or previous notable knot (denied, irreversible, divergence).
 * Every knot is also a row in the events table, the accessible source of
 * truth.
 */
export function River2D({ layout, selected, onSelect, zoom }: River2DProps) {
  const ref = useRef<SVGSVGElement>(null);
  const bySeq = useMemo(() => new Map(layout.knots.map((k) => [k.seq, k])), [layout]);
  const selectedKnot = selected == null ? undefined : bySeq.get(selected);
  const activeCall = selectedKnot?.callId ?? null;
  const index = layout.knots.findIndex((k) => k.seq === selected);

  const onKey = (e: KeyboardEvent) => {
    const knots = layout.knots;
    if (knots.length === 0) return;
    const notable = (k: Knot) => ["denied", "irreversible", "divergence"].includes(k.meaning);
    let next: Knot | undefined;
    if (e.key === "ArrowRight") next = knots[Math.min(knots.length - 1, index + 1)];
    else if (e.key === "ArrowLeft") next = knots[Math.max(0, index - 1)];
    else if (e.key === "Home") next = knots[0];
    else if (e.key === "End") next = knots[knots.length - 1];
    else if (e.key === "n") next = knots.slice(index + 1).find(notable);
    else if (e.key === "p") next = knots.slice(0, Math.max(0, index)).reverse().find(notable);
    if (next) {
      e.preventDefault();
      onSelect(next.seq);
    }
  };

  const width = Math.max(layout.width * zoom + 24, 320);
  const x = (k: Knot) => k.x * zoom;
  return (
    <div className="overflow-x-auto rounded-card border border-border bg-surface-1">
      <svg
        ref={ref}
        role="application"
        aria-roledescription="causal river"
        aria-label={`Causal river: ${layout.knots.length} events. Arrow keys move, n and p jump between notable events.`}
        tabIndex={0}
        onKeyDown={onKey}
        width={width}
        height={HEIGHT}
        className="block focus-visible:outline-2 focus-visible:outline-accent"
      >
        <text
          x={8}
          y={LANE_Y.intent - 22}
          className="fill-text-muted text-[10px] uppercase tracking-widest"
        >
          intent
        </text>
        <text
          x={8}
          y={LANE_Y.effect + 32}
          className="fill-text-muted text-[10px] uppercase tracking-widest"
        >
          effects
        </text>
        <line
          x1={0}
          x2={width}
          y1={LANE_Y.spine}
          y2={LANE_Y.spine}
          stroke="var(--color-border-strong)"
          strokeWidth={2}
        />
        {layout.threads.map((t) => {
          const a = bySeq.get(t.from);
          const b = bySeq.get(t.to);
          if (!a || !b) return null;
          const lit = activeCall === t.callId;
          const [x1, x2] = [x(a), x(b)];
          return (
            <path
              key={`${t.from}-${t.to}`}
              d={`M ${x1} ${LANE_Y.intent} C ${x1} ${LANE_Y.spine}, ${x2} ${LANE_Y.spine}, ${x2} ${LANE_Y.effect}`}
              fill="none"
              stroke={lit ? "var(--color-accent)" : "var(--color-border-strong)"}
              strokeWidth={lit ? 2 : 1}
              strokeDasharray={t.ambiguous ? "3 3" : undefined}
              opacity={activeCall && !lit ? 0.35 : 1}
            />
          );
        })}
        {layout.knots.map((k) => {
          const isSel = k.seq === selected;
          const dim = activeCall != null && k.callId !== activeCall && !isSel;
          const y = LANE_Y[k.lane];
          return (
            <g
              key={k.seq}
              data-seq={k.seq}
              data-lane={k.lane}
              data-meaning={k.meaning}
              onClick={() => onSelect(k.seq)}
              className="cursor-pointer"
              opacity={dim ? 0.4 : 1}
            >
              <title>{`#${k.seq} ${k.label}`}</title>
              {k.lane === "spine" ? (
                <rect
                  x={x(k) - 6}
                  y={y - 6}
                  width={12}
                  height={12}
                  rx={3}
                  fill="var(--color-vivid-violet)"
                />
              ) : (
                <circle cx={x(k)} cy={y} r={k.lane === "intent" ? 6 : 4} fill={fill[k.meaning]} />
              )}
              {isSel && (
                <circle
                  cx={x(k)}
                  cy={y}
                  r={10}
                  fill="none"
                  stroke="var(--color-text-primary)"
                  strokeWidth={1.5}
                />
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}
