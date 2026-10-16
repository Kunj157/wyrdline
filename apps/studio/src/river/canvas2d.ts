/** Canvas2D river: the fallback when WebGPU is unavailable or lost. */
import {
  aggregate,
  type Camera,
  type Columns,
  LOD_THRESHOLD,
  threadsOfCall,
  visibleRange,
  visibleThreads,
} from "./data";
import { css, LANE_Y, palette } from "./theme";

export interface Frame {
  cols: Columns;
  cam: Camera;
  /** CSS pixels. */
  width: number;
  height: number;
  dpr: number;
  selected: number;
  activeCall: number;
}

export function drawCanvas2d(ctx: CanvasRenderingContext2D, f: Frame): { lod: boolean } {
  const { cols: c, cam } = f;
  ctx.setTransform(f.dpr, 0, 0, f.dpr, 0, 0);
  ctx.clearRect(0, 0, f.width, f.height);
  const sx = (i: number) => ((c.x[i] ?? 0) - cam.offset) * cam.scale;
  const [from, to] = visibleRange(c, cam, f.width);
  const lod = to - from > LOD_THRESHOLD;

  // Spine.
  ctx.strokeStyle = css(palette.thread);
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(0, LANE_Y[1]);
  ctx.lineTo(f.width, LANE_Y[1]);
  ctx.stroke();

  // Threads ending in view when sparse; only the active call's when dense.
  const [t0, t1] = visibleThreads(c, from, to);
  const list = lod
    ? threadsOfCall(c, f.activeCall)
    : Array.from({ length: Math.max(0, t1 - t0) }, (_, i) => t0 + i);
  for (const t of list) {
    const a = c.threadFrom[t] ?? 0;
    const b = c.threadTo[t] ?? 0;
    const lit = f.activeCall >= 0 && c.callIndex[a] === f.activeCall;
    const [x1, x2] = [sx(a), sx(b)];
    if (Math.max(x1, x2) < 0 || Math.min(x1, x2) > f.width) continue;
    ctx.strokeStyle = css(lit ? palette.threadLit : palette.thread);
    ctx.lineWidth = lit ? 2 : 1;
    ctx.setLineDash(c.threadAmbiguous[t] ? [3, 3] : []);
    ctx.beginPath();
    ctx.moveTo(x1, LANE_Y[0]);
    ctx.bezierCurveTo(x1, LANE_Y[1], x2, LANE_Y[1], x2, LANE_Y[2]);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  if (lod) {
    for (const lane of [0, 1, 2]) {
      for (const bk of aggregate(c, cam, from, to, lane)) {
        const h = Math.min(18, 3 + Math.log2(bk.count) * 3);
        ctx.fillStyle = css(
          lane === 1 ? palette.spine : (palette.knots[bk.meaning] ?? palette.thread),
        );
        ctx.fillRect(bk.sx, (LANE_Y[lane] ?? 0) - h / 2, 2, h);
      }
    }
  } else {
    for (let i = from; i < to; i++) {
      const lane = c.lane[i] ?? 0;
      const x = sx(i);
      const y = LANE_Y[lane] ?? 0;
      const dim = f.activeCall >= 0 && c.callIndex[i] !== f.activeCall && i !== f.selected;
      ctx.globalAlpha = dim ? 0.4 : 1;
      if (lane === 1) {
        ctx.fillStyle = css(palette.spine);
        ctx.fillRect(x - 6, y - 6, 12, 12);
      } else {
        ctx.fillStyle = css(palette.knots[c.meaning[i] ?? 0] ?? palette.thread);
        ctx.beginPath();
        ctx.arc(x, y, lane === 0 ? 6 : 4, 0, Math.PI * 2);
        ctx.fill();
      }
    }
    ctx.globalAlpha = 1;
  }
  if (f.selected >= 0) {
    ctx.strokeStyle = css(palette.ring);
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.arc(sx(f.selected), LANE_Y[c.lane[f.selected] ?? 0] ?? 0, 10, 0, Math.PI * 2);
    ctx.stroke();
  }
  return { lod };
}
