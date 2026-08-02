/**
 * M4 acceptance: consent gates capture, and the controls users are promised
 * actually work.
 *
 * The claims in docs/PRIVACY.md are the kind a user can check for themselves
 * with a network tab open, so these tests check them the same way: by
 * counting real requests to /v1/events rather than by inspecting internal
 * state. A capture pipeline that *believes* it is paused is worth nothing.
 */

import { expect, test, type Page, type Request } from '@playwright/test'

const CONSENT_KEY = 'gridline.consent'
const TOKEN_KEY = 'gridline.token'

/** Count every request the page makes to the ingest endpoint. */
function trackIngest(page: Page): { count: () => number; bodies: () => string[] } {
  const seen: Request[] = []
  page.on('request', (r) => {
    if (r.url().includes('/v1/events')) seen.push(r)
  })
  return {
    count: () => seen.length,
    bodies: () => seen.map((r) => r.postData() ?? ''),
  }
}

/**
 * Stub the API so these tests exercise the client in isolation: the server's
 * own gating has its own suite, and a network dependency would make this
 * flaky rather than more truthful.
 */
async function stubApi(page: Page) {
  await page.route('**/v1/**', async (route) => {
    const url = route.request().url()
    if (url.includes('/v1/consent/me')) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ mode: null, captures: false }),
      })
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ accepted: 0, duplicates: 0, rejected: 0, warnings: [] }),
    })
  })
}

async function gotoFresh(page: Page, opts: { consent?: string } = {}) {
  await stubApi(page)
  await page.addInitScript(
    ({ key, tokenKey, consent }) => {
      window.localStorage.clear()
      window.localStorage.setItem(tokenKey, 'test-token')
      if (consent) window.localStorage.setItem(key, consent)
    },
    { key: CONSENT_KEY, tokenKey: TOKEN_KEY, consent: opts.consent ?? '' },
  )
  await page.goto('/')
}

/** Consent already granted in the given mode, so the modal stays away. */
function grantedConsent(mode: string) {
  return JSON.stringify({
    mode,
    consent_text_version: '2026-08-01',
    granted_at: new Date().toISOString(),
  })
}

const HEADER_W = 46
const HEADER_H = 24
const COL_W = 100
const ROW_H = 24

async function clickCell(page: Page, row: number, col: number) {
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  await page.mouse.click(
    box.x + HEADER_W + col * COL_W + COL_W / 2,
    box.y + HEADER_H + row * ROW_H + ROW_H / 2,
  )
}

async function typeInCell(page: Page, text: string) {
  await page.keyboard.type(text)
  await page.keyboard.press('Enter')
}

test.describe('consent', () => {
  test('the modal appears on first run and blocks nothing until answered', async ({
    page,
  }) => {
    await gotoFresh(page)
    const modal = page.getByTestId('consent-modal')
    await expect(modal).toBeVisible({ timeout: 30_000 })
    await expect(modal).toHaveAttribute('role', 'dialog')
    await expect(modal).toHaveAttribute('aria-modal', 'true')
    // The user must be told what each mode does, in the modal itself.
    await expect(modal).toContainText(/structural/i)
    await expect(modal).toContainText(/formula/i)
  })

  test('declining means no events are ever sent', async ({ page }) => {
    await gotoFresh(page)
    const ingest = trackIngest(page)
    const modal = page.getByTestId('consent-modal')
    await expect(modal).toBeVisible({ timeout: 30_000 })

    await page.getByTestId('consent-decline').click()
    await expect(page.getByTestId('consent-modal')).toHaveCount(0)

    await clickCell(page, 0, 0)
    await typeInCell(page, 'sensitive')
    await typeInCell(page, '12345')
    // Well past the 5s flush interval.
    await page.waitForTimeout(6000)

    expect(ingest.count(), 'events were sent despite declining').toBe(0)
    await expect(page.getByTestId('capture-chip')).toHaveAttribute('data-state', 'off')
  })

  test('choosing structural starts capture', async ({ page }) => {
    await gotoFresh(page)
    await expect(page.getByTestId('consent-modal')).toBeVisible({ timeout: 30_000 })
    await page.getByTestId('consent-mode-structural').click()
    await page.getByTestId('consent-accept').click()
    await expect(page.getByTestId('consent-modal')).toHaveCount(0)
    await expect(page.getByTestId('capture-chip')).toHaveAttribute(
      'data-state',
      'capturing',
    )
  })

  test('the modal does not reappear once answered', async ({ page }) => {
    await gotoFresh(page, { consent: grantedConsent('structural') })
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    await expect(page.getByTestId('consent-modal')).toHaveCount(0)
  })
})

