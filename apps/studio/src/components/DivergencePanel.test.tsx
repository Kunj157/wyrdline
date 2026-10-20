import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createClient } from "@wyrdline/schemas/client";
import type * as api from "@wyrdline/schemas/generated-api";

import { setApiClient } from "../lib/api";
import { DivergencePanel } from "./DivergencePanel";

const view = (over: Partial<api.DivergenceView> = {}): api.DivergenceView => ({
  seq: 66,
  divergence_id: "dv-1",
  rule_id: "D002",
  rule_name: "out-of-scope-write",
  severity: "critical",
  claim_id: null,
  claim: null,
  evidence_seqs: [61, 62],
  confidence_bp: 9500,
  summary: "wrote /home/user/.bashrc outside the workspace",
  status: "open",
  note: null,
  ...over,
});

/** A daemon with one divergence; PATCH answers `patch` (status, body). */
function daemon(patch: [number, unknown]) {
  let current = view();
  const patches: unknown[] = [];
  const fetch = (async (_url: string, init: RequestInit) => {
    let status = 200;
    let body: unknown = { divergences: [current] };
    if (init.method === "PATCH") {
      patches.push(JSON.parse(String(init.body)));
      [status, body] = patch;
      if (status === 200) current = body as api.DivergenceView;
    }
    return new Response(JSON.stringify(body), { status });
  }) as unknown as typeof globalThis.fetch;
  setApiClient(createClient({ fetch }));
  return patches;
}

/** The status badge in an item's header. */
const status = (item: HTMLElement) => item.querySelector("header [data-meaning]");

function renderPanel(onSelect = vi.fn()) {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <DivergencePanel sid="01s" onSelect={onSelect} />
    </QueryClientProvider>,
  );
  return onSelect;
}

describe("session divergences", () => {
  it("lists a divergence and selects the events it cites", async () => {
    daemon([200, {}]);
    const onSelect = renderPanel();
    const item = await screen.findByRole("article", { name: /D002 out-of-scope-write/ });
    expect(item).toHaveTextContent("wrote /home/user/.bashrc outside the workspace");
    expect(item).toHaveTextContent("critical");
    fireEvent.click(within(item).getByRole("button", { name: "Select event #62" }));
    expect(onSelect).toHaveBeenCalledWith(62);
  });

  it("acknowledges with a note", async () => {
    const patches = daemon([200, view({ status: "acknowledged", note: "real leak" })]);
    renderPanel();
    const item = await screen.findByRole("article", { name: /D002/ });
    fireEvent.click(within(item).getByRole("button", { name: "Acknowledge" }));
    fireEvent.change(within(item).getByLabelText("Note"), { target: { value: "real leak" } });
    fireEvent.click(within(item).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(status(item)).toHaveTextContent("acknowledged"));
    expect(item).toHaveTextContent("Note: real leak");
    expect(patches).toEqual([{ status: "acknowledged", note: "real leak" }]);
  });

  it("marks a false positive without a note", async () => {
    const patches = daemon([200, view({ status: "false_positive" })]);
    renderPanel();
    const item = await screen.findByRole("article", { name: /D002/ });
    fireEvent.click(within(item).getByRole("button", { name: "False positive" }));
    fireEvent.click(within(item).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(status(item)).toHaveTextContent("false positive"));
    expect(patches).toEqual([{ status: "false_positive", note: null }]);
  });

  it("shows a refused triage and changes nothing", async () => {
    daemon([
      409,
      { type: "busy", title: "Conflict", status: 409, detail: "the session is recording" },
    ]);
    renderPanel();
    const item = await screen.findByRole("article", { name: /D002/ });
    fireEvent.click(within(item).getByRole("button", { name: "Acknowledge" }));
    fireEvent.change(within(item).getByLabelText("Note"), { target: { value: "seen" } });
    fireEvent.click(within(item).getByRole("button", { name: "Save" }));
    expect(await within(item).findByRole("alert")).toHaveTextContent(
      "409 Conflict: the session is recording",
    );
    expect(status(item)).toHaveTextContent("open");
    // The note is kept for another try.
    expect(within(item).getByLabelText("Note")).toHaveValue("seen");
  });

  it("says when a session has none", async () => {
    setApiClient(
      createClient({
        fetch: (async () =>
          new Response(JSON.stringify({ divergences: [] }))) as unknown as typeof fetch,
      }),
    );
    renderPanel();
    expect(await screen.findByText(/No divergences/)).toBeInTheDocument();
  });
});
