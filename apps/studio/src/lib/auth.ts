/**
 * The daemon token. `wyrd studio` opens Studio with `#token=…` in the URL
 * fragment (never sent to a server); it moves to session storage here and
 * is removed from the address bar.
 */
const KEY = "wyrd.token";

export function takeTokenFromUrl(
  loc: Location = window.location,
  store: Storage = sessionStorage,
): string | null {
  const m = /(?:^#|&)token=([0-9a-f]{16,128})(?:&|$)/.exec(loc.hash);
  if (m?.[1]) {
    try {
      store.setItem(KEY, m[1]);
    } catch {
      // Storage may be blocked; the token then lives for this page only.
    }
    window.history.replaceState(null, "", loc.pathname + loc.search);
    return m[1];
  }
  try {
    return store.getItem(KEY);
  } catch {
    return null;
  }
}
