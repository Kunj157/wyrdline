/**
 * Bundle mode: open a `.wyrd` bundle with no daemon and no network. The
 * bundle is verified in the browser by the same Rust reader as
 * `wyrd verify` (compiled to WebAssembly) — signature, every file hash,
 * the ledger hash chain — before anything in it is shown. Checkpoint
 * diffs and file contents come from the bundle's `cas/` when it has one.
 * Bundle content is untrusted: it is only ever rendered as text.
 */
import type { Client } from "@wyrdline/schemas/client";
import type * as api from "@wyrdline/schemas/generated-api";

import { type Wasm, createOfflineClient, parseExport, sessionFromEvents } from "./offline";

/** The four magic bytes of a zstd frame: every bundle starts with them. */
export function isBundle(bytes: Uint8Array): boolean {
  return bytes[0] === 0x28 && bytes[1] === 0xb5 && bytes[2] === 0x2f && bytes[3] === 0xfd;
}

export type Stage = "archive" | "signature" | "manifest" | "files" | "chain";

export const stageNames: Record<Stage, string> = {
  archive: "archive structure and limits",
  signature: "signature over the manifest",
  manifest: "manifest",
  files: "file hashes",
  chain: "ledger hash chain",
};

export interface Check {
  stage: Stage;
  status: "passed" | "failed" | "skipped";
  detail: string | null;
}

export interface BundleInfo {
  session: { id: string; name: string | null; agent: string };
  redaction: "standard" | "strict";
  content: { included: boolean; chunks: number; withheld_files: number };
  events: number;
  head_hash: string;
  manifest_hash: string;
  signer_fingerprint: string;
}

/** `wyrd verify <file.wyrd> --json`, without the path. */
export interface BundleReport {
  ok: boolean;
  checks: Check[];
  bundle: BundleInfo | null;
  error: { stage: Stage; seq: number | null; message: string } | null;
}

/** A bundle that did not verify; `report` lists every check. */
export class BundleError extends Error {
  constructor(readonly report: BundleReport) {
    const e = report.error;
    super(
      e
        ? `The bundle did not verify — ${stageNames[e.stage] ?? e.stage} failed: ${e.message}`
        : "The bundle did not verify",
    );
    this.name = "BundleError";
  }
}

/** What the WebAssembly `BundleView` offers (a fake in unit tests). */
export interface BundleView {
  report(): string;
  events(): string;
  diff(a: string, b: string): string;
  file(a: string, b: string, path: string): string;
}

/** The checkpoint a target names: a checkpoint id, a branch, or `head`. */
function resolve(detail: api.SessionDetail, target: string): api.CheckpointRecord {
  const cp = detail.checkpoints.find((c) => c.id === target);
  if (cp) return cp;
  const name = target === "head" ? detail.current_branch : target;
  const branch = detail.branches.find((b) => b.id === name || b.name === name);
  if (branch) {
    const own = detail.checkpoints.filter((c) => c.branch === branch.id).at(-1);
    const head = own ?? detail.checkpoints.find((c) => c.id === branch.forked_from);
    if (head) return head;
  }
  throw new Error(`no checkpoint or branch ${target} in this bundle`);
}

/**
 * Verify a bundle and serve it as a read-only client. Throws
 * [`BundleError`] (nothing exposed) when any check fails.
 */
export function createBundleClient(
  view: BundleView,
  name: string,
  correlate: ((eventsJson: string) => string) | null,
): { client: Client; report: BundleReport & { bundle: BundleInfo } } {
  const report = JSON.parse(view.report()) as BundleReport;
  if (!report.ok || !report.bundle) throw new BundleError(report);
  const info = report.bundle;
  const events = parseExport(view.events());
  const detail = sessionFromEvents(events, info.session.name ?? name);
  const base = createOfflineClient(events, info.session.name ?? name, correlate);
  const sealed = events.some((e) => e.body.kind === "meta.session.closed");
  const client = {
    ...base,
    session: async (s: string) => ({
      ...(await base.session(s)),
      head_hash: info.head_hash,
      sealed,
    }),
    verify: async (s: string) => {
      await base.session(s);
      return {
        ok: true,
        session_id: detail.id,
        events: info.events,
        head_hash: info.head_hash,
        sealed: true,
        signer_fingerprint: info.signer_fingerprint,
        problem: null,
      } as api.VerifyReport;
    },
    diff: async (s: string, a: string, b: string) => {
      await base.session(s);
      return JSON.parse(
        view.diff(resolve(detail, a).manifest, resolve(detail, b).manifest),
      ) as api.Changes;
    },
    diffFile: async (s: string, a: string, b: string, path: string) => {
      await base.session(s);
      return JSON.parse(
        view.file(resolve(detail, a).manifest, resolve(detail, b).manifest, path),
      ) as api.FileDiff;
    },
  };
  return { client: client as unknown as Client, report: { ...report, bundle: info } };
}

/**
 * Open a file the user chose or dropped: a `.wyrd` bundle (verified first)
 * or an events export (`wyrd events <session> --json`).
 */
export async function openSessionFile(
  name: string,
  bytes: Uint8Array,
  wasm: Wasm | null,
): Promise<{ client: Client; bundle: BundleInfo | null }> {
  if (isBundle(bytes)) {
    if (!wasm) throw new Error("This Studio build has no WebAssembly bundle reader.");
    const { client, report } = createBundleClient(new wasm.BundleView(bytes), name, wasm.correlate);
    return { client, bundle: report.bundle };
  }
  const events = parseExport(new TextDecoder().decode(bytes));
  return { client: createOfflineClient(events, name, wasm?.correlate ?? null), bundle: null };
}
