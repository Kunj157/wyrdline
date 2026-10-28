import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { createClient } from "@wyrdline/schemas/client";
import type * as api from "@wyrdline/schemas/generated-api";

import { setApiClient } from "../lib/api";
import { makeRouter } from "../router";

const session = (id: string, name: string) =>
  ({
    id,
    name,
    status: "paused",
    created_ms: 1,
    checkpoints: [],
    branches: [],
  }) as unknown as api.SessionRecord;

const divergence = (id: string, rule_id: string, rule_name: string, status = "open") =>
  ({
    seq: 9,
    divergence_id: id,
    rule_id,
    rule_name,
    severity: "high",
    claim_id: null,
    claim: null,
    evidence_seqs: [3],
    confidence_bp: 9000,
    summary: `${rule_name} in ${id}`,
    status,
    note: null,
  }) as api.DivergenceView;

const data: Record<string, unknown> = {
  "/v1/sessions": { sessions: [session("s1", "first run"), session("s2", "second run")] },
  "/v1/sessions/s1/divergences": {
    divergences: [
      divergence("dv-1", "D002", "out-of-scope-write"),
      divergence("dv-2", "D001", "claim-without-evidence", "acknowledged"),
    ],
  },
  "/v1/sessions/s2/divergences": {
    divergences: [divergence("dv-3", "D002", "out-of-scope-write")],
  },
};

function renderInbox() {
  setApiClient(
    createClient({
      fetch: (async (url: string) =>
        new Response(JSON.stringify(data[url] ?? {}), {
          status: url in data ? 200 : 404,
        })) as unknown as typeof fetch,
    }),
  );
  const router = makeRouter(createMemoryHistory({ initialEntries: ["/divergences"] }));
  render(
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

describe("divergence inbox", () => {
  it("groups open divergences across sessions by rule", async () => {
    renderInbox();
    const group = await screen.findByRole("region", { name: /D002 out-of-scope-write/ });
    const items = within(group).getAllByRole("article");
    expect(items).toHaveLength(2);
    expect(within(group).getByRole("link", { name: "first run" })).toBeInTheDocument();
    expect(within(group).getByRole("link", { name: "second run" })).toBeInTheDocument();
    // Acknowledged ones are out of the open view.
    expect(screen.queryByRole("region", { name: /D001/ })).toBeNull();
    expect(screen.getByText(/2 open across 2 sessions/)).toBeInTheDocument();
  });

  it("shows every divergence on request", async () => {
    renderInbox();
    await screen.findByRole("region", { name: /D002/ });
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    const group = await screen.findByRole("region", { name: /D001 claim-without-evidence/ });
    expect(within(group).getByText("acknowledged")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "All" })).toHaveAttribute("aria-pressed", "true");
  });

  it("is in the navigation", async () => {
    renderInbox();
    const nav = await screen.findByRole("navigation", { name: "Primary" });
    expect(within(nav).getByRole("link", { name: "Divergences" })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });
});
