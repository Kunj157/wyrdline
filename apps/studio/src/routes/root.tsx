import { Link, Outlet } from "@tanstack/react-router";
import { Activity, GitFork, LayoutGrid, ShieldCheck, TriangleAlert } from "lucide-react";
import type { ComponentType } from "react";

type NavItem = { to: string; label: string; icon: ComponentType<{ className?: string }> };

const nav: NavItem[] = [
  { to: "/", label: "Overview", icon: LayoutGrid },
  { to: "/", label: "Sessions", icon: Activity },
  { to: "/", label: "Divergences", icon: TriangleAlert },
  { to: "/", label: "Policies", icon: ShieldCheck },
  { to: "/", label: "Branches", icon: GitFork },
];

export function RootLayout() {
  return (
    <div className="flex h-full">
      <aside className="hidden w-60 shrink-0 flex-col gap-1 border-r border-border bg-surface-1 p-3 md:flex">
        <div className="mb-4 flex items-center gap-2 px-2 py-1">
          <img src="/wyrdline.svg" alt="" className="size-7" />
          <span className="font-semibold tracking-tight">Wyrdline</span>
        </div>
        <nav aria-label="Primary" className="flex flex-col gap-0.5">
          {nav.map(({ label, icon: Icon, to }) => (
            <Link
              key={label}
              to={to}
              className="flex items-center gap-2.5 rounded-sm px-2 py-1.5 text-sm text-text-secondary transition-colors duration-[var(--duration-fast)] hover:bg-surface-2 hover:text-text-primary"
            >
              <Icon className="size-4" />
              {label}
            </Link>
          ))}
        </nav>
      </aside>
      <main className="min-w-0 flex-1 overflow-auto">
        <Outlet />
      </main>
    </div>
  );
}
