import { createRootRoute, createRoute, createRouter } from "@tanstack/react-router";

import { RootLayout } from "./routes/root";
import { OverviewPage } from "./routes/overview";

const rootRoute = createRootRoute({ component: RootLayout });

const overviewRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: OverviewPage,
});

export const routeTree = rootRoute.addChildren([overviewRoute]);

export function makeRouter(history?: Parameters<typeof createRouter>[0]["history"]) {
  return createRouter({ routeTree, ...(history ? { history } : {}) });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
