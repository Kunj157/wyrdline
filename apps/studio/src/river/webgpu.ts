/**
 * WebGPU river: instanced knots, thread ribbons along cubic Béziers, and
 * rectangles for the spine and level-of-detail buckets. Drawn on demand
 * (no animation loop); the CPU only uploads typed arrays.
 */
import type { Frame } from "./canvas2d";
import { aggregate, LOD_THRESHOLD, threadsOfCall, visibleRange, visibleThreads } from "./data";
import { LANE_Y, palette, type Rgba } from "./theme";

/** Colour slots in the uniform: 0–6 meanings, then these. */
const SPINE = 7;
const THREAD = 8;
const RING = 9;
const THREAD_LIT = 10;
const COLORS = 11;

const SHADER = /* wgsl */ `
struct U {
  offset: f32, scale: f32, width: f32, height: f32,
  selected: i32, active_call: i32, lod: u32, _pad: u32,
  lane_y: vec4<f32>,
  colors: array<vec4<f32>, ${COLORS}>,
};
struct Knot { x: f32, lane: u32, meaning: u32, call: i32 };
struct Thread { head: u32, tail: u32, ambiguous: u32, _pad: u32 };
struct Rect { x: f32, y: f32, w: f32, h: f32, color: vec4<f32> };

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> knots: array<Knot>;
@group(0) @binding(2) var<storage, read> threads: array<Thread>;
@group(0) @binding(3) var<storage, read> rects: array<Rect>;

fn ndc(p: vec2<f32>) -> vec4<f32> {
  return vec4<f32>(p.x / u.width * 2.0 - 1.0, 1.0 - p.y / u.height * 2.0, 0.0, 1.0);
}
fn sx(x: f32) -> f32 { return (x - u.offset) * u.scale; }

const CORNERS = array<vec2<f32>, 6>(
  vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
  vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0));

struct KnotOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) local: vec2<f32>,
  @location(1) color: vec4<f32>,
  @location(2) @interpolate(flat) radius: f32,
  @location(3) @interpolate(flat) flags: u32,
};

@vertex fn knot_vs(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> KnotOut {
  let k = knots[i];
  let selected = i32(i) == u.selected;
  let radius = select(4.0, 6.0, k.lane != 2u);
  let half = select(radius + 1.5, 11.5, selected);
  let c = CORNERS[v] * half;
  let centre = vec2<f32>(sx(k.x), u.lane_y[k.lane]);
  var out: KnotOut;
  out.pos = ndc(centre + c);
  out.local = c;
  var color = select(u.colors[k.meaning], u.colors[${SPINE}], k.lane == 1u);
  let dim = u.active_call >= 0 && k.call != u.active_call && !selected;
  color.a = color.a * select(1.0, 0.4, dim);
  out.color = color;
  out.radius = radius;
  out.flags = select(0u, 1u, k.lane == 1u) | select(0u, 2u, selected);
  return out;
}

@fragment fn knot_fs(in: KnotOut) -> @location(0) vec4<f32> {
  let d = length(in.local);
  var col: vec4<f32>;
  if ((in.flags & 1u) == 1u) {
    // Checkpoint: a rounded square.
    let q = abs(in.local) - vec2<f32>(in.radius - 3.0);
    let sd = length(max(q, vec2(0.0))) - 3.0;
    col = in.color * (1.0 - smoothstep(-0.5, 0.5, sd));
  } else {
    col = in.color * (1.0 - smoothstep(in.radius - 0.75, in.radius + 0.75, d));
  }
  if ((in.flags & 2u) == 2u) {
    let ring = 1.0 - smoothstep(0.5, 1.25, abs(d - 10.0));
    col = mix(col, u.colors[${RING}], ring);
  }
  if (col.a < 0.01) { discard; }
  return vec4<f32>(col.rgb * col.a, col.a);
}

const SEGMENTS = 16u;

struct ThreadOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) color: vec4<f32>,
  @location(1) t: f32,
  @location(2) @interpolate(flat) dashed: u32,
};

fn bezier(a: vec2<f32>, b: vec2<f32>, t: f32) -> vec2<f32> {
  let p1 = vec2<f32>(a.x, u.lane_y[1]);
  let p2 = vec2<f32>(b.x, u.lane_y[1]);
  let s = 1.0 - t;
  return s * s * s * a + 3.0 * s * s * t * p1 + 3.0 * s * t * t * p2 + t * t * t * b;
}

@vertex fn thread_vs(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> ThreadOut {
  let th = threads[i];
  let ka = knots[th.head];
  let kb = knots[th.tail];
  let a = vec2<f32>(sx(ka.x), u.lane_y[0]);
  let b = vec2<f32>(sx(kb.x), u.lane_y[2]);
  let lit = u.active_call >= 0 && ka.call == u.active_call;
  let side = select(-1.0, 1.0, (v & 1u) == 1u);
  let t = f32(v / 2u) / f32(SEGMENTS);
  let p = bezier(a, b, t);
  let q = bezier(a, b, min(1.0, t + 0.01)) - bezier(a, b, max(0.0, t - 0.01));
  let n = normalize(vec2<f32>(-q.y, q.x) + vec2<f32>(1e-6, 0.0));
  let w = select(0.6, 1.1, lit);
  var out: ThreadOut;
  out.pos = ndc(p + n * side * w);
  var color = select(u.colors[${THREAD}], u.colors[${THREAD_LIT}], lit);
  // Dense rivers (LOD) show only the selected call's threads.
  if (u.lod == 1u && !lit) { color.a = 0.0; }
  if (u.active_call >= 0 && !lit) { color.a = color.a * 0.35; }
  out.color = color;
  out.t = t;
  out.dashed = th.ambiguous;
  return out;
}

@fragment fn thread_fs(in: ThreadOut) -> @location(0) vec4<f32> {
  if (in.dashed == 1u && fract(in.t * 24.0) > 0.5) { discard; }
  if (in.color.a < 0.01) { discard; }
  return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
}

struct RectOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

@vertex fn rect_vs(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> RectOut {
  let r = rects[i];
  let c = (CORNERS[v] + vec2(1.0)) * 0.5;
  var out: RectOut;
  out.pos = ndc(vec2<f32>(r.x, r.y) + c * vec2<f32>(r.w, r.h));
  out.color = r.color;
  return out;
}

@fragment fn rect_fs(in: RectOut) -> @location(0) vec4<f32> {
  return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
}
`;

