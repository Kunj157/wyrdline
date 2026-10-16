import { describe, expect, it } from "vitest";

import { type Command, filterCommands, seqQuery } from "./palette";

const cmds: Command[] = [
  { id: "s1", label: "fix the greeting", hint: "session · fakeagent", to: "/sessions/a" },
  { id: "s2", label: "cart tests", hint: "session · claude-code", to: "/sessions/b" },
  { id: "c3", label: "checkpoint c3", hint: "fix the greeting · branch a", to: "/sessions/a" },
];

describe("palette", () => {
  it("needs every word and ranks prefix matches first", () => {
    expect(filterCommands(cmds, "greet").map((c) => c.id)).toEqual(["s1", "c3"]);
    expect(filterCommands(cmds, "checkpoint greeting").map((c) => c.id)).toEqual(["c3"]);
    expect(filterCommands(cmds, "nothing")).toEqual([]);
    expect(filterCommands(cmds, "").length).toBe(3);
  });

  it("recognises seq jumps", () => {
    expect(seqQuery("#42")).toBe(42);
    expect(seqQuery("seq 7")).toBe(7);
    expect(seqQuery("#4x")).toBeNull();
  });
});
