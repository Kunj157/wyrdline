import assert from "node:assert/strict";
import { test } from "node:test";

import { splitWords } from "../scripts/argv.mjs";

test("words, quotes, escapes, and line continuations", () => {
  assert.deepEqual(splitWords("claude -p 'fix the failing cart test'"), [
    "claude",
    "-p",
    "fix the failing cart test",
  ]);
  assert.deepEqual(splitWords('a "b c" d\\ e'), ["a", "b c", "d e"]);
  assert.deepEqual(splitWords(`x "say \\"hi\\" \\\\ \\$HOME"`), ["x", 'say "hi" \\ $HOME']);
  assert.deepEqual(splitWords("wyrd-fakeagent run \\\n  demo/s.toml\n"), [
    "wyrd-fakeagent",
    "run",
    "demo/s.toml",
  ]);
  assert.deepEqual(splitWords("a '' b"), ["a", "", "b"]);
  assert.deepEqual(splitWords("a'b'\"c\""), ["abc"]);
  assert.deepEqual(splitWords("bash -c 'make test && echo $?'"), [
    "bash",
    "-c",
    "make test && echo $?",
  ]);
});

test("nothing can expand or run outside the Cell", () => {
  for (const bad of [
    "a; b",
    "a | b",
    "a && b",
    "echo $HOME",
    "echo `id`",
    'echo "$(id)"',
    'x "`id`"',
    "a > f",
    "(a)",
  ]) {
    assert.throws(() => splitWords(bad), /command:/, bad);
  }
});

test("malformed input is refused", () => {
  assert.throws(() => splitWords("a 'b"), /unbalanced/);
  assert.throws(() => splitWords('a "b'), /unbalanced/);
  assert.throws(() => splitWords("a \\"), /trailing backslash/);
  assert.throws(() => splitWords("  \n "), /empty/);
});
