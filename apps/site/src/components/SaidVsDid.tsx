import { Badge } from "@wyrdline/ui";

import { planes } from "../content";
import { Reveal, Tilt } from "./motion";

export function SaidVsDid() {
  return (
    <section id="how" aria-labelledby="how-title" className="mx-auto max-w-6xl px-4 py-24 md:px-6">
      <Reveal>
        <p className="text-sm font-semibold tracking-widest text-accent uppercase">The gap</p>
        <h2
          id="how-title"
          className="mt-2 max-w-3xl text-4xl font-semibold tracking-tight text-balance"
        >
          What an agent says it did and what it actually did can differ.
        </h2>
        <p className="mt-4 max-w-2xl text-lg text-text-secondary">
          From the demo session: one Bash tool call, the agent's summary, and the kernel's record of
          the same moment.
        </p>
      </Reveal>
      <div className="mt-10 grid gap-5 md:grid-cols-2 [&>*]:min-w-0">
        <Reveal>
          <Tilt
            max={4}
            className="h-full rounded-lg border border-border bg-surface-1 p-6 shadow-2"
          >
            <Badge meaning="intent">The agent said</Badge>
            <blockquote className="mt-4 text-2xl font-medium text-balance">
              “Fixed the greeting, removed old.txt, and the check passes.”
            </blockquote>
            <p className="mt-4 font-mono text-sm text-text-muted">assistant · turn 1</p>
          </Tilt>
        </Reveal>
        <Reveal delay={0.08}>
          <Tilt
            max={4}
            className="h-full rounded-lg border border-border bg-surface-1 p-6 shadow-2"
          >
            <Badge meaning="effect">The kernel saw</Badge>
            <ol className="mt-4 space-y-2 font-mono text-sm break-words">
              <li>exec sh -c "rm old.txt && mkdir -p out && echo token=x &gt; out/env.txt"</li>
              <li>exec rm old.txt</li>
              <li>unlink /home/dev/shop/old.txt</li>
              <li>exec mkdir -p out</li>
              <li>mkdir /home/dev/shop/out</li>
              <li className="rounded-sm bg-vivid-vermilion/12 px-2 py-1 text-verdict-divergence">
                open_write /home/dev/shop/out/env.txt
              </li>
            </ol>
            <p className="mt-4 text-sm text-text-secondary">
              The summary never mentions <code className="font-mono">out/env.txt</code>, a new file
              holding the token from the prompt. Each line is attributed to the tool call that
              caused it, and the Cell can rewind all of it.
            </p>
          </Tilt>
        </Reveal>
      </div>
      <div className="mt-16 grid gap-5 md:grid-cols-3">
        {planes.map((p, i) => (
          <Reveal key={p.name} delay={i * 0.06}>
            <Tilt className="h-full rounded-lg border border-border bg-surface-1 p-6 shadow-1">
              <Badge meaning={p.meaning}>{`${p.name} plane`}</Badge>
              <p className="mt-4 text-text-secondary">{p.source}</p>
              <p className="mt-4 rounded-sm bg-surface-2 px-3 py-2 font-mono text-xs text-text-primary">
                {p.example}
              </p>
            </Tilt>
          </Reveal>
        ))}
      </div>
    </section>
  );
}
