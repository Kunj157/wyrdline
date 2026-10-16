import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { Badge, Button, Dialog, Tabs } from "./index";

describe("Badge", () => {
  it("pairs every meaning colour with an icon and a label", () => {
    for (const meaning of [
      "intent",
      "effect",
      "divergence",
      "denied",
      "irreversible",
      "ok",
      "unknown",
    ] as const) {
      const { container, unmount } = render(<Badge meaning={meaning} />);
      const badge = container.querySelector(`[data-meaning="${meaning}"]`);
      expect(badge).toHaveTextContent(meaning);
      expect(badge?.querySelector("svg[aria-hidden='true']")).not.toBeNull();
      unmount();
    }
  });
});

describe("Button", () => {
  it("is a plain button unless told otherwise", () => {
    render(<Button>Rewind</Button>);
    expect(screen.getByRole("button", { name: "Rewind" })).toHaveAttribute("type", "button");
  });
});

function TabsHarness() {
  const [value, setValue] = useState("a");
  return (
    <Tabs
      label="Inspector"
      value={value}
      onChange={setValue}
      tabs={[
        { id: "a", label: "Event", panel: <p>event panel</p> },
        { id: "b", label: "Attribution", panel: <p>attribution panel</p> },
        { id: "c", label: "Policy", panel: <p>policy panel</p> },
      ]}
    />
  );
}

describe("Tabs", () => {
  it("moves with arrow keys, Home, and End and shows the selected panel", () => {
    render(<TabsHarness />);
    const first = screen.getByRole("tab", { name: "Event" });
    expect(first).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(first, { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "Attribution" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("tabpanel")).toHaveTextContent("attribution panel");
    fireEvent.keyDown(screen.getByRole("tab", { name: "Attribution" }), { key: "End" });
    expect(screen.getByRole("tab", { name: "Policy" })).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(screen.getByRole("tab", { name: "Policy" }), { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "Event" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getAllByRole("tab").filter((t) => t.tabIndex === 0)).toHaveLength(1);
  });
});

describe("Dialog", () => {
  it("is labelled by its title and closes from the close button", () => {
    const onClose = vi.fn();
    render(
      <Dialog open onClose={onClose} title="Rewind to c3" description="Lists irreversible effects">
        <p>body</p>
      </Dialog>,
    );
    const dialog = screen.getByRole("dialog", { name: "Rewind to c3" });
    expect(dialog).toHaveAccessibleDescription("Lists irreversible effects");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalled();
  });
});
