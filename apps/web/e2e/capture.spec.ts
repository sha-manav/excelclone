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
async function stubApi(page: Page, consentBody?: Record<string, unknown>) {
  await page.route('**/v1/**', async (route) => {
    const url = route.request().url()
    if (url.includes('/v1/consent/me')) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(consentBody ?? { mode: null, captures: false }),
      })
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ accepted: 0, duplicates: 0, rejected: 0, warnings: [] }),
    })
  })
}

async function gotoFresh(
  page: Page,
  opts: { consent?: string; consentBody?: Record<string, unknown> } = {},
) {
  await stubApi(page, opts.consentBody)
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

test('envelopes carry the actor id the server authenticated, not a local one', async ({
  page,
}) => {
  // The client mints a local id so capture works before the first response,
  // but the server rejects any envelope whose actor_id is not the
  // authenticated one — that check is what stops a client writing into
  // someone else's log. If the client does not converge on the server's
  // answer, every single event is refused and capture silently does nothing.
  const serverActor = 'u_server_side_id'
  await gotoFresh(page, {
    consent: grantedConsent('structural'),
    consentBody: {
      actor_id: serverActor,
      mode: 'structural',
      captures: true,
      consent_text_version: '1',
    },
  })
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  const ingest = trackIngest(page)

  await clickCell(page, 0, 0)
  await typeInCell(page, 'anything')
  await page.waitForTimeout(6000)

  const bodies = ingest.bodies()
  expect(bodies.length, 'nothing was sent, so this proves nothing').toBeGreaterThan(0)
  for (const body of bodies) {
    const batch = JSON.parse(body) as { events: { actor_id: string }[] }
    for (const e of batch.events) {
      expect(e.actor_id, 'envelope used a client-invented actor id').toBe(serverActor)
    }
  }
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

test('every action in a batch is captured, not just the first', async ({ page, context }) => {
  // A batched gesture — an external paste is four cell edits in one step —
  // must reach the log as four events. One would make the log replay to a
  // different workbook than the user is looking at, which is the one thing
  // the log is for.
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  await gotoFresh(page, { consent: grantedConsent('structural') })
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  const ingest = trackIngest(page)

  await page.evaluate(() => navigator.clipboard.writeText('10\t20\n30\t40\n'))
  await clickCell(page, 0, 0)
  await page.keyboard.press('Control+v')
  await page.waitForTimeout(6000)

  const edits = ingest
    .bodies()
    .flatMap((b) => (JSON.parse(b) as { events: { action: string }[] }).events)
    .filter((e) => e.action === 'cell.edit')
  expect(edits.length, 'the batch was captured as fewer events than it had').toBe(4)
})

test.describe('a rejected batch is visible', () => {
  /**
   * The failure this exists for: a blank auth token.
   *
   * Every ingest answers 401, a 401 is not retryable, so the queue discards
   * the batch — and the app went on reporting "capturing, 0 waiting" while
   * one hundred percent of events were being thrown away. There was no
   * backlog to notice and no error anywhere in the UI. The only way to find
   * out was to query the database.
   */
  async function stubRejectingApi(page: Page) {
    await page.route('**/v1/**', async (route) => {
      const url = route.request().url()
      if (url.includes('/v1/consent/me')) {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ mode: 'full', captures: true }),
        })
      }
      return route.fulfill({
        status: 401,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'missing or unknown bearer token' }),
      })
    })
  }

  test('the chip stops claiming to capture when the server refuses', async ({ page }) => {
    await stubRejectingApi(page)
    await page.addInitScript(
      ({ key, consent }) => {
        window.localStorage.clear()
        window.localStorage.setItem(key, consent)
      },
      { key: CONSENT_KEY, consent: grantedConsent('full') },
    )
    await page.goto('/')
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })

    await clickCell(page, 0, 0)
    await typeInCell(page, '42')

    const chip = page.getByTestId('capture-chip')
    await expect(chip).toHaveAttribute('data-state', 'rejected', { timeout: 15_000 })
    await expect(chip).toContainText('not recording')
    await expect(page.getByTestId('capture-rejected')).toBeVisible()
  })

  test('the transparency page says so in words', async ({ page }) => {
    await stubRejectingApi(page)
    await page.addInitScript(
      ({ key, consent }) => {
        window.localStorage.clear()
        window.localStorage.setItem(key, consent)
      },
      { key: CONSENT_KEY, consent: grantedConsent('full') },
    )
    await page.goto('/')
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    await clickCell(page, 0, 0)
    await typeInCell(page, '42')
    await expect(page.getByTestId('capture-rejected')).toBeVisible({ timeout: 15_000 })

    await page.locator('.toolbar__link').click()
    const alert = page.getByTestId('transparency-rejected')
    await expect(alert).toBeVisible()
    await expect(alert).toContainText('Nothing is being recorded')
  })

  test('a healthy server leaves the chip alone', async ({ page }) => {
    // The other half: this must not fire on an ordinary session, or it becomes
    // one more warning nobody reads.
    await gotoFresh(page, { consent: grantedConsent('full') })
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    await clickCell(page, 0, 0)
    await typeInCell(page, '42')
    await page.waitForTimeout(6_000)

    await expect(page.getByTestId('capture-chip')).toHaveAttribute('data-state', 'capturing')
    await expect(page.getByTestId('capture-rejected')).toHaveCount(0)
  })
})

