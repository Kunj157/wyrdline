import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

import { loadAutomerge } from "./automerge";

/** Initialise Automerge in Vitest from the package's wasm file. */
export function loadAutomergeForTests(): Promise<void> {
  const require = createRequire(import.meta.url);
  const path = require.resolve("@automerge/automerge/automerge.wasm");
  return loadAutomerge(new Uint8Array(readFileSync(path)));
}
