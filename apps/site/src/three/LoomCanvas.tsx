import { useEffect, useRef, useState } from "react";

import { mountLoom } from "./loom";

/** The 3D Loom; fades in over the static poster once it has drawn. */
export default function LoomCanvas({
  onFail,
  onReady,
}: {
  onFail: () => void;
  onReady: () => void;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    let handle: ReturnType<typeof mountLoom> | null = null;
    try {
      handle = mountLoom(canvas);
      const id = requestAnimationFrame(() => {
        setReady(true);
        onReady();
      });
      return () => {
        cancelAnimationFrame(id);
        handle?.dispose();
      };
    } catch {
      onFail();
      return undefined;
    }
  }, [onFail, onReady]);
  return (
    <canvas
      ref={ref}
      aria-hidden="true"
      data-testid="loom-3d"
      className={`absolute inset-0 size-full transition-opacity duration-700 ${ready ? "opacity-100" : "opacity-0"}`}
    />
  );
}
