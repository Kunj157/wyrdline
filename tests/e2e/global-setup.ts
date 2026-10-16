import { type ChildProcess, execFileSync, spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(__dirname, "../..");
const bin = process.env.WYRD_E2E_BIN ?? join(root, "target/debug");
const studio = process.env.WYRD_E2E_STUDIO ?? join(root, "apps/studio/dist");

function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const s = createServer();
    s.once("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const addr = s.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      s.close(() => ok(port));
    });
  });
}

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 15_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await ok()) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** Fixture session + wyrdd; returns the teardown. */
export default async function globalSetup() {
  for (const need of [
    join(bin, "wyrdd"),
    join(bin, "wyrd"),
    join(bin, "examples/e2e_fixture"),
    join(studio, "index.html"),
  ]) {
    if (!existsSync(need)) throw new Error(`missing ${need}: run make test-e2e (it builds them)`);
  }
  // A short directory: unix socket paths are limited to 107 bytes.
  const dir = mkdtempSync(join(tmpdir(), "wyrd-e2e-"));
  const data = join(dir, "data");
  const [sid = "", liveSid = ""] = execFileSync(
    join(bin, "examples/e2e_fixture"),
    [data, join(dir, "ws")],
    { encoding: "utf8" },
  )
    .trim()
    .split("\n");
  const port = await freePort();
  const env = {
    ...process.env,
    WYRD_DATA_DIR: data,
    XDG_STATE_HOME: join(dir, "state"),
    WYRD_LOG: "warn",
  };
  const daemon: ChildProcess = spawn(
    join(bin, "wyrdd"),
    ["--socket", join(dir, "d.sock"), "--listen", `127.0.0.1:${port}`, "--studio", studio],
    { env, stdio: ["ignore", "ignore", "inherit"] },
  );
  const tokenFile = join(dir, "state/wyrdline/token");
  await until("the daemon token", () => existsSync(tokenFile));
  const token = readFileSync(tokenFile, "utf8").trim();
  const base = `http://127.0.0.1:${port}`;
  await until("the daemon", async () => {
    try {
      return (await fetch(`${base}/v1/health`, { headers: { authorization: `Bearer ${token}` } }))
        .ok;
    } catch {
      return false;
    }
  });
  // An export of the same session for the offline-replay spec, served
  // next to the Studio build.
  const exported = execFileSync(join(bin, "wyrd"), ["events", sid, "--json"], {
    encoding: "utf8",
    env,
  });
  writeFileSync(join(studio, "e2e-offline.jsonl"), exported);
  process.env.WYRD_E2E_BASE = base;
  process.env.WYRD_E2E_TOKEN = token;
  process.env.WYRD_E2E_SID = sid;
  process.env.WYRD_E2E_LIVE_SID = liveSid;
  process.env.WYRD_E2E_DATA = data;
  process.env.WYRD_E2E_FIXTURE = join(bin, "examples/e2e_fixture");
  return () => {
    daemon.kill("SIGTERM");
    rmSync(dir, { recursive: true, force: true });
  };
}
