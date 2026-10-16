import { render, screen, within } from "@testing-library/react";

import { App } from "./App";
import { demoUrl, features, roadmap } from "./content";

const flags = globalThis as { WYRD_REDUCED?: boolean };

afterEach(() => {
  flags.WYRD_REDUCED = false;
});

describe("landing page", () => {
  it("has one title and a section per shipped feature and roadmap phase", () => {
    render(<App />);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    for (const f of features) {
      expect(screen.getByRole("heading", { level: 3, name: f.title })).toBeInTheDocument();
    }
    const next = screen.getByRole("region", { name: "Where the thread goes next." });
    expect(within(next).getAllByRole("listitem")).toHaveLength(roadmap.length);
  });

  it("links the demo to the offline Studio replaying the recorded session", () => {
    render(<App />);
    for (const link of screen.getAllByRole("link", { name: /open the live demo/i })) {
      expect(link).toHaveAttribute("href", demoUrl);
    }
    expect(demoUrl).toMatch(/studio\/\?offline=.*studio\/demo-session\.jsonl$/);
  });

  it("gives every screenshot a description", () => {
    render(<App />);
    for (const img of screen.getAllByRole("img")) {
      if (img.getAttribute("alt") === "") continue; // decorative logo next to the name
      expect(img.getAttribute("alt") ?? img.getAttribute("aria-label")).toBeTruthy();
    }
  });

  it("shows the static weave and loads no 3D under reduced motion", () => {
    flags.WYRD_REDUCED = true;
    render(<App />);
    expect(screen.getByRole("img", { name: /threads from tool calls/i })).toBeInTheDocument();
    expect(document.querySelector("canvas")).toBeNull();
    expect(document.querySelector("[data-3d]")).toHaveAttribute("data-3d", "off");
  });
});
