#!/usr/bin/env node
// Split the action's `command` input into argv words, shell-style quoting
// but nothing else: no expansion, no globbing, no command substitution.
// The agent must be the program `wyrd run` starts (adapters such as
// claude-code add their hook settings to its argv), and nothing in the
// input may run outside the Cell. Shell features need an explicit
// `bash -c '…'`. Prints the words NUL-terminated.

import { pathToFileURL } from "node:url";

const SPECIAL = new Set([";", "|", "&", "<", ">", "(", ")", "$", "`"]);

export function splitWords(input) {
  const words = [];
  let word = null;
  let quote = null;
  const s = String(input ?? "");
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (quote === "'") {
      if (c === "'") quote = null;
      else word += c;
    } else if (quote === '"') {
      if (c === '"') quote = null;
      else if (c === "\\" && i + 1 < s.length && '"\\$`\n'.includes(s[i + 1])) {
        i++;
        if (s[i] !== "\n") word += s[i];
      } else if (c === "$" || c === "`") {
        throw new Error(
          `command: "${c}" inside double quotes would expand; use single quotes or bash -c`,
        );
      } else word += c;
    } else if (/\s/.test(c)) {
      if (word !== null) words.push(word);
      word = null;
    } else if (c === "\\") {
      i++;
      if (i >= s.length) throw new Error("command: trailing backslash");
      if (s[i] !== "\n") word = (word ?? "") + s[i];
    } else if (c === "'" || c === '"') {
      quote = c;
      word = word ?? "";
    } else if (SPECIAL.has(c)) {
      throw new Error(
        `command: "${c}" is a shell operator; quote it, or run a script with bash -c '…'`,
      );
    } else {
      word = (word ?? "") + c;
    }
  }
  if (quote) throw new Error(`command: unbalanced ${quote}`);
  if (word !== null) words.push(word);
  if (words.length === 0) throw new Error("command: empty");
  return words;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  try {
    process.stdout.write(
      splitWords(process.env.WYRD_COMMAND)
        .map((w) => `${w}\0`)
        .join(""),
    );
  } catch (e) {
    console.log(`::error::wyrdline ${e.message}`);
    process.exit(1);
  }
}
