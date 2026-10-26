import * as A from "@automerge/automerge/slim";

/** Largest sync message accepted from a peer (the Hub enforces the same). */
export const MAX_MESSAGE_BYTES = 1 << 20;

/**
 * One side of an Automerge sync conversation, independent of the transport:
 * the WebSocket client feeds it frames and sends whatever it produces, and
 * the property tests drive it over a simulated lossy network.
 */
export class SyncPeer<T> {
  private state: A.SyncState = A.initSyncState();

  constructor(public doc: A.Doc<T>) {}

  /** The next message for the remote peer, or null when it is up to date. */
  outgoing(): Uint8Array | null {
    const [state, msg] = A.generateSyncMessage(this.doc, this.state);
    this.state = state;
    return msg;
  }

  /** Apply a message from the remote peer. */
  receive(msg: Uint8Array): void {
    if (msg.byteLength > MAX_MESSAGE_BYTES) throw new Error("sync message too large");
    const [doc, state] = A.receiveSyncMessage(this.doc, this.state, msg);
    this.doc = doc;
    this.state = state;
  }

  /**
   * Forget what the remote peer has: after a reconnect it may have lost
   * messages in flight, so the conversation starts over (the protocol then
   * resends whatever is missing).
   */
  reset(): void {
    this.state = A.initSyncState();
  }
}
