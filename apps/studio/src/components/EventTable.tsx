import { useVirtualizer } from "@tanstack/react-virtual";
import type { Event } from "@wyrdline/schemas";
import { Badge } from "@wyrdline/ui";
import { useEffect, useRef } from "react";

import { describe, meaningOf, planeOf } from "../lib/model";

export interface EventTableProps {
  events: Event[];
  selected: number | null;
  onSelect: (seq: number) => void;
}

const ROW = 30;
const COLS =
  "grid grid-cols-[3.5rem_5.5rem_minmax(0,1fr)] md:grid-cols-[4.5rem_6.5rem_5rem_minmax(0,1fr)] items-center";

/**
 * Every event as a row: the accessible view of the river. Rows are
 * virtualized, so the table is built from ARIA table roles on a CSS grid.
 */
export function EventTable({ events, selected, onSelect }: EventTableProps) {
  const parent = useRef<HTMLDivElement>(null);
  // TanStack Virtual returns fresh functions each render; the React
  // Compiler skips this component, which is what we want.
  // eslint-disable-next-line react-hooks/incompatible-library
  const v = useVirtualizer({
    count: events.length,
    getScrollElement: () => parent.current,
    estimateSize: () => ROW,
    overscan: 12,
    // jsdom and first paint have no layout: render the first rows anyway.
    initialRect: { width: 800, height: 480 },
  });
  const selIndex = events.findIndex((e) => e.seq === selected);
  useEffect(() => {
    if (selIndex >= 0) v.scrollToIndex(selIndex, { align: "auto" });
  }, [selIndex, v]);

  return (
    <div
      ref={parent}
      role="table"
      aria-label="Session events in order"
      aria-rowcount={events.length + 1}
      className="h-[min(60vh,480px)] overflow-auto rounded-card border border-border bg-surface-1 text-sm"
    >
      <div role="rowgroup" className="sticky top-0 z-10">
        <div
          role="row"
          aria-rowindex={1}
          className={`${COLS} bg-surface-2 text-xs font-medium text-text-secondary`}
        >
          <span role="columnheader" className="px-3 py-1.5">
            Seq
          </span>
          <span role="columnheader" className="px-3 py-1.5">
            Plane
          </span>
          <span role="columnheader" className="hidden px-3 py-1.5 md:block">
            PID
          </span>
          <span role="columnheader" className="px-3 py-1.5">
            Event
          </span>
        </div>
      </div>
      <div role="rowgroup" style={{ height: v.getTotalSize(), position: "relative" }}>
        {v.getVirtualItems().map((row) => {
          const e = events[row.index];
          if (!e) return null;
          const isSel = e.seq === selected;
          return (
            <div
              key={e.seq}
              role="row"
              aria-rowindex={row.index + 2}
              aria-selected={isSel}
              onClick={() => onSelect(e.seq)}
              className={`${COLS} absolute inset-x-0 cursor-pointer border-b border-border ${isSel ? "bg-accent-subtle" : "hover:bg-surface-2"}`}
              style={{ height: ROW, transform: `translateY(${row.start}px)` }}
            >
              <span role="cell" className="px-3 font-mono text-xs tabular-nums text-text-muted">
                {e.seq}
              </span>
              <span role="cell" className="px-3">
                <Badge meaning={meaningOf(e)}>{planeOf(e)}</Badge>
              </span>
              <span role="cell" className="hidden px-3 font-mono text-xs text-text-muted md:block">
                {e.process?.pid ?? ""}
              </span>
              <span role="cell" className="min-w-0 px-3">
                <button
                  type="button"
                  className="block w-full truncate text-left"
                  onClick={() => onSelect(e.seq)}
                >
                  {describe(e)}
                </button>
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
