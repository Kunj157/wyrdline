import { roadmap } from "../content";
import { Reveal, Tilt } from "./motion";

export function Roadmap() {
  return (
    <section
      id="roadmap"
      aria-labelledby="roadmap-title"
      className="mx-auto max-w-6xl px-4 py-24 md:px-6"
    >
      <Reveal>
        <p className="text-sm font-semibold tracking-widest text-accent uppercase">Next</p>
        <h2 id="roadmap-title" className="mt-2 text-4xl font-semibold tracking-tight">
          Where the thread goes next.
        </h2>
        <p className="mt-4 max-w-2xl text-lg text-text-secondary">
          Phases 0 to 8 are released. Each phase below gets its own section here when it ships.
        </p>
      </Reveal>
      <ol className="mt-10 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {roadmap.map((p, i) => (
          <li key={p.n}>
            <Reveal delay={i * 0.04} className="h-full">
              <Tilt className="h-full rounded-lg border border-border bg-surface-1 p-5 shadow-1">
                <p className="font-mono text-xs text-text-muted">Phase {p.n}</p>
                <h3 className="mt-1 text-lg font-semibold">{p.title}</h3>
                <p className="mt-2 text-text-secondary">{p.body}</p>
              </Tilt>
            </Reveal>
          </li>
        ))}
      </ol>
    </section>
  );
}

export function GetStarted() {
  return (
    <section
      id="start"
      aria-labelledby="start-title"
      className="mx-auto max-w-6xl px-4 pb-24 md:px-6"
    >
      <Reveal>
        <div className="wyrd-mesh relative overflow-hidden rounded-lg border border-border-strong p-8 shadow-2 md:p-12">
          <h2 id="start-title" className="text-3xl font-semibold tracking-tight">
            Build it from source
          </h2>
          <p className="mt-3 max-w-2xl text-text-secondary">
            Linux with BTF and cgroup v2 (kernel 5.15 or newer). Signed packages arrive with the
            launch; until then, build it yourself. <code className="font-mono">wyrd doctor</code>{" "}
            reports what your kernel supports and what degrades without it.
          </p>
          <pre
            tabIndex={0}
            aria-label="Build commands"
            className="mt-6 overflow-x-auto rounded-card bg-text-primary p-5 font-mono text-sm leading-7 text-surface-1"
          >
            {[
              "git clone https://github.com/Kunj157/wyrdline && cd wyrdline",
              "make build                    # crates, eBPF object, Studio",
              "./target/release/wyrd doctor  # what this kernel can capture",
              "./target/release/wyrd run -- <your agent command>",
              "./target/release/wyrd studio  # open the session in Studio",
            ].join("\n")}
          </pre>
        </div>
      </Reveal>
    </section>
  );
}
