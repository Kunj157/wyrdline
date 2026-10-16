/** Keyboard movement between knots, shared by every river renderer. */
import type { Knot } from "../lib/model";

const notable = (k: Knot) =>
  k.meaning === "denied" || k.meaning === "irreversible" || k.meaning === "divergence";

/** The knot index a key moves to from `index`, or -1 if the key does nothing. */
export function nextKnot(knots: Knot[], index: number, key: string): number {
  const last = knots.length - 1;
  if (last < 0) return -1;
  switch (key) {
    case "ArrowRight":
      return Math.min(last, index + 1);
    case "ArrowLeft":
      return Math.max(0, index - 1);
    case "Home":
      return 0;
    case "End":
      return last;
    case "n": {
      const i = knots.findIndex((k, j) => j > index && notable(k));
      return i;
    }
    case "p": {
      for (let j = Math.min(index, knots.length) - 1; j >= 0; j--) {
        const k = knots[j];
        if (k && notable(k)) return j;
      }
      return -1;
    }
    default:
      return -1;
  }
}
