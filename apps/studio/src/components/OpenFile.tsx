import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { FileUp } from "lucide-react";
import { useState } from "react";

import { setOffline } from "../lib/api";
import { BundleError, type BundleReport, openSessionFile, stageNames } from "../lib/bundle";
import { loadWasm } from "../lib/offline";

const mark = { passed: "✓", failed: "✗", skipped: "–" } as const;

/** Every check of a bundle that did not verify. Untrusted text, as text. */
export function BundleChecks({ report }: { report: BundleReport }) {
  return (
    <ul aria-label="Bundle checks" className="mt-1 flex flex-col gap-0.5 font-mono text-xs">
      {report.checks.map((c) => (
        <li
          key={c.stage}
          className={c.status === "failed" ? "text-verdict-denied" : "text-text-secondary"}
        >
          {mark[c.status]} {stageNames[c.stage] ?? c.stage}
          {c.detail ? `: ${c.detail}` : ""}
        </li>
      ))}
    </ul>
  );
}

/**
 * Open a `.wyrd` bundle (verified in the browser first) or an events export,
 * from the file picker or by dropping it here. Nothing is uploaded.
 */
export function OpenFile() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [error, setError] = useState<{ message: string; report: BundleReport | null } | null>(null);
  const [over, setOver] = useState(false);
  async function open(file: File) {
    setError(null);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const { client, bundle } = await openSessionFile(file.name, bytes, await loadWasm());
      setOffline(file.name, client, bundle);
      qc.clear();
      const sid = (await client.sessions()).sessions[0]?.id;
      if (sid) await navigate({ to: "/sessions/$sid", params: { sid } });
    } catch (e) {
      setError({
        message: e instanceof Error ? e.message : String(e),
        report: e instanceof BundleError ? e.report : null,
      });
    }
  }
  return (
    <div
      role="region"
      aria-label="Drop a bundle or session file"
      onDragOver={(e) => {
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        const f = e.dataTransfer.files[0];
        if (f) void open(f);
      }}
      className={`mt-3 rounded-card border border-dashed p-3 text-sm ${over ? "border-accent bg-accent-subtle" : "border-border-strong"}`}
    >
      <label className="inline-flex cursor-pointer items-center gap-2 rounded-sm border border-border-strong bg-surface-1 px-3 py-1.5 font-medium hover:bg-surface-2">
        <FileUp className="size-4" aria-hidden="true" />
        Open a bundle or session file
        <input
          type="file"
          accept=".wyrd,.json,.jsonl,application/json"
          className="sr-only"
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = "";
            if (f) void open(f);
          }}
        />
      </label>
      <span className="ml-2 text-text-muted">
        or drop it here: a <code className="font-mono">.wyrd</code> bundle (verified in your
        browser) or <code className="font-mono">wyrd events &lt;session&gt; --json</code> — nothing
        is uploaded
      </span>
      {error && (
        <div role="alert" className="mt-2 text-verdict-denied">
          <p>{error.message}</p>
          {error.report && <BundleChecks report={error.report} />}
        </div>
      )}
    </div>
  );
}
