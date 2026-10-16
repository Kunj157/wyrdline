import "@testing-library/jest-dom/vitest";

// jsdom has no layout or media queries; the page only needs them to exist.
// `reduce` is switchable per test through the WYRD_REDUCED flag.
const flags = globalThis as { WYRD_REDUCED?: boolean };
window.matchMedia = (query: string) =>
  ({
    matches: query.includes("prefers-reduced-motion") && !!flags.WYRD_REDUCED,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  }) as MediaQueryList;
class Observer {
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
}
window.IntersectionObserver = Observer as unknown as typeof IntersectionObserver;
window.ResizeObserver = Observer as unknown as typeof ResizeObserver;
// No WebGL in jsdom: the hero takes its static path.
HTMLCanvasElement.prototype.getContext = (() =>
  null) as typeof HTMLCanvasElement.prototype.getContext;
