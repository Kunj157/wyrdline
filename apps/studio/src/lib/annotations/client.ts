import * as A from "@automerge/automerge/slim";

import { SyncPeer } from "./sync";

/**
 * The WebSocket side of annotation sync: one [`SyncPeer`] over a socket to
 * the Hub (or wyrdd) with reconnection and backoff. Local edits are applied
 * to the document at once and sent when the socket is open; while it is
 * not, they wait in the document and the protocol sends them on reconnect,
 * so nothing is lost. A change the server refuses (a viewer's edit, an
 * oversized document) is rolled back to what the server last confirmed
 * and reported through `onStatus`.
 */

export type SyncStatus = "connecting" | "live" | "offline" | "reconnecting" | "closed";
export type Role = "viewer" | "member" | "admin";

export interface StatusReport {
  status: SyncStatus;
  role: Role | null;
  /** The server's reason for refusing the last change, until the next change. */
  error: string | null;
}

/** What the client needs from a WebSocket (the browser's, or a fake in tests). */
export interface SocketLike {
  binaryType: string;
  readyState: number;
  send(data: Uint8Array): void;
  close(): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

export type SocketFactory = (url: string, protocols: string[]) => SocketLike;

export interface SyncClientOptions<T> {
  url: string;
  /** Subprotocols; wyrdd takes the bearer as `wyrd-token.<token>`. */
  protocols?: string[];
  doc: A.Doc<T>;
  onChange: (doc: A.Doc<T>) => void;
  onStatus: (report: StatusReport) => void;
  socket?: SocketFactory;
  /** Reconnect delay bounds in milliseconds. */
  backoff?: { initial: number; max: number };
  /** Jitter source in [0, 1). */
  random?: () => number;
}

const OPEN = 1;

/** The ws(s) form of a daemon URL, absolute against the page. */
export function wsUrl(url: string, base = globalThis.location?.href): string {
  const u = new URL(url, base);
  u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
  return u.toString();
}

const sameHeads = (a: readonly string[], b: readonly string[]) =>
  a.length === b.length && [...a].sort().every((h, i) => h === [...b].sort()[i]);

export class SyncClient<T> {
  private readonly peer: SyncPeer<T>;
  /** The document as the server last confirmed it (rollback target). */
  private confirmed: A.Doc<T>;
  private socket: SocketLike | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private attempts = 0;
  private closed = false;
  private role: Role | null = null;
  private error: string | null = null;
  private status: SyncStatus = "connecting";
  private readonly make: SocketFactory;
  private readonly backoff: { initial: number; max: number };
  private readonly random: () => number;
  private readonly online = () => this.reconnectNow();

  constructor(private readonly options: SyncClientOptions<T>) {
    this.peer = new SyncPeer(options.doc);
    this.confirmed = options.doc;
    this.make = options.socket ?? ((url, protocols) => new WebSocket(url, protocols));
    this.backoff = options.backoff ?? { initial: 500, max: 10_000 };
    this.random = options.random ?? Math.random;
    globalThis.addEventListener?.("online", this.online);
    this.connect();
  }

  get doc(): A.Doc<T> {
    return this.peer.doc;
  }

  /** Apply a local edit and send it (now, or once the socket is open). */
  change(edit: (doc: A.Doc<T>) => A.Doc<T>): void {
    this.peer.doc = edit(this.peer.doc);
    this.error = null;
    this.flush();
    this.report(this.status);
  }

  close(): void {
    this.closed = true;
    globalThis.removeEventListener?.("online", this.online);
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.socket?.close();
    this.socket = null;
    this.report("closed");
  }

  private report(status: SyncStatus): void {
    this.status = status;
    this.options.onStatus({ status, role: this.role, error: this.error });
  }

  private connect(): void {
    if (this.closed) return;
    this.timer = null;
    let socket: SocketLike;
    try {
      socket = this.make(this.options.url, this.options.protocols ?? []);
    } catch {
      this.scheduleReconnect();
      return;
    }
    socket.binaryType = "arraybuffer";
    this.socket = socket;
    socket.onopen = () => {
      if (socket !== this.socket) return;
      // Whatever the previous socket did not deliver is unknown now: start
      // the conversation over and the protocol resends what is missing.
      this.peer.reset();
      this.attempts = 0;
      this.report("live");
      this.flush();
    };
    socket.onmessage = (ev) => {
      if (socket !== this.socket) return;
      this.receive(ev.data);
    };
    socket.onerror = () => {
      // The close event follows; nothing to do here.
    };
    socket.onclose = () => {
      if (socket !== this.socket) return;
      this.socket = null;
      this.scheduleReconnect();
    };
  }

  private scheduleReconnect(): void {
    if (this.closed || this.timer) return;
    const offline = globalThis.navigator?.onLine === false;
    this.report(offline ? "offline" : "reconnecting");
    const base = Math.min(this.backoff.initial * 2 ** this.attempts, this.backoff.max);
    const delay = Math.round(base * (0.5 + this.random() / 2));
    this.attempts = Math.min(this.attempts + 1, 30);
    this.timer = setTimeout(() => this.connect(), delay);
  }

  private reconnectNow(): void {
    if (this.closed || this.socket) return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.attempts = 0;
    this.connect();
  }

  private flush(): void {
    const s = this.socket;
    if (!s || s.readyState !== OPEN) return;
    for (let m = this.peer.outgoing(); m; m = this.peer.outgoing()) s.send(m);
  }

  private receive(data: unknown): void {
    if (typeof data === "string") {
      this.note(data);
      return;
    }
    const bytes =
      data instanceof ArrayBuffer ? new Uint8Array(data) : ArrayBuffer.isView(data) ? data : null;
    if (!bytes) return;
    const msg = new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const before = A.getHeads(this.peer.doc);
    let theirs: string[];
    try {
      theirs = A.decodeSyncMessage(msg).heads;
      this.peer.receive(msg);
    } catch {
      // Not a sync message (or over the limit): drop the socket and resync.
      this.socket?.close();
      return;
    }
    const after = A.getHeads(this.peer.doc);
    // The server holds everything we hold: a safe point to roll back to.
    if (sameHeads(theirs, after)) this.confirmed = this.peer.doc;
    this.flush();
    if (!sameHeads(before, after)) this.options.onChange(this.peer.doc);
  }

  /** A text frame: the role on connect, or why a change was refused. */
  private note(text: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return;
    }
    if (!parsed || typeof parsed !== "object") return;
    const n = parsed as { role?: unknown; error?: unknown; message?: unknown };
    if (n.role === "viewer" || n.role === "member" || n.role === "admin") {
      this.role = n.role;
      this.report(this.status);
    }
    if (typeof n.error === "string") {
      this.error = typeof n.message === "string" ? n.message : n.error;
      // The server never applied what we sent: back to what it confirmed.
      this.peer.doc = this.confirmed;
      this.options.onChange(this.peer.doc);
      this.report(this.status);
    }
  }
}
