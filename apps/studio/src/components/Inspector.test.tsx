import { fireEvent, render, screen } from "@testing-library/react";
import type { Event } from "@wyrdline/schemas";
import type * as api from "@wyrdline/schemas/generated-api";
import { describe, expect, it } from "vitest";

import { Inspector } from "./Inspector";

const write: Event = {
  seq: 61,
  ts_mono_ns: 61_000,
  ts_wall_ms: 61,
  process: null,
  drops_since_last: 0,
  body: {
    kind: "effect.file.write",
    payload: { file: { path: "/home/user/.bashrc", in_cell: false, truncated: false }, bytes: 10 },
  },
};

const view = (over: Partial<api.DivergenceView> = {}): api.DivergenceView => ({
  seq: 66,
  divergence_id: "dv-1",
  rule_id: "D002",
  rule_name: "out-of-scope-write",
  severity: "critical",
  claim_id: null,
  claim: null,
  evidence_seqs: [61],
  confidence_bp: 9500,
  summary: "wrote /home/user/.bashrc outside the workspace",
  status: "open",
  note: null,
  ...over,
});

describe("Inspector", () => {
  it("lists the divergences citing the selected event", () => {
    const other = view({ divergence_id: "dv-2", rule_id: "D001", evidence_seqs: [70] });
    render(
      <Inspector event={write} graph={undefined} events={[write]} divergences={[view(), other]} />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Divergences (1)" }));
    expect(screen.getByText("out-of-scope-write")).toBeTruthy();
    expect(screen.getByText("wrote /home/user/.bashrc outside the workspace")).toBeTruthy();
    expect(screen.queryByText("D001")).toBeNull();
  });

  it("says when no divergence cites the event", () => {
    render(<Inspector event={write} graph={undefined} events={[write]} />);
    fireEvent.click(screen.getByRole("tab", { name: "Divergences" }));
    expect(screen.getByText("No divergence cites this event.")).toBeTruthy();
  });
});
