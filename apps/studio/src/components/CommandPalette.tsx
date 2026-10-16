import { useNavigate } from "@tanstack/react-router";
import { Kbd } from "@wyrdline/ui";
import { Search } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";

import { useSessions } from "../lib/api";
import { type Command, filterCommands, seqQuery } from "../lib/palette";

/**
 * ⌘K / Ctrl+K anywhere: jump to a session, a checkpoint, or `#seq` in the
 * open session. A WAI-ARIA combobox with a listbox; Escape closes.
 */
export function CommandPalette({ onSeq }: { onSeq?: (seq: number) => void }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const dialog = useRef<HTMLDialogElement>(null);
  const listId = useId();
  const sessions = useSessions();
  const navigate = useNavigate();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    const el = dialog.current;
    if (!el) return;
    if (open && !el.open) el.showModal();
    if (!open && el.open) el.close();
  }, [open]);

  const commands = useMemo<Command[]>(() => {
    const out: Command[] = [];
    for (const s of sessions.data?.sessions ?? []) {
      const name = s.name ?? s.id.slice(0, 10);
      out.push({
        id: `s-${s.id}`,
        label: name,
        hint: `session · ${s.agent} · ${s.status}`,
        to: `/sessions/${s.id}`,
      });
      for (const c of s.checkpoints) {
        out.push({
          id: `c-${s.id}-${c.id}`,
          label: `checkpoint ${c.id}`,
          hint: `${name} · branch ${c.branch}`,
          to: `/sessions/${s.id}`,
          seq: c.seq,
        });
      }
    }
    return out;
  }, [sessions.data]);

  const seq = seqQuery(query);
  const results =
    seq != null && onSeq
      ? [{ id: "seq", label: `Go to event #${seq}`, hint: "in this session", to: "", seq }]
      : filterCommands(commands, query);
  const close = () => {
    setOpen(false);
    setQuery("");
    setActive(0);
  };
  const run = (c: Command | undefined) => {
    if (!c) return;
    close();
    if (c.to) void navigate({ to: c.to });
    if (c.seq != null && onSeq) onSeq(c.seq);
  };

  return (
    <dialog
      ref={dialog}
      aria-label="Command palette"
      onClose={close}
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      className="mx-auto mt-[12vh] w-[min(36rem,calc(100vw-2rem))] rounded-lg border border-border bg-surface-1 p-0 text-text-primary shadow-2 backdrop:bg-[rgba(28,24,48,0.32)]"
    >
      {open && (
        <div className="flex flex-col">
          <div className="flex items-center gap-2 border-b border-border px-3">
            <Search className="size-4 text-text-muted" aria-hidden="true" />
            <input
              autoFocus
              role="combobox"
              aria-expanded="true"
              aria-controls={listId}
              aria-activedescendant={
                results[active] ? `${listId}-${results[active].id}` : undefined
              }
              aria-label="Jump to a session, checkpoint, or #seq"
              placeholder="Jump to a session, checkpoint, or #seq"
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setActive(0);
              }}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setActive((a) => Math.min(results.length - 1, a + 1));
                } else if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setActive((a) => Math.max(0, a - 1));
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  run(results[active]);
                }
              }}
              className="h-11 flex-1 bg-transparent text-sm outline-none"
            />
            <Kbd>Esc</Kbd>
          </div>
          <ul
            id={listId}
            role="listbox"
            aria-label="Results"
            className="max-h-80 overflow-auto p-1"
          >
            {results.length === 0 && (
              <li className="px-3 py-2 text-sm text-text-muted">No matches.</li>
            )}
            {results.map((c, i) => (
              <li
                key={c.id}
                id={`${listId}-${c.id}`}
                role="option"
                aria-selected={i === active}
                onMouseEnter={() => setActive(i)}
                onClick={() => run(c)}
                className={`flex cursor-pointer items-baseline justify-between gap-3 rounded-sm px-3 py-2 text-sm ${i === active ? "bg-accent-subtle" : ""}`}
              >
                <span className="truncate">{c.label}</span>
                <span className="shrink-0 text-xs text-text-muted">{c.hint}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </dialog>
  );
}
