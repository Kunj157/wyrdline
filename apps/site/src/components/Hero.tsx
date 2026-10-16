import { ArrowRight } from "lucide-react";

import { demoUrl, repo, stack } from "../content";
import { GithubMark } from "./GithubMark";
import { LoomStage } from "./LoomStage";
import { Reveal } from "./motion";

export function Hero() {
  return (
    <section id="top" aria-labelledby="hero-title" className="relative overflow-hidden">
      <div aria-hidden="true" className="wyrd-mesh absolute inset-0 -z-10" />
      <div className="mx-auto max-w-6xl px-4 pt-16 pb-6 text-center md:px-6 md:pt-24">
        <Reveal>
          <p className="mx-auto inline-flex items-center gap-2 rounded-full border border-border bg-surface-glass px-3 py-1 text-sm text-text-secondary backdrop-blur">
            <span className="size-2 rounded-full bg-vivid-verdigris" aria-hidden="true" />A causal
            flight recorder for AI coding agents
          </p>
        </Reveal>
        <Reveal delay={0.05}>
          <h1
            id="hero-title"
            className="mx-auto mt-6 max-w-4xl text-5xl font-semibold tracking-tight text-balance md:text-7xl"
          >
            Every agent leaves a{" "}
            <span className="wyrd-ink bg-clip-text text-transparent">thread</span>.
          </h1>
        </Reveal>
        <Reveal delay={0.1}>
          <p className="mx-auto mt-6 max-w-2xl text-lg text-pretty text-text-secondary md:text-xl">
            Wyrdline records what a coding agent said, what the kernel saw it do, and the workspace
            after every step. Then it lets you follow the thread, cut it, and re-weave it.
          </p>
        </Reveal>
        <Reveal delay={0.15} className="mt-8 flex flex-wrap items-center justify-center gap-3">
          <a
            href={demoUrl}
            className="inline-flex h-11 items-center gap-2 rounded-card bg-accent px-5 font-medium text-on-accent shadow-2 hover:bg-accent-hover"
          >
            Open the live demo
            <ArrowRight className="size-4" aria-hidden="true" />
          </a>
          <a
            href={repo}
            className="inline-flex h-11 items-center gap-2 rounded-card border border-border-strong bg-surface-1 px-5 font-medium hover:bg-surface-2"
          >
            <GithubMark className="size-4" />
            Star on GitHub
          </a>
        </Reveal>
      </div>
      <div className="mx-auto max-w-6xl px-2 md:px-6">
        <LoomStage />
      </div>
      <Stack />
    </section>
  );
}

function Stack() {
  const items = [...stack, ...stack];
  return (
    <div className="border-y border-border bg-surface-glass py-3" aria-label="Built with">
      <div className="wyrd-marquee-mask overflow-hidden">
        <ul className="wyrd-marquee flex w-max gap-8 text-sm font-medium text-text-muted">
          {items.map((s, i) => (
            <li
              key={`${s}-${i}`}
              aria-hidden={i >= stack.length || undefined}
              className="whitespace-nowrap"
            >
              {s}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
