import { KINDS, SCHEMA_ID } from "@wyrdline/schemas";

const planes = ["intent", "effect", "state", "guard", "verdict", "meta"] as const;

export function OverviewPage() {
  const counts = planes.map((plane) => ({
    plane,
    kinds: Object.values(KINDS).filter((p) => p === plane).length,
  }));
  return (
    <div className="mx-auto max-w-6xl p-6 md:p-10">
      <p className="font-mono text-xs uppercase tracking-[0.2em] text-accent">Wyrdline Studio</p>
      <h1 className="mt-2 text-3xl font-semibold tracking-tight">Every agent leaves a thread.</h1>
      <p className="mt-2 max-w-2xl text-text-secondary">
        No sessions recorded yet. Start one with{" "}
        <code className="rounded-sm bg-surface-2 px-1.5 py-0.5 font-mono text-sm text-text-primary">
          wyrd run -- &lt;agent&gt;
        </code>
        .
      </p>
      <section aria-label="Schema planes" className="mt-8 grid grid-cols-2 gap-3 md:grid-cols-3">
        {counts.map(({ plane, kinds }) => (
          <div key={plane} className="rounded-card border border-border bg-surface-1 p-4 shadow-2">
            <p className="text-sm capitalize text-text-secondary">{plane} plane</p>
            <p className="mt-1 text-2xl font-semibold tabular-nums">{kinds}</p>
            <p className="text-xs text-text-muted">event kinds</p>
          </div>
        ))}
      </section>
      <p className="mt-6 font-mono text-xs text-text-muted">schema {SCHEMA_ID}</p>
    </div>
  );
}
