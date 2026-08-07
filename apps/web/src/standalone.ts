/**
 * Whether this build has a Gridline server behind it.
 *
 * A standalone build is the whole spreadsheet and nothing else: the engine
 * runs in the browser, so every formula, every function, import and export all
 * work with no backend at all. What it drops is the half that needs one —
 * capture, the consent notice, the transparency page and mined routines.
 *
 * Dropping them is not only a technical convenience. A link sent to a friend
 * should not be quietly recording what they type, even hashed, and a consent
 * notice offering a choice that cannot take effect is worse than no notice:
 * it asks for a decision and then ignores it. So the honest standalone build
 * does not ask, does not record, and does not show controls for either.
 *
 * Set `VITE_STANDALONE=1` at build time. `scripts/dev.sh` does not, so the
 * development loop is unchanged.
 */
export function isStandalone(): boolean {
  const env = import.meta.env as Record<string, string | boolean | undefined>
  return env?.VITE_STANDALONE === '1' || env?.VITE_STANDALONE === 'true'
}
