import { Link, Outlet } from "@tanstack/react-router";

import { useOffline } from "../lib/api";
import { Activity, LayoutGrid, TriangleAlert } from "lucide-react";
import type { ComponentType } from "react";

import { CommandPalette } from "../components/CommandPalette";

type NavItem = { to: string; label: string; icon: ComponentType<{ className?: string }> };

const nav: NavItem[] = [
  { to: "/", label: "Overview", icon: LayoutGrid },
  { to: "/sessions", label: "Sessions", icon: Activity },
  { to: "/divergences", label: "Divergences", icon: TriangleAlert },
];

export function RootLayout() {
  const offline = useOffline();
  const bundle = offline?.bundle ?? null;
  return (
    <div className="flex h-full">
      <aside className="hidden w-60 shrink-0 flex-col gap-1 border-r border-border bg-surface-1 p-3 md:flex">
        <div className="mb-4 flex items-center gap-2 px-2 py-1">
          <img src={`${import.meta.env.BASE_URL}wyrdline.svg`} alt="" className="size-7" />
          <span className="font-semibold tracking-tight">Wyrdline</span>
        </div>
        <nav aria-label="Primary" className="flex flex-col gap-0.5">
          {nav.map(({ label, icon: Icon, to }) => (
            <Link
              key={label}
              to={to}
              activeOptions={{ exact: to === "/" }}
              activeProps={{ "aria-current": "page", className: "bg-surface-2 text-text-primary" }}
              className="flex items-center gap-2.5 rounded-sm px-2 py-1.5 text-sm text-text-secondary transition-colors duration-[var(--duration-fast)] hover:bg-surface-2 hover:text-text-primary"
            >
              <Icon className="size-4" />
              {label}
            </Link>
          ))}
        </nav>
      </aside>
      <CommandPalette />
      <main className="min-w-0 flex-1 overflow-auto">
        <nav
          aria-label="Primary (compact)"
          className="flex items-center gap-1 border-b border-border bg-surface-1 px-3 py-2 md:hidden"
        >
          <img src={`${import.meta.env.BASE_URL}wyrdline.svg`} alt="" className="mr-1 size-6" />
          {nav.map(({ label, icon: Icon, to }) => (
            <Link
              key={label}
              to={to}
              activeOptions={{ exact: to === "/" }}
              activeProps={{ "aria-current": "page", className: "bg-surface-2 text-text-primary" }}
              className="flex items-center gap-1.5 rounded-sm px-2 py-1 text-sm text-text-secondary"
            >
              <Icon className="size-4" />
              {label}
            </Link>
          ))}
        </nav>
        {offline && (
          <div
            role="status"
            className="border-b border-border bg-accent-subtle px-4 py-2 text-sm text-text-primary"
          >
            {bundle ? (
              <>
                Bundle <span className="font-mono">{offline.name}</span> — verified in your browser
                (signature by <span className="font-mono">{bundle.signer_fingerprint}</span>, every
                file hash, the ledger chain), {bundle.redaction} redaction
                {bundle.content.included ? "" : ", no checkpoint content"}; read-only.
              </>
            ) : (
              <>
                Offline replay of <span className="font-mono">{offline.name}</span> — read-only.
              </>
            )}{" "}
            The causal graph is computed in your browser by Wyrdline's correlator, compiled to
            WebAssembly.
          </div>
        )}
        <Outlet />
      </main>
    </div>
  );
}
