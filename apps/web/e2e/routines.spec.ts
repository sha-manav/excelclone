/**
 * M6 acceptance: a mined routine previews honestly and runs for real.
 *
 * The server is stubbed — its own suite covers it — but the *routine body* is
 * not. It is the exact JSON the miner produces, so this drives the real
 * engine through the real sandbox and the real apply path. A preview that
 * agreed with a mocked engine would prove nothing; the whole claim of the
 * panel is that what it shows is what will happen.
 */

import { expect, test, type Page, type Route } from '@playwright/test'

const HEADER_W = 46
const HEADER_H = 24
const COL_W = 100
const ROW_H = 24
/** A row that stays on screen even when the toolbar wraps to two lines. */
const SEED_ROW = 5

/**
 * A routine exactly as `gridline-miner` emits it: two formulas recorded at
 * row 5, anchored there, so running anywhere else has to shift both the
 * addresses and the references inside them.
 */
const LEDGER_ROUTINE = {
  id: 'rt_ledger',
  summary: 'Enter 2 formulas — repeated 30 times in a row, about 6 min of work',
  anchor: 'E5',
  actions: [
    {
      action: 'cell_edit',
      sheet: '<routine>',
      addr: { row: 4, col: 4 },
      input: '=SUM(B5:D5)',
    },
    {
      action: 'cell_edit',
      sheet: '<routine>',
      addr: { row: 4, col: 5 },
      input: '=E5*0.2',
    },
  ],
  requires: [],
  support: 30,
  estimated_minutes_saved: 5.7,
  kind: 'loop',
}

/** The same routine, but with a value the log could only hash. */
const PARTIAL_ROUTINE = {
  ...LEDGER_ROUTINE,
  id: 'rt_partial',
  summary: 'Enter 1 formula, type 1 value — repeated 8 times in a row',
  actions: [LEDGER_ROUTINE.actions[0]],
  requires: [{ row_offset: 0, col_offset: -3, kind: 'number' }],
}

/**
 * Wrap a routine the way the server stores it: the macro lives in `body`,
 * with the summary and the estimate alongside as columns. The panel reads
 * records, not bare routines.
 */
function record(routine: typeof LEDGER_ROUTINE) {
  return {
    id: routine.id,
    workbook_id: 'wb_test',
    summary: routine.summary,
    body: routine,
    estimated_minutes_saved: routine.estimated_minutes_saved,
    support: routine.support,
    status: 'proposed',
    created_at: '2024-01-01T00:00:00Z',
  }
}

interface StubState {
  routines: unknown[]
  feedback: { id: string; status: string }[]
}

async function stubServer(page: Page, state: StubState) {
  await page.route('**/v1/**', async (route: Route) => {
    const url = route.request().url()
    const json = (body: unknown) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(body),
      })

    if (url.includes('/v1/consent/me')) return json({ mode: null, captures: false })
    if (url.includes('/v1/routines/') && url.includes('/feedback')) {
      const id = /\/v1\/routines\/([^/]+)\/feedback/.exec(url)?.[1] ?? ''
      const status = JSON.parse(route.request().postData() ?? '{}').status
      state.feedback.push({ id, status })
      return json({ id, status })
    }
    if (url.includes('/v1/routines')) return json(state.routines)
    return json({ accepted: 0, duplicates: 0, rejected: 0, warnings: [] })
  })
}

async function cellPoint(page: Page, row: number, col: number) {
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  return {
    x: box.x + HEADER_W + (col + 0.5) * COL_W,
    y: box.y + HEADER_H + (row + 0.5) * ROW_H,
  }
}

async function clickCell(page: Page, row: number, col: number) {
  const p = await cellPoint(page, row, col)
  await page.mouse.click(p.x, p.y)
}

async function typeInCell(page: Page, text: string) {
  await page.keyboard.type(text)
  await page.keyboard.press('Enter')
}

async function snapshot(page: Page) {
  const text = await page.evaluate(() => {
    const w = window as unknown as { __gridline__?: { stateSnapshot(): string } }
    return w.__gridline__?.stateSnapshot() ?? ''
  })
  return text ? JSON.parse(text) : null
}