test.describe('the captured log', () => {
  /** The API, with a history the page can read back. */
  async function stubApiWithHistory(page: Page, events: unknown[]) {
    await page.route('**/v1/**', async (route) => {
      const url = route.request().url()
      if (url.includes('/v1/consent/me')) {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ mode: 'structural', captures: true }),
        })
      }
      if (url.includes('/v1/events/recent')) {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(events),
        })
      }
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ accepted: 0, duplicates: 0, rejected: 0, warnings: [] }),
      })
    })
    await page.addInitScript(
      ({ key, tokenKey, consent }) => {
        window.localStorage.clear()
        window.localStorage.setItem(tokenKey, 'test-token')
        window.localStorage.setItem(key, consent)
      },
      { key: CONSENT_KEY, tokenKey: TOKEN_KEY, consent: grantedConsent('structural') },
    )
    await page.goto('/')
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    await page.locator('.toolbar__link').click()
  }

  const storedEvent = (overrides: Record<string, unknown> = {}) => ({
    schema_version: 1,
    event_id: 'ev_1',
    session_id: 's_1',
    actor_id: 'u_1',
    workbook_id: 'wb_1',
    seq: 1,
    ts_ms: 1_700_000_000_000,
    action: 'cell.edit',
    payload: {
      addr: 'A1',
      input: { hash: '90020ebfd48797ad', len: 5, type: 'number' },
      is_formula: false,
    },
    context: { sheet: 'h', selection: 'A1', privacy_mode: 'structural' },
    client_version: '0.1.0',
    ...overrides,
  })

  test('shows the stored events, with redacted values shown as hashes', async ({ page }) => {
    await stubApiWithHistory(page, [storedEvent()])
    const rows = page.getByTestId('captured-log-row')
    await expect(rows).toHaveCount(1)
    await expect(rows.first()).toContainText('cell.edit')
    await expect(rows.first()).toContainText('addr=A1')
    // The literal must not appear, and the thing that replaced it must.
    await expect(rows.first()).toContainText('number:5')
    await expect(rows.first()).not.toContainText('48250')
  })

  test('an empty history says so rather than showing nothing', async ({ page }) => {
    // The state that used to be indistinguishable from a working pipeline.
    await stubApiWithHistory(page, [])
    await expect(page.getByTestId('captured-log-empty')).toBeVisible()
  })

  test('an unreachable server is reported, not rendered as an empty log', async ({ page }) => {
    await page.route('**/v1/events/recent*', (route) => route.abort())
    await page.route('**/v1/consent/me', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ mode: 'structural', captures: true }),
      }),
    )
    await page.route('**/v1/events', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: '{}' }),
    )
    await page.addInitScript(
      ({ key, consent }) => {
        window.localStorage.clear()
        window.localStorage.setItem(key, consent)
      },
      { key: CONSENT_KEY, consent: grantedConsent('structural') },
    )
    await page.goto('/')
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
    await page.locator('.toolbar__link').click()

    await expect(page.getByTestId('captured-log-error')).toBeVisible()
    await expect(page.getByTestId('captured-log-empty')).toHaveCount(0)
  })
})

test.describe('a standalone build', () => {
  /**
   * The published site has no backend, and that is a promise about behaviour,
   * not just a missing button: a link sent to a friend must not record what
   * they type, and a consent notice offering a choice that cannot take effect
   * would be worse than no notice at all.
   *
   * This drives the real production bundle rather than the dev server, because
   * the flag is inlined at build time and only the built artefact can show
   * what was inlined.
   */
  test.skip(
    !process.env.GRIDLINE_STANDALONE_URL,
    'set GRIDLINE_STANDALONE_URL to a served standalone build',
  )

  test('shows no capture controls and talks to no server', async ({ page }) => {
    const calls: string[] = []
    page.on('request', (r) => {
      if (r.url().includes('/v1/')) calls.push(r.url())
    })

    await page.goto(process.env.GRIDLINE_STANDALONE_URL!)
    await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })

    await expect(page.getByTestId('consent-modal')).toHaveCount(0)
    await expect(page.getByTestId('capture-chip')).toHaveCount(0)
    await expect(page.locator('.toolbar__link')).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Routines' })).toHaveCount(0)

    await clickCell(page, 0, 0)
    await typeInCell(page, '42')
    await page.waitForTimeout(6_000)

    expect(calls, `standalone build called the API: ${calls.join(', ')}`).toHaveLength(0)
  })
})
