import { render, screen } from "@testing-library/react";

import { App } from "./App";
import { demoUrl, planes } from "./content";

const flags = globalThis as { WYRD_REDUCED?: boolean };

afterEach(() => {
  flags.WYRD_REDUCED = false;
});

describe("landing page", () => {
  it("has one title and shows the three planes", () => {
    render(<App />);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    for (const p of planes) expect(screen.getByText(`${p.name} plane`)).toBeInTheDocument();
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

  it("shows the static weave and no canvas", () => {
    render(<App />);
    expect(screen.getByRole("img", { name: /threads from tool calls/i })).toBeInTheDocument();
    expect(document.querySelector("canvas")).toBeNull();
    expect(document.querySelector("[data-3d]")).toHaveAttribute("data-3d", "off");
  });
});
