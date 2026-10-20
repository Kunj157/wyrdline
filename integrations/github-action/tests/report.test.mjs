// node --test integrations/github-action/tests
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";

import {
  escapeCell,
  failing,
  marker,
  openDivergences,
  parseFailOn,
  prNumber,
  render,
  upsertComment,
} from "../scripts/report.mjs";

const D = (rule_id, severity, extra = {}) => ({
  divergence_id: `dv-${rule_id}`,
  rule_id,
  rule_name: "name",
  severity,
  status: "open",
  evidence_seqs: [3, 9],
  summary: `summary of ${rule_id}`,
  ...extra,
});

test("fail-on: severities are thresholds, rule ids are exact", () => {
  const list = [D("D001", "high"), D("D002", "medium"), D("D006", "low"), D("D003", "critical")];
  const ids = (spec) => failing(list, parseFailOn(spec)).map((d) => d.rule_id);
  assert.deepEqual(ids(""), []);
  assert.deepEqual(ids("none"), []);
  assert.deepEqual(ids("high"), ["D001", "D003"]);
  assert.deepEqual(ids("medium, d006"), ["D001", "D002", "D006", "D003"]);
  assert.deepEqual(ids("D002"), ["D002"]);
  assert.throws(() => parseFailOn("D1"), /neither a rule id/);
  assert.throws(() => parseFailOn("severe"), /neither a rule id/);
});

test("triaged divergences neither count nor fail", () => {
  const list = [
    D("D001", "high", { status: "false_positive" }),
    D("D002", "high", { status: "acknowledged" }),
  ];
  assert.equal(openDivergences(list).length, 0);
  assert.deepEqual(failing(list, parseFailOn("low")), []);
});

test("render: table of open divergences with evidence and links", () => {
  const md = render({
    divergences: [D("D001", "high"), D("D002", "medium", { status: "acknowledged" })],
    sessionId: "01abc",
    agentExitCode: "0",
    sensor: "running",
    artifactUrl: "https://github.com/o/r/actions/runs/1/artifacts/2",
    failed: [],
  });
  assert.match(md, /^### Wyrdline: 1 divergence$/m);
  assert.match(md, /\| D001 name \| high \| summary of D001 \| #3, #9 \|/);
  assert.doesNotMatch(md, /D002/);
  assert.match(md, /\[bundle\]\(https:\/\/github.com\/o\/r\/actions\/runs\/1\/artifacts\/2\)/);
  assert.match(md, /kernel capture on/);
});

test("render: degraded capture, warnings, and fail-on are visible", () => {
  const md = render({
    divergences: [],
    sensor: "degraded",
    warnings: ["wyrd-sensord is not running"],
    failed: [D("D003", "critical")],
  });
  assert.match(md, /no divergences/);
  assert.match(md, /\*\*kernel capture degraded\*\*/);
  assert.match(md, /> ⚠ wyrd-sensord is not running/);
  assert.match(md, /fail-on matched\*\* D003/);
});

test("untrusted text cannot break the table, inject HTML, or mention", () => {
  const s = escapeCell("a|b\n<img src=x> `code` @octocat &");
  assert.equal(s.includes("\n"), false);
  assert.equal(s.includes("<"), false);
  assert.equal(s.includes("`"), false);
  assert.match(s, /a\\\|b/);
  assert.match(s, /@​octocat/);
  assert.equal(escapeCell("x".repeat(1000)).length, 300);
  // A marker key cannot close the HTML comment.
  const m = marker("job --> <b>");
  assert.equal(m.indexOf("-->"), m.length - 3);
  assert.equal(marker("agent-e2e.v1"), "<!-- wyrdline-action:agent-e2e.v1 -->");
});

test("PR number from pull_request and issue_comment events only", () => {
  assert.equal(prNumber({ pull_request: { number: 7 } }), 7);
  assert.equal(prNumber({ issue: { number: 8, pull_request: {} } }), 8);
  assert.equal(prNumber({ issue: { number: 9 } }), null);
  assert.equal(prNumber({}), null);
});

/** A fake GitHub issues-comments API holding comments in memory. */
async function fakeApi({ status = 200, existing = [] } = {}) {
  const comments = [...existing];
  const calls = [];
  const server = createServer((req, res) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      calls.push(`${req.method} ${req.url}`);
      assert.equal(req.headers.authorization, "Bearer t0k");
      if (status !== 200) {
        res.writeHead(status).end("{}");
        return;
      }
      const body = raw ? JSON.parse(raw) : null;
      const url = new URL(req.url, "http://x");
      res.setHeader("content-type", "application/json");
      if (req.method === "GET") {
        const page = Number(url.searchParams.get("page"));
        res.end(JSON.stringify(comments.slice((page - 1) * 100, page * 100)));
      } else if (req.method === "POST") {
        const c = { id: 1000 + comments.length, body: body.body };
        comments.push(c);
        res.writeHead(201).end(JSON.stringify(c));
      } else if (req.method === "PATCH") {
        const id = Number(url.pathname.split("/").pop());
        comments.find((c) => c.id === id).body = body.body;
        res.end(JSON.stringify({ id }));
      }
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const api = `http://127.0.0.1:${server.address().port}`;
  return { api, comments, calls, close: () => server.close() };
}

test("one comment per key: created once, then updated in place", async () => {
  const others = Array.from({ length: 150 }, (_, i) => ({ id: i + 1, body: `unrelated ${i}` }));
  const gh = await fakeApi({ existing: others });
  try {
    const args = { api: gh.api, token: "t0k", repo: "o/r", pr: 5, mark: marker("e2e") };
    const first = await upsertComment({ ...args, body: "first" });
    assert.equal(first.action, "created");
    const second = await upsertComment({ ...args, body: "second" });
    assert.deepEqual(second, { action: "updated", id: first.id });
    const ours = gh.comments.filter((c) => c.body.startsWith(marker("e2e")));
    assert.equal(ours.length, 1);
    assert.equal(ours[0].body, `${marker("e2e")}\nsecond`);
    // A different key is a different comment.
    assert.equal(
      (await upsertComment({ ...args, mark: marker("other"), body: "x" })).action,
      "created",
    );
    assert.ok(gh.calls.includes("GET /repos/o/r/issues/5/comments?per_page=100&page=2"));
  } finally {
    gh.close();
  }
});

test("a refused comment API surfaces the status (fork PRs)", async () => {
  const gh = await fakeApi({ status: 403 });
  try {
    await assert.rejects(
      upsertComment({
        api: gh.api,
        token: "t0k",
        repo: "o/r",
        pr: 5,
        mark: marker("k"),
        body: "b",
      }),
      (e) => e.status === 403,
    );
  } finally {
    gh.close();
  }
});
