import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { apiClient, setOffline } from "./lib/api";
import { createOfflineClient, loadCorrelator, parseExport } from "./lib/offline";
import { makeRouter } from "./router";
import "./styles.css";

/** `?offline=<url>` replays an exported session (the landing page's demo). */
async function boot(): Promise<string | null> {
  const src = new URLSearchParams(window.location.search).get("offline");
  if (!src) {
    // Take the token out of the address bar before anything renders.
    apiClient();
    return null;
  }
  const url = new URL(src, window.location.href);
  if (url.origin !== window.location.origin)
    throw new Error("offline files must come from this site");
  const [text, correlate] = await Promise.all([fetch(url).then((r) => r.text()), loadCorrelator()]);
  const events = parseExport(text);
  const name = url.pathname.split("/").pop() ?? "session";
  const client = createOfflineClient(events, name, correlate);
  setOffline(name, client);
  return (await client.sessions()).sessions[0]?.id ?? null;
}

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

void boot().then((sid) => {
  const queryClient = new QueryClient();
  const router = makeRouter();
  if (sid) void router.navigate({ to: "/sessions/$sid", params: { sid } });
  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </StrictMode>,
  );
});
