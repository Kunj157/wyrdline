import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { BundleChecks } from "./components/OpenFile";
import { apiClient, setOffline } from "./lib/api";
import { BundleError, openSessionFile } from "./lib/bundle";
import { loadWasm } from "./lib/offline";
import { makeRouter } from "./router";
import "./styles.css";

/**
 * `?bundle=<url>` opens a `.wyrd` bundle (what `wyrd open` serves), and
 * `?offline=<url>` replays an exported session (the landing page's demo).
 * Both are read from this site only and opened without a daemon.
 */
async function boot(): Promise<string | null> {
  const params = new URLSearchParams(window.location.search);
  const src = params.get("bundle") ?? params.get("offline");
  if (!src) {
    // Take the token out of the address bar before anything renders.
    apiClient();
    return null;
  }
  const url = new URL(src, window.location.href);
  if (url.origin !== window.location.origin)
    throw new Error("offline files must come from this site");
  const [res, wasm] = await Promise.all([fetch(url), loadWasm()]);
  if (!res.ok) throw new Error(`could not read ${url.pathname}: HTTP ${res.status}`);
  const name = url.pathname.split("/").pop() ?? "session";
  const { client, bundle } = await openSessionFile(
    name,
    new Uint8Array(await res.arrayBuffer()),
    wasm,
  );
  setOffline(name, client, bundle);
  return (await client.sessions()).sessions[0]?.id ?? null;
}

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

/** A file that cannot be opened: say why, render nothing from it. */
function OpenFailed({ error }: { error: unknown }) {
  return (
    <main className="mx-auto max-w-3xl p-8">
      <h1 className="text-xl font-semibold">This file cannot be opened</h1>
      <div role="alert" className="mt-3 text-verdict-denied">
        <p>{error instanceof Error ? error.message : String(error)}</p>
        {error instanceof BundleError && <BundleChecks report={error.report} />}
      </div>
    </main>
  );
}

void boot().then(
  (sid) => {
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
  },
  (error: unknown) => {
    createRoot(root).render(
      <StrictMode>
        <OpenFailed error={error} />
      </StrictMode>,
    );
  },
);
