/**
 * Live mode: follow `/v1/sessions/{sid}/stream` and append new events to
 * the cached event list. Uses fetch rather than EventSource so the bearer
 * token travels in a header, never in a URL; reconnects resume with
 * Last-Event-ID.
 */
import { useQueryClient } from "@tanstack/react-query";
import type { Event } from "@wyrdline/schemas";
import { useEffect, useState } from "react";

import { apiClient, apiToken, keys } from "./api";

export interface SseMessage {
  event: string;
  id: string | null;
  data: string;
}

/** Split complete SSE messages off `buffer`; returns them and the rest. */
export function parseSse(buffer: string): { messages: SseMessage[]; rest: string } {
  const messages: SseMessage[] = [];
  const blocks = buffer.split("\n\n");
  const rest = blocks.pop() ?? "";
  for (const block of blocks) {
    let event = "message";
    let id: string | null = null;
    const data: string[] = [];
    for (const line of block.split("\n")) {
      if (line.startsWith(":")) continue;
      const [field, ...v] = line.split(":");
      const value = v.join(":").replace(/^ /, "");
      if (field === "event") event = value;
      else if (field === "id") id = value;
      else if (field === "data") data.push(value);
    }
    if (data.length > 0 || id !== null) messages.push({ event, id, data: data.join("\n") });
  }
  return { messages, rest };
}

export type LiveState = "off" | "live" | "ended" | "reconnecting";

export function useLiveEvents(sid: string, enabled: boolean): LiveState {
  const qc = useQueryClient();
  const [state, setState] = useState<LiveState>("off");
  useEffect(() => {
    if (!enabled) return;
    const abort = new AbortController();
    let lastSeq = (qc.getQueryData<Event[]>(keys.events(sid)) ?? []).at(-1)?.seq ?? 0;
    let delay = 500;
    const run = async () => {
      while (!abort.signal.aborted) {
        try {
          const headers: Record<string, string> = {
            accept: "text/event-stream",
            "last-event-id": String(lastSeq),
          };
          const t = apiToken();
          if (t) headers.authorization = `Bearer ${t}`;
          const res = await fetch(apiClient().streamUrl(sid), { headers, signal: abort.signal });
          if (!res.ok || !res.body) throw new Error(`stream: HTTP ${res.status}`);
          setState("live");
          delay = 500;
          const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
          let buffer = "";
          for (;;) {
            const { value, done } = await reader.read();
            if (done) break;
            const { messages, rest } = parseSse(buffer + value);
            buffer = rest;
            const fresh: Event[] = [];
            for (const m of messages) {
              if (m.event === "ledger") {
                const e = JSON.parse(m.data) as Event;
                if (e.seq > lastSeq) {
                  fresh.push(e);
                  lastSeq = e.seq;
                }
              } else if (m.event === "end") {
                setState("ended");
                void qc.invalidateQueries({ queryKey: keys.session(sid) });
                return;
              }
            }
            if (fresh.length > 0) {
              qc.setQueryData<Event[]>(keys.events(sid), (old) => [...(old ?? []), ...fresh]);
              void qc.invalidateQueries({ queryKey: keys.graph(sid) });
            }
          }
        } catch {
          if (abort.signal.aborted) return;
        }
        setState("reconnecting");
        await new Promise((r) => setTimeout(r, delay));
        delay = Math.min(delay * 2, 10_000);
      }
    };
    void run();
    return () => abort.abort();
  }, [sid, enabled, qc]);
  return enabled ? state : "off";
}
