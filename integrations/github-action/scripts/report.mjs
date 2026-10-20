#!/usr/bin/env node
// Wyrdline GitHub Action: divergence summary for the job summary and one
// PR comment (created once, then updated in place), and the fail-on gate.
// No dependencies: runs on the runner's Node (>= 20, global fetch).

import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const SEVERITIES = ["low", "medium", "high", "critical"];

/** Hidden marker that identifies this action's comment for one job. */
export function marker(key) {
  return `<!-- wyrdline-action:${String(key).replace(/[^A-Za-z0-9_.-]/g, "_")} -->`;
}

/**
 * Text from the session (paths, quotes the agent wrote) is untrusted: keep
 * it on one line, out of tables and HTML, and unable to mention anyone.
 */
export function escapeCell(text) {
  return String(text ?? "")
    .replace(/[\r\n]+/g, " ")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\|/g, "\\|")
    .replace(/`/g, "'")
    .replace(/@/g, "@​")
    .slice(0, 300);
}

/** Open divergences only: acknowledged or false-positive ones are triaged. */
export function openDivergences(list) {
  return (Array.isArray(list) ? list : []).filter((d) => (d.status ?? "open") === "open");
}

/**
 * `fail-on`: comma-separated rule ids (`D003`) and severities (`high`
 * fails on high and critical). Empty or `none` never fails.
 */
export function parseFailOn(spec) {
  const rules = new Set();
  let minSeverity = null;
  for (const raw of String(spec ?? "").split(/[\s,]+/)) {
    const item = raw.trim();
    if (!item || item.toLowerCase() === "none") continue;
    const sev = SEVERITIES.indexOf(item.toLowerCase());
    if (sev >= 0) {
      minSeverity = minSeverity === null ? sev : Math.min(minSeverity, sev);
    } else if (/^D\d{3}$/i.test(item)) {
      rules.add(item.toUpperCase());
    } else {
      throw new Error(
        `fail-on: "${item}" is neither a rule id (D001) nor a severity (${SEVERITIES.join(", ")})`,
      );
    }
  }
  return { rules, minSeverity };
}

/** The open divergences that trip the `fail-on` gate. */
export function failing(divergences, failOn) {
  const { rules, minSeverity } = failOn;
  return openDivergences(divergences).filter(
    (d) =>
      rules.has(String(d.rule_id).toUpperCase()) ||
      (minSeverity !== null && SEVERITIES.indexOf(d.severity) >= minSeverity),
  );
}

/** Markdown summary shared by the job summary and the PR comment. */
export function render(ctx) {
  const open = openDivergences(ctx.divergences);
  const lines = [];
  const count = open.length;
  const head = count === 0 ? "no divergences" : `${count} divergence${count === 1 ? "" : "s"}`;
  lines.push(`### Wyrdline: ${head}`);
  lines.push("");
  const facts = [];
  if (ctx.sessionId) facts.push(`session \`${escapeCell(ctx.sessionId)}\``);
  if (ctx.agentExitCode !== undefined && ctx.agentExitCode !== "")
    facts.push(`agent exit ${escapeCell(ctx.agentExitCode)}`);
  facts.push(ctx.sensor === "running" ? "kernel capture on" : "**kernel capture degraded**");
  if (ctx.artifactUrl) facts.push(`[bundle](${encodeURI(ctx.artifactUrl)})`);
  if (ctx.runUrl) facts.push(`[run](${encodeURI(ctx.runUrl)})`);
  lines.push(facts.join(" · "));
  lines.push("");
  for (const w of ctx.warnings ?? []) lines.push(`> ⚠ ${escapeCell(w)}`);
  if ((ctx.warnings ?? []).length) lines.push("");
  if (count > 0) {
    lines.push("| Rule | Severity | What happened | Evidence |");
    lines.push("| --- | --- | --- | --- |");
    for (const d of open) {
      const rule = `${escapeCell(d.rule_id)} ${escapeCell(d.rule_name ?? "")}`.trim();
      const seqs = (d.evidence_seqs ?? []).map((s) => `#${Number(s)}`).join(", ");
      lines.push(`| ${rule} | ${escapeCell(d.severity)} | ${escapeCell(d.summary)} | ${seqs} |`);
    }
    lines.push("");
  }
  if (ctx.failed?.length) {
    lines.push(`**fail-on matched** ${ctx.failed.map((d) => escapeCell(d.rule_id)).join(", ")}.`);
    lines.push("");
  }
  lines.push(
    "<sub>Open the bundle with `wyrd open` or in Studio; evidence numbers are ledger sequence numbers.</sub>",
  );
  return lines.join("\n") + "\n";
}

