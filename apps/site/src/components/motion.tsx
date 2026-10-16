/**
 * Motion primitives in the spirit of Watermelon UI and Motion Primitives,
 * restyled to the shared tokens. Every one has a static path: with
 * prefers-reduced-motion they render their children unchanged.
 */
import { motion, useMotionValue, useReducedMotion, useSpring, useTransform } from "motion/react";
import type { PointerEvent, ReactNode } from "react";

/** Fade and rise into place the first time it scrolls into view. */
export function Reveal({
  children,
  delay = 0,
  className,
}: {
  children: ReactNode;
  delay?: number;
  className?: string;
}) {
  const reduced = useReducedMotion();
  if (reduced) return <div className={className}>{children}</div>;
  return (
    <motion.div
      className={className}
      initial={{ opacity: 0, y: 24 }}
      whileInView={{ opacity: 1, y: 0 }}
      viewport={{ once: true, margin: "-60px" }}
      transition={{ duration: 0.6, delay, ease: [0.16, 1, 0.3, 1] }}
    >
      {children}
    </motion.div>
  );
}

/** A card that tilts toward the pointer (at most `max` degrees) with a moving light. */
export function Tilt({
  children,
  max = 6,
  className,
}: {
  children: ReactNode;
  max?: number;
  className?: string;
}) {
  const reduced = useReducedMotion();
  const px = useMotionValue(0.5);
  const py = useMotionValue(0.5);
  const rx = useSpring(useTransform(py, [0, 1], [max, -max]), { stiffness: 200, damping: 20 });
  const ry = useSpring(useTransform(px, [0, 1], [-max, max]), { stiffness: 200, damping: 20 });
  const light = useTransform(
    [px, py],
    ([x, y]) =>
      `radial-gradient(420px circle at ${(x as number) * 100}% ${(y as number) * 100}%, rgba(255,255,255,0.55), transparent 60%)`,
  );
  if (reduced) return <div className={className}>{children}</div>;
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    px.set((e.clientX - r.left) / r.width);
    py.set((e.clientY - r.top) / r.height);
  };
  const leave = () => {
    px.set(0.5);
    py.set(0.5);
  };
  return (
    <div className="[perspective:1000px]">
      <motion.div
        className={`relative ${className ?? ""}`}
        style={{ rotateX: rx, rotateY: ry, transformStyle: "preserve-3d" }}
        onPointerMove={move}
        onPointerLeave={leave}
      >
        {children}
        <motion.div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 rounded-[inherit] opacity-0 transition-opacity duration-300 [div:hover>&]:opacity-100"
          style={{ background: light }}
        />
      </motion.div>
    </div>
  );
}
