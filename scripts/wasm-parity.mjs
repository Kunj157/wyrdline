// Native-vs-Wasm parity: run the WebAssembly correlator on every fixture and
// require byte-identical output to the native build.
// Usage: node scripts/wasm-parity.mjs <fixture-dir> <wasm-pkg-dir>
import { readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";

const [dir, pkg] = process.argv.slice(2).map((p) => resolve(p));
const require = createRequire(import.meta.url);
const wasm = require(join(pkg, "wyrd_wasm.js"));
let checked = 0;
let failed = 0;
for (const f of readdirSync(dir)
  .filter((f) => f.endsWith(".events.json"))
  .sort()) {
  const events = readFileSync(join(dir, f), "utf8");
  const expected = readFileSync(join(dir, f.replace(".events.json", ".graph.json")), "utf8");
  const got = wasm.correlate(events);
  checked += 1;
  if (got !== expected) {
    failed += 1;
    console.error(`MISMATCH ${f}`);
  }
}
console.log(`wasm parity: ${checked - failed}/${checked} identical (schema ${wasm.schema_id()})`);
process.exit(failed === 0 && checked > 0 ? 0 : 1);
