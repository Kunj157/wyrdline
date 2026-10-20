import { initializeWasm, isWasmInitialized } from "@automerge/automerge/slim";
import wasmUrl from "@automerge/automerge/automerge.wasm?url";

let ready: Promise<void> | undefined;

/**
 * Load the Automerge WebAssembly module once, on first use, so sessions
 * without annotations never fetch it. Tests initialise it from bytes.
 */
export function loadAutomerge(bytes?: Uint8Array): Promise<void> {
  if (isWasmInitialized()) return Promise.resolve();
  ready ??= initializeWasm(bytes ?? wasmUrl).catch((e: unknown) => {
    ready = undefined;
    throw e;
  });
  return ready;
}
