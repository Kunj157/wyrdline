import { ArrowUpRight } from "lucide-react";
import { motion, useReducedMotion, useScroll, useTransform } from "motion/react";
import { useRef } from "react";

import { demoUrl, shot } from "../content";
import { Reveal } from "./motion";

export function Demo() {
  const ref = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const { scrollYProgress } = useScroll({ target: ref, offset: ["start end", "center center"] });
  const rotateX = useTransform(scrollYProgress, [0, 1], [24, 0]);
  const scale = useTransform(scrollYProgress, [0, 1], [0.9, 1]);
  return (
    <section
      id="demo"
      aria-labelledby="demo-title"
      className="mx-auto max-w-6xl overflow-x-clip px-4 py-24 md:px-6"
    >
      <Reveal className="text-center">
        <p className="text-sm font-semibold tracking-widest text-accent uppercase">Try it</p>
        <h2 id="demo-title" className="mt-2 text-4xl font-semibold tracking-tight">
          The real Studio, in your browser.
        </h2>
        <p className="mx-auto mt-4 max-w-2xl text-lg text-text-secondary">
          A recorded session replays with no server: the events load from a file and Wyrdline's
          correlator, compiled to WebAssembly, rebuilds the causal graph on your machine.
        </p>
      </Reveal>
      <div ref={ref} className="mt-12 [perspective:1400px]">
        <motion.a
          href={demoUrl}
          aria-label="Open the live demo: the session in Studio"
          style={reduced ? undefined : { rotateX, scale, transformOrigin: "center top" }}
          className="group relative block overflow-hidden rounded-lg border border-border-strong bg-surface-1 shadow-2"
        >
          <img
            src={shot("river.jpg")}
            alt="Studio showing the demo session: the Causal River with intent knots above, kernel effects below and checkpoints on the spine, and the events table"
            width={2880}
            height={1800}
            loading="lazy"
            decoding="async"
            className="block h-auto w-full"
          />
          <span className="absolute inset-x-0 bottom-6 mx-auto flex w-max items-center gap-2 rounded-full bg-accent px-5 py-2.5 font-medium text-on-accent shadow-2 transition-transform group-hover:scale-105">
            Open the live demo
            <ArrowUpRight className="size-4" aria-hidden="true" />
          </span>
        </motion.a>
      </div>
    </section>
  );
}
