import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { render, screen } from "@testing-library/react";

import { makeRouter } from "./router";

describe("Studio shell", () => {
  it("renders the overview with schema planes", async () => {
    const router = makeRouter(createMemoryHistory({ initialEntries: ["/"] }));
    render(
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent(
      "Every agent leaves a thread.",
    );
    expect(screen.getByRole("navigation", { name: "Primary" })).toBeInTheDocument();
    expect(screen.getByText("effect plane")).toBeInTheDocument();
    expect(screen.getByText("schema wyrdline.event.v1")).toBeInTheDocument();
  });
});
