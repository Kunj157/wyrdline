import { repo } from "../content";
import { GithubMark } from "./GithubMark";

const logo = `${import.meta.env.BASE_URL}wyrdline.svg`;

export function Nav() {
  return (
    <header className="sticky top-0 z-30 border-b border-border bg-surface-glass backdrop-blur-md">
      <nav
        aria-label="Main"
        className="mx-auto flex h-14 max-w-6xl items-center gap-6 px-4 md:px-6"
      >
        <a href="#top" className="flex items-center gap-2 font-semibold tracking-tight">
          <img src={logo} alt="" width={26} height={26} />
          Wyrdline
        </a>
        <div className="ml-auto hidden items-center gap-5 text-sm text-text-secondary md:flex">
          <a href="#how" className="hover:text-text-primary">
            How it works
          </a>
          <a href="#features" className="hover:text-text-primary">
            Features
          </a>
          <a href="#demo" className="hover:text-text-primary">
            Demo
          </a>
          <a href="#roadmap" className="hover:text-text-primary">
            Roadmap
          </a>
        </div>
        <a
          href={repo}
          className="ml-auto inline-flex items-center gap-1.5 rounded-sm border border-border-strong bg-surface-1 px-3 py-1.5 text-sm font-medium hover:bg-surface-2 md:ml-0"
        >
          <GithubMark className="size-4" />
          GitHub
        </a>
      </nav>
    </header>
  );
}

export function Footer() {
  return (
    <footer className="border-t border-border bg-surface-2">
      <div className="mx-auto flex max-w-6xl flex-wrap items-center gap-4 px-4 py-8 text-sm text-text-secondary md:px-6">
        <img src={logo} alt="" width={22} height={22} />
        <span>Wyrdline · MIT licensed · the eBPF programs are MIT OR GPL-2.0</span>
        <a href={repo} className="ml-auto text-accent hover:text-accent-hover">
          Source on GitHub
        </a>
      </div>
    </footer>
  );
}
