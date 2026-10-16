import { type KeyboardEvent, type PointerEvent, useEffect, useMemo, useRef, useState } from "react";

import type { RiverLayout } from "../lib/model";
import { drawCanvas2d } from "./canvas2d";
import { type Camera, clampCamera, hitTest, toColumns, zoomAt } from "./data";
import { nextKnot } from "./nav";
import { LANE_Y, RIVER_HEIGHT } from "./theme";
import { createGpuRiver, type GpuRiver } from "./webgpu";

export type Renderer = "webgpu" | "canvas2d" | "unavailable";

export interface RiverCanvasProps {
  layout: RiverLayout;
  selected: number | null;
  onSelect: (seq: number) => void;
  /** Try WebGPU first (default) or go straight to Canvas2D. */
  prefer?: "webgpu" | "canvas2d";
  /** Reports which renderer is drawing; "unavailable" means use the table. */
  onRenderer?: (r: Renderer) => void;
}

/**
 * The Causal River on a canvas: WebGPU when available, Canvas2D when not
 * (or when the GPU device is lost). Wheel zooms around the pointer, drag
 * pans, click selects, and the keyboard works as in the SVG river plus
 * `[` and `]` to zoom. Draws only when something changed.
 */
export function RiverCanvas({
  layout,
  selected,
  onSelect,
  prefer = "webgpu",
  onRenderer,
}: RiverCanvasProps) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const gpu = useRef<GpuRiver | null>(null);
  const [renderer, setRenderer] = useState<Renderer | null>(null);
  const [why, setWhy] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);
  const [width, setWidth] = useState(800);
  const cols = useMemo(() => toColumns(layout), [layout]);
  const [cam, setCam] = useState<Camera>({ offset: 0, scale: 1 });
  const drag = useRef<{ x: number; offset: number; moved: boolean } | null>(null);
  const selIndex = useMemo(
    () => layout.knots.findIndex((k) => k.seq === selected),
    [layout, selected],
  );
  const activeCall = selIndex >= 0 ? (cols.callIndex[selIndex] ?? -1) : -1;

  // Width follows the container.
  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) =>
      setWidth(Math.max(200, Math.floor(e?.contentRect.width ?? 800))),
    );
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Pick a renderer; a lost device bumps the epoch, which remounts the
  // canvas (a WebGPU canvas cannot switch to a 2D context) as Canvas2D.
  useEffect(() => {
    let cancelled = false;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const choose = async () => {
      if (prefer === "webgpu" && epoch === 0) {
        const g = await createGpuRiver(canvas, (reason) => {
          gpu.current = null;
          setWhy(`WebGPU device lost: ${reason}`);
          setEpoch((e) => e + 1);
        });
        if (cancelled) {
          if (typeof g !== "string") g.destroy();
          return;
        }
        if (typeof g !== "string") {
          gpu.current = g;
          setRenderer("webgpu");
          return;
        }
        // The canvas may already hold a WebGPU context, which rules out a
        // 2D one: retry on a fresh canvas.
        setWhy(g);
        setEpoch(1);
        return;
      }
      setRenderer(canvas.getContext("2d") ? "canvas2d" : "unavailable");
    };
    choose().catch((e: unknown) => {
      // A renderer that throws while starting falls back like a lost device.
      gpu.current = null;
      setWhy(`renderer failed to start: ${e instanceof Error ? e.message : String(e)}`);
      setEpoch((n) => (n === 0 ? 1 : n));
      if (epoch > 0) setRenderer("unavailable");
    });
    return () => {
      cancelled = true;
      gpu.current?.destroy();
      gpu.current = null;
    };
  }, [prefer, epoch]);

  useEffect(() => {
    if (renderer) onRenderer?.(renderer);
  }, [renderer, onRenderer]);

  // Draw on change, never while the tab is hidden.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !renderer || renderer === "unavailable") return;
    let raf = 0;
    const draw = () => {
      if (document.hidden) return;
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      const frame = { cols, cam, width, height: RIVER_HEIGHT, dpr, selected: selIndex, activeCall };
      if (renderer === "webgpu" && gpu.current) {
        gpu.current.draw(frame);
      } else if (renderer === "canvas2d") {
        const pw = Math.round(width * dpr);
        const ph = Math.round(RIVER_HEIGHT * dpr);
        if (canvas.width !== pw || canvas.height !== ph) {
          canvas.width = pw;
          canvas.height = ph;
        }
        const ctx = canvas.getContext("2d");
        if (ctx) drawCanvas2d(ctx, frame);
      }
    };
    raf = requestAnimationFrame(draw);
    const onVisible = () => {
      if (!document.hidden) raf = requestAnimationFrame(draw);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      cancelAnimationFrame(raf);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [renderer, cols, cam, width, selIndex, activeCall]);

  // Keep the selection in view: adjust the camera while rendering when the
  // selection changes (React's pattern for state derived from props).
  const [followed, setFollowed] = useState(selIndex);
  if (selIndex !== followed) {
    setFollowed(selIndex);
    if (selIndex >= 0) {
      const x = cols.x[selIndex] ?? 0;
      const sx = (x - cam.offset) * cam.scale;
      if (sx < 24 || sx > width - 24) {
        setCam(clampCamera({ ...cam, offset: x - width / 2 / cam.scale }, cols.width, width));
      }
    }
  }

  const local = (e: { clientX: number; clientY: number }) => {
    const r = canvasRef.current?.getBoundingClientRect();
    return { x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) };
  };

  const onWheel = (e: React.WheelEvent) => {
    const { x } = local(e);
    setCam((c) =>
      clampCamera(
        e.shiftKey
          ? { ...c, offset: c.offset + e.deltaY / c.scale }
          : zoomAt(c, Math.exp(-e.deltaY * 0.0015), x),
        cols.width,
        width,
      ),
    );
  };

  const onPointerDown = (e: PointerEvent) => {
    drag.current = { x: e.clientX, offset: cam.offset, moved: false };
    (e.target as Element).setPointerCapture?.(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.x;
    if (Math.abs(dx) > 3) d.moved = true;
    if (d.moved)
      setCam((c) => clampCamera({ ...c, offset: d.offset - dx / c.scale }, cols.width, width));
  };
  const onPointerUp = (e: PointerEvent) => {
    const d = drag.current;
    drag.current = null;
    if (d?.moved) return;
    const { x, y } = local(e);
    const i = hitTest(cols, cam, x, y, (l) => LANE_Y[l] ?? 0);
    const seq = i >= 0 ? cols.seq[i] : undefined;
    if (seq !== undefined) onSelect(seq);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "[" || e.key === "]") {
      e.preventDefault();
      setCam((c) =>
        clampCamera(zoomAt(c, e.key === "]" ? 1.5 : 1 / 1.5, width / 2), cols.width, width),
      );
      return;
    }
    const i = nextKnot(layout.knots, selIndex, e.key);
    const k = i >= 0 ? layout.knots[i] : undefined;
    if (k) {
      e.preventDefault();
      onSelect(k.seq);
    }
  };

  const label = selIndex >= 0 ? layout.knots[selIndex]?.label : undefined;
  return (
    <div ref={wrap} className="rounded-card border border-border bg-surface-1">
      <div
        role="application"
        aria-roledescription="causal river"
        aria-label={`Causal river: ${layout.knots.length} events. Arrow keys move, n and p jump between notable events, [ and ] zoom.`}
        tabIndex={0}
        onKeyDown={onKey}
        data-renderer={renderer ?? "starting"}
        data-fallback-reason={why ?? undefined}
        title={why ? `Using ${renderer ?? "a fallback"}: ${why}` : undefined}
        className="relative focus-visible:outline-2 focus-visible:outline-accent"
      >
        <canvas
          key={epoch}
          ref={canvasRef}
          onWheel={onWheel}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          style={{ width, height: RIVER_HEIGHT, touchAction: "none" }}
          className="block cursor-grab active:cursor-grabbing"
        />
        <span className="pointer-events-none absolute top-1 left-2 text-[10px] tracking-widest text-text-muted uppercase">
          intent
        </span>
        <span className="pointer-events-none absolute bottom-1 left-2 text-[10px] tracking-widest text-text-muted uppercase">
          effects
        </span>
      </div>
      <p className="sr-only" aria-live="polite">
        {label ? `Selected #${selected}: ${label}` : ""}
      </p>
    </div>
  );
}
