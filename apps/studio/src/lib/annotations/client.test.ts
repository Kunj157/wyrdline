import * as A from "@automerge/automerge/slim";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { type Role, type SocketLike, type StatusReport, SyncClient, wsUrl } from "./client";
import { addComment, type Annotations, createAnnotations, viewAnnotations } from "./doc";
import { AnnotationReplica } from "./replica";
import { memoryStore } from "./store";
import { SyncPeer } from "./sync";
import { loadAutomergeForTests } from "./test-wasm";

beforeAll(loadAutomergeForTests);

/** A socket the test opens, feeds and drops by hand. */
class FakeSocket implements SocketLike {
  binaryType = "blob";
  readyState = 0;
  onopen: SocketLike["onopen"] = null;
  onmessage: SocketLike["onmessage"] = null;
  onclose: SocketLike["onclose"] = null;
  onerror: SocketLike["onerror"] = null;
  constructor(
    readonly url: string,
    private readonly relay: FakeRelay,
  ) {}
  send(data: Uint8Array): void {
    this.relay.receive(this, data);
  }
  close(): void {
    this.end();
  }
  open(): void {
    this.readyState = 1;
    this.onopen?.({});
    this.relay.hello(this);
  }
  /** The server (or the network) ends the connection. */
  end(): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.relay.detach(this);
    this.onclose?.({});
  }
  deliver(data: Uint8Array | string): void {
    const payload = typeof data === "string" ? data : data.buffer.slice(0);
    this.onmessage?.({ data: payload });
  }
}

/** The Hub's side: one document, a sync peer per live socket, roles. */
class FakeRelay {
  doc = createAnnotations("s1");
  role: Role = "member";
  sockets: FakeSocket[] = [];
  private peers = new Map<FakeSocket, SyncPeer<Annotations>>();
  /** Every rejection the relay answered. */
  rejected: string[] = [];

  socket = (url: string): FakeSocket => {
    const s = new FakeSocket(url, this);
    this.sockets.push(s);
    return s;
  };

  /** The most recent socket the client made. */
  get last(): FakeSocket {
    const s = this.sockets.at(-1);
    if (!s) throw new Error("no socket yet");
    return s;
  }

  hello(s: FakeSocket): void {
    const peer = new SyncPeer(this.doc);
    this.peers.set(s, peer);
    s.deliver(JSON.stringify({ role: this.role }));
    this.pump(s, peer);
  }

  detach(s: FakeSocket): void {
    this.peers.delete(s);
  }

  receive(s: FakeSocket, data: Uint8Array): void {
    const peer = this.peers.get(s);
    if (!peer) return;
    if (this.role === "viewer" && A.decodeSyncMessage(data).changes.length > 0) {
      this.rejected.push("read_only");
      s.deliver(
        JSON.stringify({ error: "read_only", message: "viewers cannot change annotations" }),
      );
      s.end();
      return;
    }
    peer.doc = this.doc;
    peer.receive(data);
    this.doc = peer.doc;
    // Fan out: every peer, including the sender, gets what it misses.
    for (const [other, p] of this.peers) {
      p.doc = this.doc;
      this.pump(other, p);
    }
  }

  private pump(s: FakeSocket, peer: SyncPeer<Annotations>): void {
    for (let m = peer.outgoing(); m; m = peer.outgoing()) s.deliver(m);
  }

  comments(): string[] {
    return viewAnnotations(this.doc).comments.map((c) => c.text);
  }
}

function start(relay: FakeRelay, doc = createAnnotations("s1"), base?: A.Doc<Annotations>) {
  const statuses: StatusReport[] = [];
  const changes: A.Doc<Annotations>[] = [];
  const client = new SyncClient<Annotations>({
    url: "wss://hub.test/api/v1/sync?bundle=b1",
    doc,
    base,
    onChange: (d) => changes.push(d),
    onStatus: (r) => statuses.push(r),
    socket: relay.socket,
    backoff: { initial: 100, max: 1000 },
    random: () => 0.5,
  });
  return { client, statuses, changes };
}

const texts = (d: A.Doc<Annotations>) => viewAnnotations(d).comments.map((c) => c.text);
const add = (text: string) => (d: A.Doc<Annotations>) =>
  addComment(d, { from: 1, to: 2, text, author: "ana" });

