import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer as httpServer, request as httpRequest } from "node:http";
import { connect, createServer as netServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import wyrddSetup from "../global-setup";

/**
 * The Hub-backed Studio test. One same-origin proxy fronts wyrdd (Studio and
 * the session API, from the fixture) and the Hub (`/api/…`), so the Hub's
 * Origin check and session cookie behave as they do behind a reverse proxy.
 *
 * The Hub's bundle upload is #198 and not on dev yet, so the harness seeds
 * the rows the annotation sync reads (org, users, memberships, sessions, one
 * bundle) with psql. The sync endpoint reads no bundle object.
 *
 * Needs (CI provides them): WYRD_E2E_HUB_BIN (the wyrd-hub binary),
 * WYRD_E2E_HUB_DATABASE_URL (an empty database), WYRD_E2E_HUB_S3_ENDPOINT,
 * WYRD_E2E_HUB_S3_KEY and WYRD_E2E_HUB_S3_SECRET (MinIO), and psql.
 */

const env = (name: string): string => {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required for the Hub end-to-end test (CI sets it)`);
  return v;
};

function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const s = netServer();
    s.once("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const addr = s.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      s.close(() => ok(port));
    });
  });
}

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 30_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await ok()) return;
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** The same-origin proxy: `/api/…` to the Hub, everything else to wyrdd. */
function proxy(port: number, hubPort: number, wyrddPort: number) {
  const server = httpServer((req, res) => {
    const target = req.url?.startsWith("/api/") ? hubPort : wyrddPort;
    const up = httpRequest(
      {
        host: "127.0.0.1",
        port: target,
        method: req.method,
        path: req.url,
        headers: { ...req.headers, host: `127.0.0.1:${target}` },
      },
      (r) => {
        res.writeHead(r.statusCode ?? 502, r.headers);
        r.pipe(res);
      },
    );
    up.on("error", () => {
      if (!res.headersSent) res.writeHead(502);
      res.end();
    });
    req.pipe(up);
  });
  // WebSockets (the annotation sync) are only proxied to the Hub.
  server.on("upgrade", (req, socket, head) => {
    if (!req.url?.startsWith("/api/")) {
      socket.destroy();
      return;
    }
    const up = connect(hubPort, "127.0.0.1", () => {
      const lines = [`${req.method} ${req.url} HTTP/1.1`];
      for (let i = 0; i < req.rawHeaders.length; i += 2) {
        lines.push(`${req.rawHeaders[i]}: ${req.rawHeaders[i + 1]}`);
      }
      up.write(`${lines.join("\r\n")}\r\n\r\n`);
      if (head.length) up.write(head);
      up.pipe(socket);
      socket.pipe(up);
    });
    up.on("error", () => socket.destroy());
    socket.on("error", () => up.destroy());
  });
  return new Promise<import("node:http").Server>((ok) =>
    server.listen(port, "127.0.0.1", () => ok(server)),
  );
}

/** The Hub's own session secrets for three teammates, by name. */
const people = { alice: "member", bob: "member", carol: "viewer" } as const;

export default async function globalSetup() {
  const hubBin = env("WYRD_E2E_HUB_BIN");
  const pgUrl = env("WYRD_E2E_HUB_DATABASE_URL");
  const s3 = env("WYRD_E2E_HUB_S3_ENDPOINT");
  if (!existsSync(hubBin)) throw new Error(`missing ${hubBin}: build wyrd-hub first`);

  const teardownWyrdd = await wyrddSetup();
  const wyrddBase = process.env.WYRD_E2E_BASE ?? "";
  const sid = process.env.WYRD_E2E_SID ?? "";
  // The id goes into SQL below: only the characters a session id can hold.
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(sid))
    throw new Error("the fixture session id is not a plain id");
  const wyrddPort = Number(new URL(wyrddBase).port);

  const port = await freePort();
  const hubPort = await freePort();
  const origin = `http://127.0.0.1:${port}`;
  const dir = mkdtempSync(join(tmpdir(), "wyrd-hub-e2e-"));
  const bucket = `wyrd-e2e-${randomBytes(3).toString("hex")}`;
  const server = await proxy(port, hubPort, wyrddPort);

  const hub: ChildProcess = spawn(hubBin, [], {
    env: {
      ...process.env,
      WYRD_HUB_LISTEN: `127.0.0.1:${hubPort}`,
      WYRD_HUB_PUBLIC_URL: origin,
      WYRD_HUB_DATABASE_URL: pgUrl,
      WYRD_HUB_S3_ENDPOINT: s3,
      WYRD_HUB_S3_BUCKET: bucket,
      WYRD_HUB_S3_ACCESS_KEY_ID: env("WYRD_E2E_HUB_S3_KEY"),
      WYRD_HUB_S3_SECRET_ACCESS_KEY: env("WYRD_E2E_HUB_S3_SECRET"),
      WYRD_HUB_DATA_DIR: join(dir, "data"),
      // No identity provider: teammates get sessions from the seed below.
      WYRD_HUB_OIDC_ISSUER: "",
      WYRD_HUB_OIDC_CLIENT_ID: "",
      WYRD_HUB_OIDC_CLIENT_SECRET: "",
    },
    stdio: ["ignore", "ignore", "inherit"],
  });
  const ready = async () => {
    try {
      return (await fetch(`http://127.0.0.1:${hubPort}/readyz`)).ok;
    } catch {
      return false;
    }
  };
  await until("the Hub", ready);

  const psql = (sql: string) =>
    execFileSync("psql", [pgUrl, "-v", "ON_ERROR_STOP=1", "-q", "-c", sql], {
      stdio: ["ignore", "ignore", "inherit"],
    });
  const secrets: Record<string, string> = {};
  const sessions: string[] = [];
  const users: string[] = [];
  const members: string[] = [];
  for (const [name, role] of Object.entries(people)) {
    const secret = randomBytes(24).toString("hex");
    secrets[name] = secret;
    const hash = createHash("sha256").update(secret).digest("hex");
    users.push(`('u_${name}', 'https://idp.e2e.test', '${name}', '${name}')`);
    members.push(`('org_e2e', 'u_${name}', '${role}')`);
    sessions.push(`('${hash}', 'u_${name}', now() + interval '1 day')`);
  }
  psql(`
    INSERT INTO orgs (id, slug, name) VALUES ('org_e2e', 'e2e', 'E2E team');
    INSERT INTO users (id, issuer, subject, name) VALUES ${users.join(", ")};
    INSERT INTO memberships (org_id, user_id, role) VALUES ${members.join(", ")};
    INSERT INTO sessions (id_hash, user_id, expires_at) VALUES ${sessions.join(", ")};
    INSERT INTO bundles (id, org_id, uploaded_by, manifest_hash, signer, session_id,
      session_name, agent, events, repo, size_bytes, storage_key)
    VALUES ('bnd_e2e', 'org_e2e', 'u_alice', 'b3:e2e', 'e2e', '${sid}',
      'e2e greeting fix', 'fakeagent', 0, 'e2e/demo', 0, 'bundles/bnd_e2e');
  `);

  process.env.WYRD_E2E_BASE = origin;
  process.env.WYRD_E2E_HUB_BUNDLE = "bnd_e2e";
  process.env.WYRD_E2E_HUB_SECRETS = JSON.stringify(secrets);
  return async () => {
    hub.kill("SIGTERM");
    server.close();
    rmSync(dir, { recursive: true, force: true });
    await teardownWyrdd();
  };
}
