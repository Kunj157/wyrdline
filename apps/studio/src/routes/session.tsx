import { useParams } from "@tanstack/react-router";
import { Badge, Card, Kbd, Shimmer, Tabs } from "@wyrdline/ui";
import { ShieldCheck, ShieldX, ZoomIn, ZoomOut } from "lucide-react";
import { useMemo, useState } from "react";

import { CheckpointPanel } from "../components/CheckpointPanel";
import { EventTable } from "../components/EventTable";
import { Inspector } from "../components/Inspector";
import { River2D } from "../components/River2D";
import { type Renderer, RiverCanvas } from "../river/RiverCanvas";
import { RIVER_HEIGHT } from "../river/theme";
import { Transcript } from "../components/Transcript";
import {
  offlineSource,
  useCheckpoints,
  useEvents,
  useGraph,
  useSession,
  useVerify,
} from "../lib/api";
import { useLiveEvents } from "../lib/live";
import { layoutRiver } from "../lib/model";
import { statusMeaning } from "./sessions";

/** The river plus its toolbar row (27px) and gap (8px). */
const RIVER_BLOCK = RIVER_HEIGHT + 35;

/** Above this many knots the SVG fallback is too slow; the table remains. */
const SVG_LIMIT = 5000;

export function SessionPage() {
  const { sid } = useParams({ from: "/sessions/$sid" });
  const session = useSession(sid);
  const events = useEvents(sid);
  const graph = useGraph(sid);
  const verify = useVerify(sid);
  const checkpoints = useCheckpoints(sid);
  const recording = session.data?.status === "recording" || session.data?.status === "degraded";
  const live = useLiveEvents(sid, Boolean(recording && events.data));
  const [selected, setSelected] = useState<number | null>(null);
  const [zoom, setZoom] = useState(1);
  const [mode, setMode] = useState<"auto" | "canvas2d" | "svg">("auto");
  const [renderer, setRenderer] = useState<Renderer | null>(null);
  const [tab, setTab] = useState("events");
  const list = useMemo(() => events.data ?? [], [events.data]);
  const layout = useMemo(() => layoutRiver(list, graph.data), [list, graph.data]);
  const drops = useMemo(() => list.reduce((n, e) => n + e.drops_since_last, 0), [list]);
  const selectedEvent = list.find((e) => e.seq === selected);

  if (session.error) {
    return (
      <p role="alert" className="p-8 text-verdict-denied">
        {session.error.message}
      </p>
    );
  }
  const s = session.data;
  return (
    <div className="mx-auto flex max-w-[100rem] flex-col gap-4 p-4 md:p-6">
      <header className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <h1 className="text-xl font-semibold tracking-tight">{s?.name ?? sid.slice(0, 10)}</h1>
        {s && <Badge meaning={statusMeaning[s.status] ?? "unknown"}>{s.status}</Badge>}
        {s && <span className="text-sm text-text-secondary">{s.agent}</span>}
        {s && (
          <span
            title={s.workspace}
            className="max-w-[28rem] truncate font-mono text-xs text-text-muted"
          >
            {s.workspace}
          </span>
        )}
        {offlineSource() !== null && (
          <span className="text-sm text-text-muted">replay · chain not verifiable offline</span>
        )}
        {verify.data &&
          (verify.data.ok ? (
            <span className="flex items-center gap-1 text-sm text-verdict-ok">
              <ShieldCheck className="size-4" aria-hidden="true" />
              chain intact{verify.data.sealed ? ", signed" : ""}
            </span>
          ) : (
            <span className="flex items-center gap-1 text-sm text-verdict-denied">
              <ShieldX className="size-4" aria-hidden="true" />
              chain broken
            </span>
          ))}
        <span
          className={`text-sm ${drops > 0 ? "text-verdict-divergence" : "text-text-secondary"}`}
        >
          drops: {drops}
        </span>
        {live === "live" && <Shimmer>Recording live…</Shimmer>}
        {live === "reconnecting" && <span className="text-sm text-text-muted">Reconnecting…</span>}
      </header>

      {events.isPending ? (
        // Reserve the river's height so the panels below do not jump when it
        // arrives.
        <div style={{ minHeight: RIVER_BLOCK }}>
          <Shimmer>Loading the river…</Shimmer>
        </div>
      ) : (
        <section
          aria-label="Causal river"
          className="flex flex-col gap-2"
          style={{ minHeight: RIVER_BLOCK }}
        >
          <div className="flex flex-wrap items-center gap-2 text-xs text-text-muted">
            <label className="flex items-center gap-1">
              Renderer
              <select
                value={mode}
                onChange={(e) => setMode(e.target.value as typeof mode)}
                className="rounded-sm border border-border-strong bg-surface-1 px-1 py-0.5"
              >
                <option value="auto">Auto (WebGPU)</option>
                <option value="canvas2d">Canvas2D</option>
                <option value="svg">SVG</option>
              </select>
            </label>
            {mode !== "svg" && renderer && renderer !== "unavailable" && (
              <span data-testid="renderer">{renderer === "webgpu" ? "WebGPU" : "Canvas2D"}</span>
            )}
            {mode === "svg" && (
              <>
                <button
                  type="button"
                  aria-label="Zoom out"
                  onClick={() => setZoom((z) => Math.max(0.25, z / 1.5))}
                  className="rounded-sm p-1 hover:bg-surface-2"
                >
                  <ZoomOut className="size-4" aria-hidden="true" />
                </button>
                <button
                  type="button"
                  aria-label="Zoom in"
                  onClick={() => setZoom((z) => Math.min(6, z * 1.5))}
                  className="rounded-sm p-1 hover:bg-surface-2"
                >
                  <ZoomIn className="size-4" aria-hidden="true" />
                </button>
              </>
            )}
            <span>
              <Kbd>←</Kbd> <Kbd>→</Kbd> move · <Kbd>n</Kbd> <Kbd>p</Kbd> notable · <Kbd>[</Kbd>{" "}
              <Kbd>]</Kbd> zoom · {layout.knots.length} knots
            </span>
          </div>
          {mode !== "svg" && renderer !== "unavailable" ? (
            <RiverCanvas
              key={mode}
              layout={layout}
              selected={selected}
              onSelect={setSelected}
              prefer={mode === "auto" ? "webgpu" : "canvas2d"}
              onRenderer={setRenderer}
            />
          ) : layout.knots.length <= SVG_LIMIT ? (
            <River2D layout={layout} selected={selected} onSelect={setSelected} zoom={zoom} />
          ) : (
            <p role="status" className="rounded-card border border-border bg-surface-1 p-4 text-sm">
              No graphics renderer is available for {layout.knots.length} events; the events table
              below has them all.
            </p>
          )}
        </section>
      )}

      <div className="grid gap-4 lg:grid-cols-[minmax(20rem,1fr)_2fr]">
        <Card>
          <Inspector event={selectedEvent} graph={graph.data} events={list} />
        </Card>
        <Card>
          <Tabs
            label="Session views"
            value={tab}
            onChange={setTab}
            tabs={[
              {
                id: "events",
                label: "Events",
                panel: <EventTable events={list} selected={selected} onSelect={setSelected} />,
              },
              {
                id: "transcript",
                label: "Transcript",
                panel: <Transcript events={list} onSelect={setSelected} />,
              },
            ]}
          />
        </Card>
      </div>

      <Card>
        <h2 className="mb-2 text-sm font-semibold">Checkpoints &amp; branches</h2>
        <CheckpointPanel sid={sid} checkpoints={checkpoints.data} events={list} />
      </Card>
    </div>
  );
}
