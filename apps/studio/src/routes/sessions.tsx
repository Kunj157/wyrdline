import { Link } from "@tanstack/react-router";
import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  getSortedRowModel,
  type SortingState,
  useReactTable,
} from "@tanstack/react-table";
import type { api } from "@wyrdline/schemas";
import { Badge, type Meaning } from "@wyrdline/ui";
import { useMemo, useState } from "react";

import { useSessions } from "../lib/api";

type Row = api.SessionRecord;

export const statusMeaning: Record<string, Meaning> = {
  recording: "intent",
  closed: "ok",
  degraded: "divergence",
  failed: "denied",
};

const columns: ColumnDef<Row>[] = [
  {
    header: "Session",
    accessorFn: (r) => r.name ?? r.id,
    cell: ({ row }) => (
      <Link
        to="/sessions/$sid"
        params={{ sid: row.original.id }}
        className="font-medium text-accent hover:underline"
      >
        {row.original.name ?? row.original.id.slice(0, 10)}
      </Link>
    ),
  },
  { header: "Agent", accessorKey: "agent" },
  {
    header: "Workspace",
    accessorKey: "workspace",
    cell: ({ getValue }) => <span className="font-mono text-xs">{String(getValue())}</span>,
  },
  {
    header: "Started",
    accessorKey: "created_ms",
    cell: ({ getValue }) => new Date(Number(getValue())).toLocaleString(),
  },
  {
    header: "Status",
    accessorKey: "status",
    cell: ({ getValue }) => {
      const s = String(getValue());
      return <Badge meaning={statusMeaning[s] ?? "unknown"}>{s}</Badge>;
    },
  },
  { header: "Checkpoints", accessorFn: (r) => r.checkpoints.length },
  { header: "Branches", accessorFn: (r) => r.branches.length },
];

export function SessionsPage() {
  const sessions = useSessions();
  const [sorting, setSorting] = useState<SortingState>([{ id: "created_ms", desc: true }]);
  const data = useMemo(() => sessions.data?.sessions ?? [], [sessions.data]);
  // TanStack Table returns fresh functions each render; the React Compiler
  // skips this component, which is what we want.
  // eslint-disable-next-line react-hooks/incompatible-library
  const table = useReactTable({
    data,
    columns,
    state: { sorting },
    onSortingChange: setSorting,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
  });
  return (
    <div className="mx-auto max-w-7xl p-4 md:p-8">
      <h1 className="text-2xl font-semibold tracking-tight">Sessions</h1>
      {sessions.isPending && <p className="mt-4 text-text-muted">Loading sessions…</p>}
      {sessions.error && (
        <p role="alert" className="mt-4 text-verdict-denied">
          {sessions.error.message}
        </p>
      )}
      {sessions.data && data.length === 0 && (
        <p className="mt-4 text-text-secondary">
          No sessions yet. Start one with{" "}
          <code className="font-mono">wyrd run -- &lt;agent&gt;</code>.
        </p>
      )}
      {data.length > 0 && (
        <div className="mt-4 overflow-x-auto rounded-card border border-border bg-surface-1 shadow-2">
          <table className="w-full border-collapse text-sm">
            <thead className="bg-surface-2 text-left text-xs text-text-secondary">
              {table.getHeaderGroups().map((hg) => (
                <tr key={hg.id}>
                  {hg.headers.map((h) => {
                    const sorted = h.column.getIsSorted();
                    return (
                      <th
                        key={h.id}
                        scope="col"
                        aria-sort={
                          sorted === "asc" ? "ascending" : sorted === "desc" ? "descending" : "none"
                        }
                        className="px-3 py-2 font-medium"
                      >
                        <button
                          type="button"
                          onClick={h.column.getToggleSortingHandler()}
                          className="hover:text-text-primary"
                        >
                          {flexRender(h.column.columnDef.header, h.getContext())}
                          {sorted === "asc" ? " ↑" : sorted === "desc" ? " ↓" : ""}
                        </button>
                      </th>
                    );
                  })}
                </tr>
              ))}
            </thead>
            <tbody>
              {table.getRowModel().rows.map((r) => (
                <tr key={r.id} className="border-t border-border hover:bg-surface-2">
                  {r.getVisibleCells().map((c) => (
                    <td key={c.id} className="px-3 py-2">
                      {flexRender(c.column.columnDef.cell, c.getContext())}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