test.describe('capture control', () => {
  test('pausing stops network traffic, and resuming restarts it', async ({ page }) => {
    await gotoFresh(page, { consent: grantedConsent('structural') })
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    const chip = page.getByTestId('capture-chip')
    await expect(chip).toHaveAttribute('data-state', 'capturing')

    // Establish that capture is working before claiming pause stopped it —
    // otherwise a broken pipeline would pass this test trivially.
    const ingest = trackIngest(page)
    await clickCell(page, 0, 0)
    await typeInCell(page, 'before pause')
    await page.waitForTimeout(6000)
    const whileCapturing = ingest.count()
    expect(whileCapturing, 'no events sent while capturing').toBeGreaterThan(0)

    await chip.click()
    await expect(chip).toHaveAttribute('data-state', 'paused')

    const afterPauseBaseline = ingest.count()
    await clickCell(page, 1, 0)
    await typeInCell(page, 'during pause')
    await typeInCell(page, 'still paused')
    await page.waitForTimeout(6000)
    expect(
      ingest.count(),
      'requests were made to /v1/events while capture was paused',
    ).toBe(afterPauseBaseline)

    await chip.click()
    await expect(chip).toHaveAttribute('data-state', 'capturing')
    await clickCell(page, 2, 0)
    await typeInCell(page, 'after resume')
    await page.waitForTimeout(6000)
    expect(ingest.count(), 'capture did not restart after resume').toBeGreaterThan(
      afterPauseBaseline,
    )
  })

  test('structural mode never transmits the values that were typed', async ({
    page,
  }) => {
    await gotoFresh(page, { consent: grantedConsent('structural') })
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    const ingest = trackIngest(page)

    const secret = 'SUPERSECRET48250'
    await clickCell(page, 0, 0)
    await typeInCell(page, secret)
    await typeInCell(page, '=A1&" derived"')
    await page.waitForTimeout(6000)

    const sent = ingest.bodies().join('\n')
    expect(sent.length, 'nothing was sent, so this proves nothing').toBeGreaterThan(0)
    expect(sent, 'the typed value was transmitted verbatim').not.toContain(secret)
    // The formula's structure is kept on purpose: it is what mining reads.
    expect(sent).toContain('A1')
  })

  test('structural mode never transmits the sheet name, in payload or context', async ({
    page,
  }) => {
    await gotoFresh(page, { consent: grantedConsent('structural') })
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    const ingest = trackIngest(page)

    // A sheet name is user content: "Payroll Q3" says as much as a cell does.
    // It rides in every envelope's context, so a leak here is a leak on
    // every single event, and it would expose the hash of a known value.
    const sheetName = 'PayrollQ3Confidential'
    await page.locator('.sheet-tab').first().dblclick()
    await page.keyboard.press('ControlOrMeta+a')
    await page.keyboard.type(sheetName)
    await page.keyboard.press('Enter')

    await clickCell(page, 0, 0)
    await typeInCell(page, '123')
    await page.waitForTimeout(6000)

    const sent = ingest.bodies().join('\n')
    expect(sent.length, 'nothing was sent, so this proves nothing').toBeGreaterThan(0)
    expect(sent, 'the sheet name was transmitted in clear').not.toContain(sheetName)
  })
})

test('the transparency page lists the live action vocabulary', async ({ page }) => {
  await gotoFresh(page, { consent: grantedConsent('structural') })
  await page.goto('/transparency')
  const main = page.locator('body')
  await expect(main).toContainText('cell.edit', { timeout: 30_000 })
  await expect(main).toContainText('range.paste')
  await expect(main).toContainText('capture.pause')
  // The mode in force is shown, so the page answers "what is happening now".
  await expect(main).toContainText(/structural/i)
})