const UNIFORM_BYTES = 48 + COLORS * 16;

// Flag values from the WebGPU spec (the DOM typings declare the types but
// not these constant namespaces).
const STAGE_VERTEX = 0x1;
const STAGE_FRAGMENT = 0x2;
const USAGE_COPY_DST = 0x8;
const USAGE_UNIFORM = 0x40;
const USAGE_STORAGE = 0x80;

export interface GpuRiver {
  draw(f: Frame): { lod: boolean };
  destroy(): void;
}

type RectSpec = [number, number, number, number, Rgba];

/**
 * Create a WebGPU renderer on `canvas`, or the reason WebGPU cannot be
 * used. `onLost` fires if the device is lost later (the caller then
 * falls back to Canvas2D).
 */
export async function createGpuRiver(
  canvas: HTMLCanvasElement,
  onLost: (reason: string) => void,
): Promise<GpuRiver | string> {
  const gpu = typeof navigator === "undefined" ? undefined : navigator.gpu;
  if (!gpu) return "WebGPU is not supported by this browser";
  const adapter = await gpu.requestAdapter().catch(() => null);
  if (!adapter) return "no WebGPU adapter";
  const device = await adapter.requestDevice().catch(() => null);
  if (!device) return "no WebGPU device";
  const context = canvas.getContext("webgpu") as GPUCanvasContext | null;
  if (!context) {
    device.destroy();
    return "no WebGPU canvas context";
  }
  device.pushErrorScope("validation");
  const format = gpu.getPreferredCanvasFormat();
  context.configure({ device, format, alphaMode: "premultiplied" });
  let destroyed = false;
  void device.lost.then((info) => {
    if (!destroyed) onLost(info.message || String(info.reason));
  });

  const module = device.createShaderModule({ code: SHADER });
  const blend: GPUBlendState = {
    color: { srcFactor: "one", dstFactor: "one-minus-src-alpha" },
    alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha" },
  };
  const layout = device.createBindGroupLayout({
    entries: [
      { binding: 0, visibility: STAGE_VERTEX | STAGE_FRAGMENT, buffer: { type: "uniform" } },
      { binding: 1, visibility: STAGE_VERTEX, buffer: { type: "read-only-storage" } },
      { binding: 2, visibility: STAGE_VERTEX, buffer: { type: "read-only-storage" } },
      { binding: 3, visibility: STAGE_VERTEX, buffer: { type: "read-only-storage" } },
    ],
  });
  const pipelineLayout = device.createPipelineLayout({ bindGroupLayouts: [layout] });
  const pipeline = (vs: string, fs: string, topology: GPUPrimitiveTopology) =>
    device.createRenderPipeline({
      layout: pipelineLayout,
      vertex: { module, entryPoint: vs },
      fragment: { module, entryPoint: fs, targets: [{ format, blend }] },
      primitive: { topology },
    });
  const knotPipe = pipeline("knot_vs", "knot_fs", "triangle-list");
  const threadPipe = pipeline("thread_vs", "thread_fs", "triangle-strip");
  const rectPipe = pipeline("rect_vs", "rect_fs", "triangle-list");
  const setupError = await device.popErrorScope();
  if (setupError) {
    destroyed = true;
    device.destroy();
    return `WebGPU setup failed: ${setupError.message}`;
  }

  const uniform = device.createBuffer({
    size: UNIFORM_BYTES,
    usage: USAGE_UNIFORM | USAGE_COPY_DST,
  });
  const storage = (bytes: number) =>
    device.createBuffer({ size: Math.max(16, bytes), usage: USAGE_STORAGE | USAGE_COPY_DST });
  let knotBuf = storage(16);
  let threadBuf = storage(16);
  let rectBuf = storage(32);
  let rectCap = 32;
  // Dense rivers draw only the selected call's threads, copied here.
  let litBuf = storage(16);
  let litCap = 16;
  let litBind: GPUBindGroup | null = null;
  let uploaded: Frame["cols"] | null = null;
  let bind: GPUBindGroup | null = null;

  const upload = (c: Frame["cols"]) => {
    knotBuf.destroy();
    threadBuf.destroy();
    const k = new ArrayBuffer(Math.max(16, c.count * 16));
    const kf = new Float32Array(k);
    const ku = new Uint32Array(k);
    const ki = new Int32Array(k);
    for (let i = 0; i < c.count; i++) {
      kf[i * 4] = c.x[i] ?? 0;
      ku[i * 4 + 1] = c.lane[i] ?? 0;
      ku[i * 4 + 2] = c.meaning[i] ?? 0;
      ki[i * 4 + 3] = c.callIndex[i] ?? -1;
    }
    knotBuf = storage(k.byteLength);
    device.queue.writeBuffer(knotBuf, 0, k);
    const n = c.threadFrom.length;
    const t = new Uint32Array(Math.max(4, n * 4));
    for (let i = 0; i < n; i++) {
      t[i * 4] = c.threadFrom[i] ?? 0;
      t[i * 4 + 1] = c.threadTo[i] ?? 0;
      t[i * 4 + 2] = c.threadAmbiguous[i] ?? 0;
    }
    threadBuf = storage(t.byteLength);
    device.queue.writeBuffer(threadBuf, 0, t);
    uploaded = c;
    bind = null;
    litBind = null;
  };

  const writeRects = (rects: RectSpec[]) => {
    const need = Math.max(1, rects.length) * 32;
    if (need > rectCap) {
      rectBuf.destroy();
      rectCap = Math.max(need, rectCap * 2);
      rectBuf = storage(rectCap);
      bind = null;
      litBind = null;
    }
    const a = new Float32Array(rects.length * 8);
    rects.forEach(([x, y, w, h, col], i) => a.set([x, y, w, h, ...col], i * 8));
    device.queue.writeBuffer(rectBuf, 0, a);
  };

  return {
    draw(f) {
      const c = f.cols;
      if (uploaded !== c) upload(c);
      const pw = Math.max(1, Math.round(f.width * f.dpr));
      const ph = Math.max(1, Math.round(f.height * f.dpr));
      if (canvas.width !== pw || canvas.height !== ph) {
        canvas.width = pw;
        canvas.height = ph;
      }
      const [from, to] = visibleRange(c, f.cam, f.width);
      const lod = to - from > LOD_THRESHOLD;
      const rects: RectSpec[] = [[0, LANE_Y[1] - 1, f.width, 2, palette.thread]];
      if (lod) {
        for (const lane of [0, 1, 2]) {
          for (const b of aggregate(c, f.cam, from, to, lane)) {
            const h = Math.min(18, 3 + Math.log2(b.count) * 3);
            const col = lane === 1 ? palette.spine : (palette.knots[b.meaning] ?? palette.thread);
            rects.push([b.sx, (LANE_Y[lane] ?? 0) - h / 2, 2, h, col]);
          }
        }
      }
      writeRects(rects);

      let lit: number[] = [];
      if (lod && f.activeCall >= 0) {
        lit = threadsOfCall(c, f.activeCall);
        const need = Math.max(1, lit.length) * 16;
        if (need > litCap) {
          litBuf.destroy();
          litCap = Math.max(need, litCap * 2);
          litBuf = storage(litCap);
          litBind = null;
        }
        const t = new Uint32Array(lit.length * 4);
        lit.forEach((i, j) =>
          t.set([c.threadFrom[i] ?? 0, c.threadTo[i] ?? 0, c.threadAmbiguous[i] ?? 0, 0], j * 4),
        );
        device.queue.writeBuffer(litBuf, 0, t);
      }

      const u = new ArrayBuffer(UNIFORM_BYTES);
      const uf = new Float32Array(u);
      const ui = new Int32Array(u);
      const uu = new Uint32Array(u);
      uf.set([f.cam.offset, f.cam.scale, f.width, f.height], 0);
      ui[4] = f.selected;
      ui[5] = f.activeCall;
      uu[6] = lod ? 1 : 0;
      uf.set([...LANE_Y, 0], 8);
      const slots: Rgba[] = [...palette.knots];
      slots[SPINE] = palette.spine;
      slots[THREAD] = palette.thread;
      slots[RING] = palette.ring;
      slots[THREAD_LIT] = palette.threadLit;
      slots.forEach((col, i) => uf.set(col, 12 + i * 4));
      device.queue.writeBuffer(uniform, 0, u);

      litBind ??= device.createBindGroup({
        layout,
        entries: [
          { binding: 0, resource: { buffer: uniform } },
          { binding: 1, resource: { buffer: knotBuf } },
          { binding: 2, resource: { buffer: litBuf } },
          { binding: 3, resource: { buffer: rectBuf } },
        ],
      });
      bind ??= device.createBindGroup({
        layout,
        entries: [
          { binding: 0, resource: { buffer: uniform } },
          { binding: 1, resource: { buffer: knotBuf } },
          { binding: 2, resource: { buffer: threadBuf } },
          { binding: 3, resource: { buffer: rectBuf } },
        ],
      });
      const enc = device.createCommandEncoder();
      const pass = enc.beginRenderPass({
        colorAttachments: [
          {
            view: context.getCurrentTexture().createView(),
            loadOp: "clear",
            storeOp: "store",
            clearValue: { r: 0, g: 0, b: 0, a: 0 },
          },
        ],
      });
      pass.setBindGroup(0, bind);
      pass.setPipeline(rectPipe);
      pass.draw(6, rects.length);
      const [t0, t1] = visibleThreads(c, from, to);
      if (!lod && t1 > t0) {
        pass.setPipeline(threadPipe);
        pass.draw(2 * 17, t1 - t0, 0, t0);
      }
      if (!lod && to > from) {
        pass.setPipeline(knotPipe);
        pass.draw(6, to - from, 0, from);
      } else if (lod && f.selected >= 0) {
        pass.setPipeline(knotPipe);
        pass.draw(6, 1, 0, f.selected);
      }
      if (lit.length > 0) {
        pass.setBindGroup(0, litBind);
        pass.setPipeline(threadPipe);
        pass.draw(2 * 17, lit.length);
        pass.setBindGroup(0, bind);
      }
      pass.end();
      device.queue.submit([enc.finish()]);
      return { lod };
    },
    destroy() {
      destroyed = true;
      knotBuf.destroy();
      threadBuf.destroy();
      rectBuf.destroy();
      litBuf.destroy();
      uniform.destroy();
      device.destroy();
    },
  };
}
