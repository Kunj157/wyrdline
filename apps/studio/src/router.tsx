import { createRootRoute, createRoute, createRouter } from "@tanstack/react-router";

import { OverviewPage } from "./routes/overview";
import { RootLayout } from "./routes/root";
import { SessionPage } from "./routes/session";
import { SessionsPage } from "./routes/sessions";

const rootRoute = createRootRoute({ component: RootLayout });

const overviewRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: OverviewPage,
});

const sessionsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/sessions",
  component: SessionsPage,
});

const sessionRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/sessions/$sid",
  component: SessionPage,
});

export const routeTree = rootRoute.addChildren([overviewRoute, sessionsRoute, sessionRoute]);

export function makeRouter(history?: Parameters<typeof createRouter>[0]["history"]) {
  return createRouter({ routeTree, ...(history ? { history } : {}) });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
