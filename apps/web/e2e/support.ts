/**
 * Shared setup for the specs that are about the grid rather than about capture.
 *
 * They all used to open the app and click *Decline* on the consent notice,
 * which quietly made them depend on a database they do not control: the modal
 * only appears when the server has no consent on file for this actor, so the
 * whole suite passed on a fresh checkout and hung for thirty seconds a test on
 * any machine where somebody had answered the notice once. Green in CI, red for
 * the developer — the worst way round.
 *
 * Answering it in `localStorage` before the app boots is deterministic, needs
 * no server, and is faster. `capture.spec.ts` keeps driving the real modal,
 * because there the modal *is* the subject.
 */

import { expect, type Page } from '@playwright/test'

const CONSENT_KEY = 'gridline.consent'

/**
 * Open the app with the consent notice already declined.
 *
 * Declined rather than granted so these tests transmit nothing: what they
 * assert about is the grid, and a spec that also fills a database is a spec
 * that can fail for a reason it was never about.
 */
export async function openGrid(page: Page): Promise<void> {
  await page.addInitScript(
    ({ key }) => {
      window.localStorage.setItem(
        key,
        JSON.stringify({
          mode: 'off',
          consent_text_version: '1',
          granted_at_ms: Date.now(),
        }),
      )
    },
    { key: CONSENT_KEY },
  )
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  await expect(page.getByTestId('consent-modal')).toHaveCount(0)
}
