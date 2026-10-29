import * as A from "@automerge/automerge/slim";
import { beforeAll, describe, expect, it } from "vitest";

import { addComment, type Annotations, createAnnotations, viewAnnotations } from "./doc";
import { annotationFile } from "./export";
import { loadAutomergeForTests } from "./test-wasm";

beforeAll(loadAutomergeForTests);

describe("annotation export", () => {
  it("names the file for its session and writes the document that loads back", () => {
    const doc = addComment(createAnnotations("01JSESSION"), {
      from: 3,
      to: 4,
      text: "the .env read starts here",
      author: "ana",
    });
    const { name, bytes } = annotationFile(doc, "01JSESSION");
    expect(name).toBe("01JSESSION.annotations.automerge");
    const back = A.load<Annotations>(bytes);
    expect(viewAnnotations(back).comments.map((c) => c.text)).toEqual([
      "the .env read starts here",
    ]);
  });
});