/**
 * Type the three inputs the ledger routine sums, on the given row.
 *
 * Rows are kept low deliberately: the toolbar wraps to two lines at narrow
 * widths, which shortens the grid, and a click aimed at row 20 can land
 * outside the canvas entirely.
 */
async function seedRow(page: Page, row: number) {
  await clickCell(page, row, 1)
  for (const v of ['10', '20', '30']) {
    await page.keyboard.type(v)
    await page.keyboard.press('Tab')
  }
}

async function open(page: Page, state: StubState) {
  await stubServer(page, state)
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  await page.getByTestId('consent-decline').click()
  await expect(page.getByTestId('consent-modal')).toHaveCount(0)
  page.on('pageerror', (e) => {
    throw e
  })
}

test('a routine previews against the current selection before it runs', async ({
  page,
}) => {
  const state: StubState = { routines: [record(LEDGER_ROUTINE)], feedback: [] }
  await open(page, state)
  await seedRow(page, SEED_ROW)
  await clickCell(page, SEED_ROW, 4)

  await page.getByRole('button', { name: 'Routines' }).click()
  const panel = page.getByRole('dialog', { name: 'Routines' })
  await expect(panel).toBeVisible()
  await expect(panel.getByText(/Enter 2 formulas/)).toBeVisible()

  await panel.getByRole('button', { name: /Preview 2 changes/ }).click()
  // The preview is computed by the engine, so it knows E6 will be 60 —
  // which is only true if the routine was shifted from its anchor at E5.
  await expect(panel.locator('.routine__diff')).toContainText('E6');
  await expect(panel.locator('.routine__after').first()).toHaveText('60')
  await expect(panel.locator('.routine__after').nth(1)).toHaveText('12')

  // Nothing has happened to the workbook yet.
  expect((await snapshot(page)).sheets[0].cells.E6).toBeUndefined()
})

test('running a routine applies exactly what it previewed, in one undo step', async ({
  page,
}) => {
  const state: StubState = { routines: [record(LEDGER_ROUTINE)], feedback: [] }
  await open(page, state)
  await seedRow(page, SEED_ROW)
  await clickCell(page, SEED_ROW, 4)

  await page.getByRole('button', { name: 'Routines' }).click()
  const panel = page.getByRole('dialog', { name: 'Routines' })
  await panel.getByRole('button', { name: /Run here/ }).click()

  await expect
    .poll(async () => (await snapshot(page))?.sheets[0]?.cells?.E6?.value, {
      timeout: 5000,
    })
    .toBe('60')
  const after = await snapshot(page)
  expect(after.sheets[0].cells.F6.value).toBe('12')
  // Shifted, not copied: the formula points at this row.
  expect(after.sheets[0].cells.E6.input).toBe('=SUM(B6:D6)')

  // One gesture, one undo.
  await page.keyboard.press('Control+z')
  await expect
    .poll(async () => (await snapshot(page))?.sheets[0]?.cells?.E6)
    .toBeUndefined()
  expect((await snapshot(page)).sheets[0].cells.F6).toBeUndefined()
})

test('the same routine run somewhere else lands somewhere else', async ({ page }) => {
  const state: StubState = { routines: [record(LEDGER_ROUTINE)], feedback: [] }
  await open(page, state)
  await seedRow(page, 2)
  await clickCell(page, 2, 4)

  await page.getByRole('button', { name: 'Routines' }).click()
  await page
    .getByRole('dialog', { name: 'Routines' })
    .getByRole('button', { name: /Run here/ })
    .click()

  await expect
    .poll(async () => (await snapshot(page))?.sheets[0]?.cells?.E3?.value, {
      timeout: 5000,
    })
    .toBe('60')
  expect((await snapshot(page)).sheets[0].cells.E3.input).toBe('=SUM(B3:D3)')
})