describe("annotation sync client", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("sends edits made before the socket opened and applies the server's", () => {
    const relay = new FakeRelay();
    relay.doc = addComment(relay.doc, { from: 0, to: 0, text: "from hub", author: "bo" });
    const { client, statuses, changes } = start(relay);
    client.change(add("while connecting"));
    expect(relay.comments()).toEqual(["from hub"]);
    relay.last.open();
    expect(statuses.at(-1)).toMatchObject({ status: "live", role: "member", error: null });
    expect(relay.comments().sort()).toEqual(["from hub", "while connecting"]);
    expect(texts(client.doc).sort()).toEqual(["from hub", "while connecting"]);
    expect(changes.length).toBeGreaterThan(0);
    client.close();
    expect(statuses.at(-1)?.status).toBe("closed");
  });

  it("keeps edits made while the socket is down and resends them after reconnecting", () => {
    const relay = new FakeRelay();
    const { client, statuses } = start(relay);
    relay.last.open();
    client.change(add("before the drop"));
    expect(relay.comments()).toEqual(["before the drop"]);

    relay.last.end();
    expect(statuses.at(-1)?.status).toBe("reconnecting");
    client.change(add("while down"));
    expect(relay.comments()).toEqual(["before the drop"]);
    // Backoff: 100 ms × (0.5 + 0.5 / 2) = 75 ms before the first retry.
    expect(relay.sockets).toHaveLength(1);
    vi.advanceTimersByTime(74);
    expect(relay.sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(relay.sockets).toHaveLength(2);

    // The retry fails too: the delay doubles, up to the cap.
    relay.last.end();
    vi.advanceTimersByTime(149);
    expect(relay.sockets).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(relay.sockets).toHaveLength(3);
    for (let i = 0; i < 6; i++) {
      relay.last.end();
      vi.advanceTimersByTime(1000);
    }
    const before = relay.sockets.length;
    relay.last.end();
    vi.advanceTimersByTime(749);
    expect(relay.sockets).toHaveLength(before);
    vi.advanceTimersByTime(1);
    expect(relay.sockets).toHaveLength(before + 1);

    relay.last.open();
    expect(statuses.at(-1)?.status).toBe("live");
    expect(relay.comments().sort()).toEqual(["before the drop", "while down"]);
    client.close();
  });

  it("rolls a viewer's edit back when the server refuses it, and says why", () => {
    const relay = new FakeRelay();
    relay.role = "viewer";
    relay.doc = addComment(relay.doc, { from: 0, to: 0, text: "shared", author: "bo" });
    const { client, statuses, changes } = start(relay);
    relay.last.open();
    expect(statuses.at(-1)).toMatchObject({ status: "live", role: "viewer" });
    expect(texts(client.doc)).toEqual(["shared"]);

    client.change(add("mine"));
    expect(relay.rejected).toEqual(["read_only"]);
    expect(texts(client.doc)).toEqual(["shared"]);
    expect(texts(changes.at(-1) ?? client.doc)).toEqual(["shared"]);
    expect(statuses.at(-1)?.error).toBe("viewers cannot change annotations");
    // A refusal from the Hub ends the connection for good: nothing retries.
    expect(statuses.at(-1)?.status).toBe("closed");
    vi.advanceTimersByTime(60_000);
    expect(relay.sockets).toHaveLength(1);
    expect(relay.rejected).toEqual(["read_only"]);
    expect(relay.comments()).toEqual(["shared"]);
    client.close();
  });

  it("does not loop when a viewer's first connection to an empty bundle is refused", () => {
    // The bundle has no shared document yet, so the root change this
    // replica starts from is new to the Hub, and the Hub refuses it.
    const relay = new FakeRelay();
    relay.role = "viewer";
    relay.doc = A.init<Annotations>();
    const root = createAnnotations("s1");
    const { client, statuses } = start(
      relay,
      addComment(root, { from: 0, to: 0, text: "mine", author: "ana" }),
      root,
    );
    relay.last.open();
    expect(relay.rejected).toEqual(["read_only"]);
    expect(statuses.at(-1)).toMatchObject({
      status: "closed",
      error: "viewers cannot change annotations",
    });
    // Rolled back to the shared root, not to the local comment.
    expect(texts(client.doc)).toEqual([]);
    vi.advanceTimersByTime(60_000);
    expect(relay.sockets).toHaveLength(1);
    expect(relay.rejected).toEqual(["read_only"]);
  });

  it("drops a socket that sends garbage and resyncs on the next one", () => {
    const relay = new FakeRelay();
    const { client, statuses } = start(relay);
    relay.last.open();
    relay.last.deliver(new Uint8Array([1, 2, 3]));
    expect(statuses.at(-1)?.status).toBe("reconnecting");
    vi.advanceTimersByTime(100);
    relay.last.open();
    expect(statuses.at(-1)?.status).toBe("live");
    client.close();
  });

  it("reconnects at once when the browser comes back online", () => {
    const relay = new FakeRelay();
    const { client } = start(relay);
    relay.last.open();
    relay.last.end();
    expect(relay.sockets).toHaveLength(1);
    globalThis.dispatchEvent(new Event("online"));
    expect(relay.sockets).toHaveLength(2);
    client.close();
  });

  it("builds wss urls from same-origin paths", () => {
    expect(wsUrl("/api/v1/sync?bundle=b1", "https://hub.test/sessions/s1")).toBe(
      "wss://hub.test/api/v1/sync?bundle=b1",
    );
    expect(wsUrl("https://hub.test/api/v1/sync?bundle=b1", "https://x/")).toBe(
      "wss://hub.test/api/v1/sync?bundle=b1",
    );
  });
  it("hands the replica the newest document after every sync message, even one with no changes", async () => {
    // Automerge returns a new document object for each sync message. A
    // message with no changes still makes the old object unusable, so the
    // replica must be moved on each time, or its next local edit throws.
    const relay = new FakeRelay();
    relay.doc = addComment(relay.doc, { from: 0, to: 0, text: "shared", author: "bo" });
    const replica = await AnnotationReplica.open("s1", memoryStore());
    const client = new SyncClient<Annotations>({
      url: "wss://hub.test/api/v1/sync?bundle=b1",
      doc: replica.doc,
      onChange: (d) => replica.replace(d),
      onStatus: () => undefined,
      socket: relay.socket,
      backoff: { initial: 100, max: 1000 },
      random: () => 0.5,
    });
    replica.onLocalChange(() => client.change(() => replica.doc));
    relay.last.open();
    expect(texts(replica.doc)).toEqual(["shared"]);
    // Local edits after the exchange: each one needs a live document, even
    // when the Hub's answer to the first carried no changes.
    for (const text of ["mine", "and mine again"]) {
      expect(() =>
        replica.change((d) => addComment(d, { from: 1, to: 1, text, author: "ana" })),
      ).not.toThrow();
    }
    expect(relay.comments().sort()).toEqual(["and mine again", "mine", "shared"]);
    client.close();
  });
});
