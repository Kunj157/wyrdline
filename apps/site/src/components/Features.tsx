import { Check } from "lucide-react";

import { features } from "../content";
import { Reveal, Tilt } from "./motion";

export function Features() {
  return (
    <section id="features" aria-labelledby="features-title" className="relative py-24">
      <div aria-hidden="true" className="wyrd-mesh wyrd-mesh-soft absolute inset-0 -z-10" />
      <div className="mx-auto max-w-6xl px-4 md:px-6">
        <Reveal>
          <p className="text-sm font-semibold tracking-widest text-accent uppercase">Shipped</p>
          <h2 id="features-title" className="mt-2 text-4xl font-semibold tracking-tight">
            Record, correlate, rewind.
          </h2>
        </Reveal>
        <div className="mt-14 flex flex-col gap-20">
          {features.map((f, i) => (
            <article
              key={f.id}
              id={f.id}
              aria-labelledby={`${f.id}-title`}
              className={`grid items-center gap-8 [&>*]:min-w-0 ${f.shot ? "lg:grid-cols-[1fr_1.35fr]" : "lg:grid-cols-2"}`}
            >
              <Reveal className={f.shot && i % 2 ? "lg:order-2" : undefined}>
                <p className="font-mono text-xs text-text-muted">{f.phase}</p>
                <h3
                  id={`${f.id}-title`}
                  className="mt-2 text-2xl font-semibold tracking-tight text-balance md:text-3xl"
                >
                  {f.title}
                </h3>
                <p className="mt-4 text-lg text-text-secondary">{f.body}</p>
                <ul className="mt-5 space-y-2">
                  {f.points.map((pt) => (
                    <li key={pt} className="flex gap-2 text-text-secondary">
                      <Check className="mt-1 size-4 shrink-0 text-verdict-ok" aria-hidden="true" />
                      {pt}
                    </li>
                  ))}
                </ul>
              </Reveal>
              {f.shot ? (
                <Reveal delay={0.08}>
                  <Tilt
                    max={3}
                    className="overflow-hidden rounded-lg border border-border-strong bg-surface-1 shadow-2"
                  >
                    <img
                      src={f.shot.src}
                      alt={f.shot.alt}
                      width={2880}
                      height={1800}
                      loading="lazy"
                      decoding="async"
                      className="block h-auto w-full"
                    />
                  </Tilt>
                </Reveal>
              ) : (
                <Reveal delay={0.08}>
                  <div className="wyrd-glow rounded-lg border border-border bg-surface-glass p-6 backdrop-blur">
                    <FeatureGlyph id={f.id} />
                  </div>
                </Reveal>
              )}
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}

/** Small terminal-style panels for the features without a Studio screen. */
function FeatureGlyph({ id }: { id: string }) {
  const titles: Record<string, string> = {
    sensor: "wyrd graph",
    ledger: "wyrd verify and a rejected delete",
    daemon: "wyrd explain",
  };
  // Real output from the demo recording; paths shortened as in the export.
  const lines: Record<string, string[]> = {
    sensor: [
      "$ wyrd graph <session>",
      "● fa-1-4 Bash rm old.txt && mkdir -p out && echo token=x > out/env.txt",
      "    59  direct  exec sh -c rm old.txt && mkdir -p out && …",
      "    61  direct  exec rm old.txt",
      "    62  direct  delete /home/dev/shop/old.txt",
      "    65  direct  exec mkdir -p out",
      "    66  direct  mkdir /home/dev/shop/out",
      "    68  direct  write /home/dev/shop/out/env.txt",
    ],
    ledger: [
      "$ wyrd verify <session>",
      "events    157",
      "head      b3:3158f5b4f840fcf0664165c52b859f0a…",
      "result    ✓ chain intact",
      "",
      "> DELETE FROM events WHERE seq = 68",
      "IntegrityError: events are append-only",
    ],
    daemon: [
      "$ wyrd explain <session> 68",
      "effect   68 — write /home/dev/shop/out/env.txt",
      "cause    direct (100 % confidence)",
      "call     fa-1-4 Bash rm old.txt && mkdir -p out && …",
      "process  sh(392804) ← wyrd-fakeagent(392776) ← ?(392772)",
    ],
  };
  return (
    <pre
      tabIndex={0}
      aria-label={`Example output: ${titles[id] ?? id}`}
      className="overflow-x-auto font-mono text-[13px] leading-6 text-text-primary"
    >
      {(lines[id] ?? []).join("\n")}
    </pre>
  );
}
