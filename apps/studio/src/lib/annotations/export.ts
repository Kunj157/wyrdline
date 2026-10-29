import * as A from "@automerge/automerge/slim";

import type { AnnotationDoc } from "./doc";

/**
 * The saved annotation document as a file, named for its session. This is
 * the file `wyrd export --annotations <file>` takes.
 */
export function annotationFile(
  doc: AnnotationDoc,
  session: string,
): { name: string; bytes: Uint8Array } {
  return { name: `${session}.annotations.automerge`, bytes: A.save(doc) };
}

/** Save the annotation document through the browser's download. */
export function downloadAnnotations(doc: AnnotationDoc, session: string): void {
  const { name, bytes } = annotationFile(doc, session);
  const url = URL.createObjectURL(
    new Blob([bytes as BlobPart], { type: "application/octet-stream" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
