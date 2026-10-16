/** Command palette entries and matching, kept pure for tests. */
export interface Command {
  id: string;
  label: string;
  hint: string;
  /** Where to go: a path in Studio. */
  to: string;
  /** For a seq jump within the current session. */
  seq?: number;
}

/** Commands that match `query`: every word must appear (case-insensitive), best first. */
export function filterCommands(commands: Command[], query: string): Command[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return commands.slice(0, 50);
  const scored: Array<[number, Command]> = [];
  for (const c of commands) {
    const hay = `${c.label} ${c.hint}`.toLowerCase();
    if (!words.every((w) => hay.includes(w))) continue;
    // Words found in the label beat words found only in the hint.
    const label = c.label.toLowerCase();
    const missing = words.filter((w) => !label.includes(w)).length;
    const score = missing + (label.startsWith(words[0] ?? "") ? 0 : 0.5) + c.label.length / 1000;
    scored.push([score, c]);
  }
  return scored
    .sort((a, b) => a[0] - b[0])
    .map(([, c]) => c)
    .slice(0, 50);
}

/** `#123` or `seq 123` in the query: a jump to that event. */
export function seqQuery(query: string): number | null {
  const m = /^\s*(?:#|seq\s+)(\d{1,9})\s*$/i.exec(query);
  return m?.[1] ? Number(m[1]) : null;
}
