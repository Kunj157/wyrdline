/**
 * The Loom: a small 3D take on the Causal River for the hero. Intent knots
 * hang on an upper rail, kernel effects sit on a lower plane, and threads
 * weave down from each tool call to the effects it caused, with checkpoint
 * rings on the spine. Colours come from the shared tokens.
 *
 * Budget (plan §16.1b): instanced knots, one mesh per thread colour, under
 * 30 draw calls, device pixel ratio capped at 2, paused when off-screen or
 * hidden. The caller only mounts it when WebGL works and motion is allowed.
 */
import { meaning, vivid } from "@wyrdline/ui";
import {
  AmbientLight,
  BufferGeometry,
  Color,
  CubicBezierCurve3,
  DirectionalLight,
  Group,
  HemisphereLight,
  InstancedMesh,
  Mesh,
  MeshStandardMaterial,
  Object3D,
  PerspectiveCamera,
  Scene,
  SphereGeometry,
  TorusGeometry,
  TubeGeometry,
  Vector3,
  WebGLRenderer,
} from "three";
import { mergeGeometries } from "three/addons/utils/BufferGeometryUtils.js";

/** A deterministic generator so the weave looks the same on every visit. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

interface Call {
  x: number;
  z: number;
  effects: { x: number; z: number; kind: "effect" | "divergence" | "irreversible" }[];
}

const SPAN = 14;
const TOP = 1.7;
const BOTTOM = -1.7;

function weave(): Call[] {
  const r = rng(157);
  const calls: Call[] = [];
  for (let i = 0; i < 11; i++) {
    const x = -SPAN / 2 + 0.6 + i * ((SPAN - 1.2) / 10) + (r() - 0.5) * 0.4;
    const z = (r() - 0.5) * 2.2;
    const n = 2 + Math.floor(r() * 5);
    const effects: Call["effects"] = [];
    for (let j = 0; j < n; j++) {
      const roll = r();
      effects.push({
        x: x + 0.25 + j * 0.32 + r() * 0.2,
        z: z + (r() - 0.5) * 1.6,
        kind: roll > 0.93 ? "divergence" : roll > 0.86 ? "irreversible" : "effect",
      });
    }
    calls.push({ x, z, effects });
  }
  return calls;
}

export interface LoomHandle {
  dispose(): void;
}

export function mountLoom(canvas: HTMLCanvasElement): LoomHandle {
  const renderer = new WebGLRenderer({ canvas, antialias: true, alpha: true });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  renderer.setClearColor(0x000000, 0);

  const scene = new Scene();
  const camera = new PerspectiveCamera(32, 1, 0.1, 100);
  camera.position.set(0, 1.2, 15);
  camera.lookAt(0, 0, 0);

  scene.add(new HemisphereLight(vivid["vivid-bone"], meaning.accent, 1.4));
  scene.add(new AmbientLight(0xffffff, 0.5));
  const sun = new DirectionalLight(0xffffff, 1.6);
  sun.position.set(4, 8, 6);
  scene.add(sun);

  const loom = new Group();
  loom.rotation.set(0.28, -0.42, 0);
  scene.add(loom);

  const calls = weave();
  const mat = (hex: string, emissive = 0) =>
    new MeshStandardMaterial({
      color: new Color(hex),
      roughness: 0.35,
      metalness: 0.05,
      emissive: new Color(hex),
      emissiveIntensity: emissive,
    });
  const materials = {
    intent: mat(vivid["vivid-verdigris"], 0.25),
    effect: mat(vivid["vivid-bone"], 0.05),
    divergence: mat(vivid["vivid-vermilion"], 0.35),
    irreversible: mat(vivid["vivid-ochre"], 0.3),
    thread: mat(meaning.accent, 0.15),
    threadHot: mat(vivid["vivid-violet"], 0.3),
    ring: mat(vivid["vivid-violet"], 0.45),
    spine: mat(vivid["vivid-sky"], 0.2),
  };

  // Knots, one instanced mesh per meaning.
  const dummy = new Object3D();
  const sphere = new SphereGeometry(1, 20, 14);
  const place = (mesh: InstancedMesh, i: number, p: Vector3, s: number) => {
    dummy.position.copy(p);
    dummy.scale.setScalar(s);
    dummy.updateMatrix();
    mesh.setMatrixAt(i, dummy.matrix);
  };
  const intent = new InstancedMesh(sphere, materials.intent, calls.length);
  calls.forEach((c, i) => place(intent, i, new Vector3(c.x, TOP, c.z), 0.2));
  loom.add(intent);
  const byKind = {
    effect: [] as Vector3[],
    divergence: [] as Vector3[],
    irreversible: [] as Vector3[],
  };
  for (const c of calls)
    for (const e of c.effects) byKind[e.kind].push(new Vector3(e.x, BOTTOM, e.z));
  for (const kind of ["effect", "divergence", "irreversible"] as const) {
    const pts = byKind[kind];
    if (!pts.length) continue;
    const m = new InstancedMesh(sphere, materials[kind], pts.length);
    pts.forEach((p, i) => place(m, i, p, kind === "effect" ? 0.13 : 0.17));
    loom.add(m);
  }

  // Threads: tubes merged into one geometry per colour, drawn in over time.
  const threads: { geo: BufferGeometry; total: number }[] = [];
  const buckets: [BufferGeometry[], BufferGeometry[]] = [[], []];
  calls.forEach((c, i) => {
    for (const e of c.effects) {
      const a = new Vector3(c.x, TOP, c.z);
      const b = new Vector3(e.x, BOTTOM, e.z);
      const curve = new CubicBezierCurve3(
        a,
        new Vector3(a.x + 0.2, 0.2, a.z + 0.8),
        new Vector3(b.x - 0.4, -0.4, b.z - 0.6),
        b,
      );
      buckets[i % 4 === 1 ? 1 : 0].push(new TubeGeometry(curve, 40, 0.028, 6, false));
    }
  });
  buckets.forEach((geos, k) => {
    const geo = mergeGeometries(geos);
    geos.forEach((g) => g.dispose());
    if (!geo) return;
    loom.add(new Mesh(geo, k === 1 ? materials.threadHot : materials.thread));
    threads.push({ geo, total: geo.index?.count ?? 0 });
  });

  // The spine and checkpoint rings.
  const spine = new Mesh(
    new TubeGeometry(
      new CubicBezierCurve3(
        new Vector3(-SPAN / 2, 0, 0),
        new Vector3(-SPAN / 6, 0.1, 0.3),
        new Vector3(SPAN / 6, -0.1, -0.3),
        new Vector3(SPAN / 2, 0, 0),
      ),
      80,
      0.02,
      6,
      false,
    ),
    materials.spine,
  );
  loom.add(spine);
  const rings = new InstancedMesh(new TorusGeometry(0.32, 0.05, 10, 32), materials.ring, 6);
  for (let i = 0; i < 6; i++) {
    dummy.position.set(-SPAN / 2 + 1.4 + i * 2.2, 0, 0);
    dummy.rotation.set(0, Math.PI / 2, 0);
    dummy.scale.setScalar(1);
    dummy.updateMatrix();
    rings.setMatrixAt(i, dummy.matrix);
  }
  loom.add(rings);

  // Sizing, pointer parallax, and a loop that only runs while visible.
  const resize = () => {
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (!w || !h) return;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.position.z = w / h < 1.2 ? 21 : 15;
    camera.updateProjectionMatrix();
  };
  const ro = new ResizeObserver(resize);
  ro.observe(canvas);
  resize();

  const pointer = { x: 0, y: 0 };
  const onPointer = (e: PointerEvent) => {
    pointer.x = (e.clientX / window.innerWidth) * 2 - 1;
    pointer.y = (e.clientY / window.innerHeight) * 2 - 1;
  };
  window.addEventListener("pointermove", onPointer, { passive: true });

  let visible = true;
  const io = new IntersectionObserver(([entry]) => {
    visible = entry?.isIntersecting ?? true;
    if (visible) start();
  });
  io.observe(canvas);
  const onVisibility = () => document.visibilityState === "visible" && start();
  document.addEventListener("visibilitychange", onVisibility);

  let raf = 0;
  let t0 = -1;
  const frame = (now: number) => {
    raf = 0;
    if (t0 < 0) t0 = now;
    const t = (now - t0) / 1000;
    // Threads weave in over the first 2.4 s, then hold.
    const grow = Math.min(1, t / 2.4);
    const eased = 1 - (1 - grow) ** 3;
    for (const th of threads) th.geo.setDrawRange(0, Math.floor((th.total * eased) / 3) * 3);
    loom.rotation.y +=
      (-0.42 + Math.sin(t * 0.25) * 0.16 + pointer.x * 0.12 - loom.rotation.y) * 0.05;
    loom.rotation.x += (0.28 + pointer.y * 0.06 - loom.rotation.x) * 0.05;
    rings.rotation.x = Math.sin(t * 0.6) * 0.04;
    renderer.render(scene, camera);
    if (visible && document.visibilityState === "visible") raf = requestAnimationFrame(frame);
  };
  function start() {
    if (!raf) raf = requestAnimationFrame(frame);
  }
  start();

  return {
    dispose() {
      cancelAnimationFrame(raf);
      ro.disconnect();
      io.disconnect();
      window.removeEventListener("pointermove", onPointer);
      document.removeEventListener("visibilitychange", onVisibility);
      scene.traverse((o) => {
        if (o instanceof Mesh || o instanceof InstancedMesh) o.geometry.dispose();
      });
      Object.values(materials).forEach((m) => m.dispose());
      renderer.dispose();
    },
  };
}
