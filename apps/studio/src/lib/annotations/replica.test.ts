import { beforeAll, describe, expect, it } from "vitest";

import { addComment } from "./doc";
import { AnnotationReplica } from "./replica";
import { memoryStore } from "./store";
import { loadAutomergeForTests } from "./test-wasm";

beforeAll(loadAutomergeForTests);

describe("annotation replica", () => {
  it("saves local edits and reopens them", async () => {
    const store = memoryStore();
    const r = await AnnotationReplica.open("s1", store);
    let notified = 0;
    let sent = 0;
    r.subscribe(() => notified++);
    r.onLocalChange = () => sent++;
    r.change((d) => addComment(d, { from: 1, to: 2, text: "note", author: "ana" }, "c1"));
    expect([notified, sent]).toEqual([1, 1]);
    await r.flush();
    const again = await AnnotationReplica.open("s1", store);
    expect(again.view().comments.map((c) => c.id)).toEqual(["c1"]);
  });

  it("starts fresh from a corrupt local copy", async () => {
    const store = memoryStore();
    await store.save("s1", new Uint8Array([1, 2, 3]));
    const r = await AnnotationReplica.open("s1", store);
    expect(r.view().comments).toEqual([]);
  });
});
