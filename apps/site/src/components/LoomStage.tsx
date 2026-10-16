import { useReducedMotion } from "motion/react";
import { lazy, Suspense, useCallback, useEffect, useState } from "react";

import { WeavePoster } from "./WeavePoster";

const LoomCanvas = lazy(() => import("../three/LoomCanvas"));

function webglAvailable() {
  try {
    return !!document.createElement("canvas").getContext("webgl2");
  } catch {
    return false;
  }
}

/**
 * The hero visual. The static weave paints first; the 3D chunk is fetched
 * only after the page is idle, only with WebGL2, and never under reduced
 * motion or data saver.
 */
export function LoomStage() {
  const reduced = useReducedMotion();
  const [want3d, setWant3d] = useState(false);
  const [drawn, setDrawn] = useState(false);
  const fail = useCallback(() => setWant3d(false), []);
  const ready = useCallback(() => setDrawn(true), []);
  useEffect(() => {
    const saveData = (navigator as Navigator & { connection?: { saveData?: boolean } }).connection
      ?.saveData;
    if (reduced || saveData || !webglAvailable()) return;
    const idle = window.requestIdleCallback ?? ((cb: () => void) => window.setTimeout(cb, 300));
    const cancel = window.cancelIdleCallback ?? window.clearTimeout;
    const id = idle(() => setWant3d(true));
    return () => cancel(id);
  }, [reduced]);
  return (
    <div className="relative aspect-[4/3] w-full sm:aspect-[5/2]" data-3d={want3d ? "on" : "off"}>
      <div
        className={`absolute inset-0 transition-opacity duration-700 ${drawn ? "opacity-0" : ""}`}
      >
        <WeavePoster />
      </div>
      {want3d && (
        <Suspense fallback={null}>
          <LoomCanvas onFail={fail} onReady={ready} />
        </Suspense>
      )}
    </div>
  );
}
