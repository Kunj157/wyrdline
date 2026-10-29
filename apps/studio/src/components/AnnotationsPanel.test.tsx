import { fireEvent, render, screen, within } from "@testing-library/react";
import type * as api from "@wyrdline/schemas/generated-api";

import { loadAutomergeForTests } from "../lib/annotations/test-wasm";
import { AnnotationsPanel } from "./AnnotationsPanel";

beforeAll(loadAutomergeForTests);

const divergence = {
  divergence_id: "dv-1",
  rule_id: "D003",
  rule_name: "secret-read-then-egress",
} as api.DivergenceView;
const checkpoint = { id: "cp-9", seq: 9 } as api.CheckpointRecord;

function renderPanel(sid: string, selected: number | null = 12, onSelect = vi.fn()) {
  render(
    <AnnotationsPanel
      sid={sid}
      selected={selected}
      onSelect={onSelect}
      divergences={[divergence]}
      checkpoints={[checkpoint]}
    />,
  );
  return onSelect;
}

describe("annotations panel", () => {
  it("anchors a new comment to the selected event and resolves it", async () => {
    const onSelect = renderPanel("s-comments");
    const form = await screen.findByRole("form", { name: "Add a comment" });
    expect(within(form).getByLabelText("From seq")).toHaveValue("12");
    fireEvent.change(within(form).getByLabelText("Your name"), { target: { value: "ana" } });
    fireEvent.change(within(form).getByLabelText("Comment"), {
      target: { value: "curl right after the .env read" },
    });
    fireEvent.click(within(form).getByRole("button", { name: "Add comment" }));

    const comment = await screen.findByRole("article", { name: "Comment by ana on seq 12" });
    expect(comment).toHaveTextContent("curl right after the .env read");
    fireEvent.click(within(comment).getByRole("button", { name: "seq 12" }));
    expect(onSelect).toHaveBeenCalledWith(12);
    fireEvent.click(within(comment).getByRole("button", { name: "Resolve" }));
    expect(comment).toHaveTextContent("resolved");
    fireEvent.click(within(comment).getByRole("button", { name: "Delete" }));
    expect(screen.queryByRole("article")).toBeNull();
  });

  it("refuses an inverted range with a message", async () => {
    renderPanel("s-range");
    const form = await screen.findByRole("form", { name: "Add a comment" });
    fireEvent.change(within(form).getByLabelText("To seq"), { target: { value: "3" } });
    fireEvent.change(within(form).getByLabelText("Comment"), { target: { value: "x" } });
    fireEvent.click(within(form).getByRole("button", { name: "Add comment" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/seq range/);
  });

  it("records team triage and pinned checkpoints", async () => {
    renderPanel("s-triage", null);
    const triage = await screen.findByRole("combobox", { name: /D003/ });
    fireEvent.change(triage, { target: { value: "false_positive" } });
    expect(triage).toHaveValue("false_positive");
    const pin = screen.getByRole("checkbox", { name: "Pin checkpoint at seq 9" });
    fireEvent.click(pin);
    expect(pin).toBeChecked();
    expect(screen.getByText(/pinned by/)).toBeInTheDocument();
  });
});