test('a routine that would change nothing here says so and cannot be run', async ({
  page,
}) => {
  // No inputs seeded, and the target cells are already empty — the formulas
  // would write 0, which *is* a change. So run it once first, then re-open:
  // the second run would change nothing.
  const state: StubState = { routines: [record(LEDGER_ROUTINE)], feedback: [] }
  await open(page, state)
  await seedRow(page, SEED_ROW)
  await clickCell(page, SEED_ROW, 4)
  await page.getByRole('button', { name: 'Routines' }).click()
  const panel = page.getByRole('dialog', { name: 'Routines' })
  await panel.getByRole('button', { name: /Run here/ }).click()
  await expect
    .poll(async () => (await snapshot(page))?.sheets[0]?.cells?.E6?.value, {
      timeout: 5000,
    })
    .toBe('60')

  await expect(panel.getByText(/Nothing would change at E6/)).toBeVisible()
  await expect(panel.getByRole('button', { name: /Run here/ })).toBeDisabled()
})

test('a partial routine names the values it cannot supply', async ({ page }) => {
  const state: StubState = { routines: [record(PARTIAL_ROUTINE)], feedback: [] }
  await open(page, state)
  await seedRow(page, SEED_ROW)
  await clickCell(page, SEED_ROW, 4)

  await page.getByRole('button', { name: 'Routines' }).click()
  const panel = page.getByRole('dialog', { name: 'Routines' })
  // Offset (0, -3) from E6 is B6.
  await expect(panel.locator('.routine__partial')).toContainText('number at B6')
  await expect(panel.getByRole('button', { name: /Run here/ })).toBeEnabled()
})

test('dismissing a routine removes it and tells the server', async ({ page }) => {
  const state: StubState = { routines: [record(LEDGER_ROUTINE)], feedback: [] }
  await open(page, state)
  await page.getByRole('button', { name: 'Routines' }).click()
  const panel = page.getByRole('dialog', { name: 'Routines' })
  await panel.getByRole('button', { name: 'Dismiss' }).click()

  await expect(panel.getByText(/Enter 2 formulas/)).toHaveCount(0)
  await expect.poll(() => state.feedback).toEqual([
    { id: 'rt_ledger', status: 'dismissed' },
  ])
})

test('running a routine reports it as accepted', async ({ page }) => {
  const state: StubState = { routines: [record(LEDGER_ROUTINE)], feedback: [] }
  await open(page, state)
  await seedRow(page, SEED_ROW)
  await clickCell(page, SEED_ROW, 4)
  await page.getByRole('button', { name: 'Routines' }).click()
  await page
    .getByRole('dialog', { name: 'Routines' })
    .getByRole('button', { name: /Run here/ })
    .click()

  await expect
    .poll(() => state.feedback.map((f) => f.status))
    .toEqual(['accepted'])
})

test('an empty panel explains itself rather than showing nothing', async ({ page }) => {
  const state: StubState = { routines: [], feedback: [] }
  await open(page, state)
  await page.getByRole('button', { name: 'Routines' }).click()
  await expect(
    page.getByRole('dialog', { name: 'Routines' }).getByText(/Nothing yet/),
  ).toBeVisible()
})

test('a server that is down does not break the spreadsheet', async ({ page }) => {
  await page.route('**/v1/**', async (route: Route) => {
    if (route.request().url().includes('/v1/consent/me')) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ mode: null, captures: false }),
      })
    }
    return route.fulfill({ status: 500, body: 'boom' })
  })
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  await page.getByTestId('consent-decline').click()

  await page.getByRole('button', { name: 'Routines' }).click()
  await expect(page.getByRole('dialog', { name: 'Routines' })).toBeVisible()
  // A suggestion box that cannot reach the server means no suggestions, not
  // an error toast over the grid.
  await expect(page.locator('.error-toast')).toHaveCount(0)

  // ...and the spreadsheet still works.
  await clickCell(page, 0, 0)
  await typeInCell(page, '=1+1')
  await expect
    .poll(async () => (await snapshot(page))?.sheets[0]?.cells?.A1?.value)
    .toBe('2')
})
