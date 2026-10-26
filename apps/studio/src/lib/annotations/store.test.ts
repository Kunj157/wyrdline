import * as A from "@automerge/automerge/slim";
import { IDBFactory } from "fake-indexeddb";
import { beforeAll, describe, expect, it } from "vitest";

import { addComment, createAnnotations, loadAnnotations, viewAnnotations } from "./doc";
import { indexedDbStore } from "./store";
import { loadAutomergeForTests } from "./test-wasm";

beforeAll(loadAutomergeForTests);

describe("IndexedDB annotation store", () => {
  it("keeps each session's document across store instances", async () => {
    const factory = new IDBFactory();
    let d = createAnnotations("s1");
    d = addComment(d, { from: 2, to: 4, text: "kept offline", author: "ana" }, "c1");
    await indexedDbStore(factory).save("s1", A.save(d));

    const again = indexedDbStore(factory);
    const bytes = await again.load("s1");
    expect(bytes).not.toBeNull();
    expect(viewAnnotations(loadAnnotations(bytes ?? new Uint8Array())).comments[0]?.text).toBe(
      "kept offline",
    );
    expect(await again.load("other")).toBeNull();
  });

  it("overwrites with the newest save", async () => {
    const store = indexedDbStore(new IDBFactory());
    let d = createAnnotations("s1");
    await store.save("s1", A.save(d));
    d = addComment(d, { from: 1, to: 1, text: "second", author: "bo" }, "c2");
    await store.save("s1", A.save(d));
    const back = loadAnnotations((await store.load("s1")) ?? new Uint8Array());
    expect(viewAnnotations(back).comments.map((c) => c.id)).toEqual(["c2"]);
  });
});
