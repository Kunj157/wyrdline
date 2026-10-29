import { createRootRoute, createRoute, createRouter } from "@tanstack/react-router";

import { DivergencesPage } from "./routes/divergences";
import { HubDashboardPage, HubSearchPage, validateHubSearch } from "./routes/hub";
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

const divergencesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/divergences",
  component: DivergencesPage,
});

const hubRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/hub",
  component: HubDashboardPage,
  validateSearch: validateHubSearch,
});

const hubSearchRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/hub/search",
  component: HubSearchPage,
  validateSearch: validateHubSearch,
});

export const routeTree = rootRoute.addChildren([
  overviewRoute,
  sessionsRoute,
  sessionRoute,
  divergencesRoute,
  hubRoute,
  hubSearchRoute,
]);

export function makeRouter(history?: Parameters<typeof createRouter>[0]["history"]) {
  // Served at the site root by wyrdd, or below a path (GitHub Pages demo).
  const basepath = import.meta.env.BASE_URL.replace(/\/$/, "") || "/";
  return createRouter({ routeTree, basepath, ...(history ? { history } : {}) });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