/** The PR this run belongs to, from the event payload; null otherwise. */
export function prNumber(event) {
  if (event?.pull_request?.number) return event.pull_request.number;
  if (event?.issue?.pull_request && event.issue.number) return event.issue.number;
  return null;
}

async function gh(api, token, method, path, body) {
  const res = await fetch(`${api}${path}`, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      accept: "application/vnd.github+json",
      "x-github-api-version": "2022-11-28",
      ...(body ? { "content-type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    const err = new Error(`${method} ${path}: HTTP ${res.status}`);
    err.status = res.status;
    throw err;
  }
  return res.status === 204 ? null : res.json();
}

/**
 * Create or update the one comment carrying `mark` on the PR. Returns
 * `{ action, id }`; throws with `.status` when the API refuses.
 */
export async function upsertComment({ api, token, repo, pr, mark, body }) {
  const full = `${mark}\n${body}`;
  for (let page = 1; page <= 50; page++) {
    const list = await gh(
      api,
      token,
      "GET",
      `/repos/${repo}/issues/${pr}/comments?per_page=100&page=${page}`,
    );
    const found = list.find((c) => typeof c.body === "string" && c.body.startsWith(mark));
    if (found) {
      await gh(api, token, "PATCH", `/repos/${repo}/issues/comments/${found.id}`, { body: full });
      return { action: "updated", id: found.id };
    }
    if (list.length < 100) break;
  }
  const created = await gh(api, token, "POST", `/repos/${repo}/issues/${pr}/comments`, {
    body: full,
  });
  return { action: "created", id: created.id };
}

function readJson(path, fallback) {
  if (!path || !existsSync(path)) return fallback;
  return JSON.parse(readFileSync(path, "utf8"));
}

async function main(env) {
  const divergences = readJson(env.WYRD_DIVERGENCES_JSON, []);
  const warnings = existsSync(env.WYRD_WARNINGS_FILE ?? "")
    ? readFileSync(env.WYRD_WARNINGS_FILE, "utf8").split("\n").filter(Boolean)
    : [];
  const failOn = parseFailOn(env.WYRD_FAIL_ON);
  const failed = failing(divergences, failOn);
  const runUrl = env.GITHUB_RUN_ID
    ? `${env.GITHUB_SERVER_URL}/${env.GITHUB_REPOSITORY}/actions/runs/${env.GITHUB_RUN_ID}`
    : "";
  const body = render({
    divergences,
    warnings,
    failed,
    sessionId: env.WYRD_SESSION_ID,
    agentExitCode: env.WYRD_AGENT_EXIT_CODE,
    sensor: env.WYRD_SENSOR,
    artifactUrl: env.WYRD_ARTIFACT_URL,
    runUrl,
  });
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, body);

  const event = readJson(env.GITHUB_EVENT_PATH, {});
  const pr = prNumber(event);
  if (env.WYRD_COMMENT === "true" && pr && env.WYRD_GITHUB_TOKEN) {
    try {
      const r = await upsertComment({
        api: env.GITHUB_API_URL ?? "https://api.github.com",
        token: env.WYRD_GITHUB_TOKEN,
        repo: env.GITHUB_REPOSITORY,
        pr,
        mark: marker(env.WYRD_COMMENT_KEY || env.GITHUB_JOB || "wyrdline"),
        body,
      });
      console.log(`wyrdline: PR #${pr} comment ${r.action} (${r.id})`);
    } catch (e) {
      // Fork PRs get a read-only token: the job summary has everything.
      console.log(
        `::warning::wyrdline: could not comment on PR #${pr} (${e.message}); the summary is in the job summary`,
      );
    }
  }
  if (env.GITHUB_OUTPUT)
    appendFileSync(env.GITHUB_OUTPUT, `failed-rules=${failed.map((d) => d.rule_id).join(",")}\n`);
  for (const d of failed)
    console.log(`::error::wyrdline fail-on: ${d.rule_id} (${d.severity}) ${escapeCell(d.summary)}`);
  return failed.length > 0 ? 1 : 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main(process.env).then(
    (code) => process.exit(code),
    (e) => {
      console.log(`::error::wyrdline report: ${e.message}`);
      process.exit(1);
    },
  );
}
