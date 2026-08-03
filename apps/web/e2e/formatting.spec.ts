/**
 * M5 acceptance: formatting, merges, sort, filter, find/replace and the
 * import-warnings drawer, driven through the real app.
 *
 * The grid is a canvas, so there are no DOM nodes to assert against for what
 * a cell *looks* like. Two techniques stand in for that:
 *
 *   - sample the canvas pixel where the formatting should land, which is the
 *     only way to catch "the action applied but the painter ignored it";
 *   - round-trip through the engine's own state snapshot, which catches the
 *     opposite failure — the grid looks right but nothing was recorded.
 *
 * Both matter. M3's bug was a grid that never painted while every unit test
 * passed; a suite that only checks engine state would have shipped it.
 */

import { expect, test, type Page } from '@playwright/test'

const HEADER_W = 46
const HEADER_H = 24
const COL_W = 100
const ROW_H = 24

async function cellPoint(page: Page, row: number, col: number, dx = 0.5, dy = 0.5) {
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  return {
    x: box.x + HEADER_W + (col + dx) * COL_W,
    y: box.y + HEADER_H + (row + dy) * ROW_H,
  }
}

async function clickCell(page: Page, row: number, col: number) {
  const p = await cellPoint(page, row, col)
  await page.mouse.click(p.x, p.y)
}

/** Drag-select from one cell to another. */
async function selectRange(page: Page, r0: number, c0: number, r1: number, c1: number) {
  const a = await cellPoint(page, r0, c0)
  const b = await cellPoint(page, r1, c1)
  await page.mouse.move(a.x, a.y)
  await page.mouse.down()
  await page.mouse.move(b.x, b.y, { steps: 6 })
  await page.mouse.up()
}

async function typeInCell(page: Page, text: string) {
  await page.keyboard.type(text)
  await page.keyboard.press('Enter')
}

/**
 * The colour of one device pixel of the grid canvas, as `r,g,b`.
 *
 * Read from the live canvas rather than a screenshot comparison: a golden
 * image would break on every font-rendering difference between machines,
 * which is noise, while a specific pixel's colour is exactly the claim.
 */
async function pixelAt(page: Page, row: number, col: number, dx = 0.5, dy = 0.5) {
  return page.evaluate(
    ({ row, col, dx, dy, HEADER_W, HEADER_H, COL_W, ROW_H }) => {
      const canvas = document.querySelector('canvas') as HTMLCanvasElement
      const ctx = canvas.getContext('2d')!
      const dpr = window.devicePixelRatio || 1
      const x = Math.round((HEADER_W + (col + dx) * COL_W) * dpr)
      const y = Math.round((HEADER_H + (row + dy) * ROW_H) * dpr)
      const d = ctx.getImageData(x, y, 1, 1).data
      return `${d[0]},${d[1]},${d[2]}`
    },
    { row, col, dx, dy, HEADER_W, HEADER_H, COL_W, ROW_H },
  )
}

/** The engine's deterministic snapshot, parsed. */
async function snapshot(page: Page) {
  const text = await page.evaluate(() => {
    const w = window as unknown as { __gridline__?: { stateSnapshot(): string } }
    return w.__gridline__?.stateSnapshot() ?? ''
  })
  return text ? JSON.parse(text) : null
}

test.beforeEach(async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (e) => errors.push(String(e)))
  page.on('console', (m) => {
    if (m.type() === 'error') errors.push(m.text())
  })
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  await page.getByTestId('consent-decline').click()
  await expect(page.getByTestId('consent-modal')).toHaveCount(0)
  // Any uncaught error during the test is a failure, not a warning: the last
  // two real bugs here were exceptions thrown inside a paint callback.
  page.on('pageerror', (e) => {
    throw e
  })
})

/* ---------------------------------------------------------------- toolbar */

test('bold applies, shows as pressed, and undoes', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'header')
  await clickCell(page, 0, 0)

  const bold = page.getByRole('button', { name: 'Bold' })
  await expect(bold).toHaveAttribute('aria-pressed', 'false')
  await bold.click()
  await expect(bold).toHaveAttribute('aria-pressed', 'true')

  const state = await snapshot(page)
  expect(state.sheets[0].formats.A1.bold).toBe(true)

  await page.keyboard.press('Control+z')
  await expect(bold).toHaveAttribute('aria-pressed', 'false')
  const after = await snapshot(page)
  expect(after.sheets[0].formats).toEqual({})
  // The contents are untouched by a formatting undo.
  expect(after.sheets[0].cells.A1.value).toBe('header')
})

test('a fill colour is actually painted', async ({ page }) => {
  await clickCell(page, 2, 2)
  const before = await pixelAt(page, 2, 2)
  expect(before).toBe('255,255,255')

  await page.getByRole('button', { name: 'Fill colour' }).click()
  await page.getByRole('button', { name: '#b3261e' }).click()

  await expect
    .poll(() => pixelAt(page, 2, 2), { timeout: 5000 })
    .toBe('179,38,30')
})

test('clearing formatting keeps the contents', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '42')
  await clickCell(page, 0, 0)
  await page.getByRole('button', { name: 'Fill colour' }).click()
  await page.getByRole('button', { name: '#1e7e45' }).click()
  await expect.poll(() => pixelAt(page, 0, 0)).toBe('30,126,69')

  await page.getByRole('button', { name: 'Clear formatting' }).click()
  await expect.poll(() => pixelAt(page, 0, 0)).toBe('255,255,255')
  const state = await snapshot(page)
  expect(state.sheets[0].cells.A1.value).toBe('42')
})

test('a number format changes the display but not the value', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '0.5')
  await clickCell(page, 1, 0)
  await typeInCell(page, '=A1*2')

  await clickCell(page, 0, 0)
  await page.getByLabel('Number format').selectOption('0.00%')

  const state = await snapshot(page)
  // The snapshot records the raw value; the format lives beside it.
  expect(state.sheets[0].cells.A1.value).toBe('0.5')
  expect(state.sheets[0].formats.A1.number_format).toBe('0.00%')
  // ...and the dependent formula still sees 0.5, not 50.
  expect(state.sheets[0].cells.A2.value).toBe('1')
})

test('borders draw only on the perimeter for an outline', async ({ page }) => {
  await selectRange(page, 1, 1, 3, 3)
  await page.getByLabel('Borders').selectOption('outline')

  const state = await snapshot(page)
  expect(state.sheets[0].formats.B2.borders).toMatchObject({
    top: true,
    left: true,
    bottom: false,
    right: false,
  })
  // The middle cell of the block gets no edges at all.
  expect(state.sheets[0].formats.C3).toBeUndefined()
})

/* ----------------------------------------------------------------- merges */

test('merging paints one block and selecting it selects the whole range', async ({
  page,
}) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'wide heading')
  await selectRange(page, 0, 0, 0, 2)

  // Compare the A1/B1 boundary against the middle of B1 rather than against
  // a fixed colour: the selection wash tints both, so "no gridline here"
  // means "this pixel matches its neighbourhood", not "this pixel is white".
  const boundary = () => pixelAt(page, 0, 1, 0, 0.5)
  const interior = () => pixelAt(page, 0, 1, 0.5, 0.5)
  expect(await boundary()).not.toBe(await interior())

  await page.getByRole('button', { name: 'Merge cells' }).click()
  await expect.poll(async () => await boundary(), { timeout: 5000 }).toBe(
    await interior(),
  )

  // Clicking the covered part of the block selects the block, not C1.
  await clickCell(page, 0, 2)
  await expect(page.locator('.toolbar__status')).toHaveText('A1:C1')
  await expect(page.locator('.formula-bar__address')).toHaveValue('A1')

  await page.getByRole('button', { name: 'Unmerge cells' }).click()
  await expect
    .poll(async () => await boundary())
    .not.toBe(await interior())
})

/* ----------------------------------------------------------- context menu */

test('the context menu inserts and deletes rows', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'first')
  await typeInCell(page, 'second')

  const p = await cellPoint(page, 0, 0)
  await page.mouse.click(p.x, p.y, { button: 'right' })
  await page.getByRole('menuitem', { name: 'Insert row' }).click()

  let state = await snapshot(page)
  expect(state.sheets[0].cells.A1).toBeUndefined()
  expect(state.sheets[0].cells.A2.value).toBe('first')

  await page.mouse.click(p.x, p.y, { button: 'right' })
  await page.getByRole('menuitem', { name: 'Delete row' }).click()
  state = await snapshot(page)
  expect(state.sheets[0].cells.A1.value).toBe('first')
})

test('a menu item that cannot act says why instead of doing nothing', async ({
  page,
}) => {
  await clickCell(page, 0, 0)
  const p = await cellPoint(page, 0, 0)
  await page.mouse.click(p.x, p.y, { button: 'right' })
  const merge = page.getByRole('menuitem', { name: 'Merge cells' })
  await expect(merge).toHaveAttribute('aria-disabled', 'true')
  await expect(merge).toHaveAttribute('title', /more than one cell/)
})

/* ------------------------------------------------------------ sort/filter */

test('a custom sort orders by the chosen column', async ({ page }) => {
  await clickCell(page, 0, 0)
  for (const name of ['name', 'Zoe', 'Ada', 'Mia']) await typeInCell(page, name)

  await selectRange(page, 0, 0, 3, 0)
  const p = await cellPoint(page, 1, 0)
  await page.mouse.click(p.x, p.y, { button: 'right' })
  await page.getByRole('menuitem', { name: 'Custom sort…' }).click()

  await expect(page.getByRole('dialog', { name: 'Sort' })).toBeVisible()
  await page.getByRole('button', { name: 'Sort', exact: true }).click()

  const state = await snapshot(page)
  expect(state.sheets[0].cells.A1.value).toBe('name')
  expect(state.sheets[0].cells.A2.value).toBe('Ada')
  expect(state.sheets[0].cells.A4.value).toBe('Zoe')
})

test('a filter hides the rows it excludes', async ({ page }) => {
  await clickCell(page, 0, 0)
  for (const v of ['tier', 'pro', 'basic', 'pro']) await typeInCell(page, v)

  await selectRange(page, 0, 0, 3, 0)
  const p = await cellPoint(page, 1, 0)
  await page.mouse.click(p.x, p.y, { button: 'right' })
  await page.getByRole('menuitem', { name: 'Filter…' }).click()

  const dialog = page.getByRole('dialog', { name: 'Filter' })
  await expect(dialog).toBeVisible()
  await dialog.getByRole('button', { name: 'Select none' }).click()
  await dialog.getByLabel('pro').check()
  await dialog.getByRole('button', { name: 'Apply' }).click()

  const state = await snapshot(page)
  // Row index 2 holds "basic" and is the only one hidden.
  expect(state.sheets[0].hidden_rows).toEqual([2])
})

/* ------------------------------------------------------- find and replace */

test('find highlights matches and replace all is one undo step', async ({ page }) => {
  await clickCell(page, 0, 0)
  for (const v of ['draft', 'final', 'draft']) await typeInCell(page, v)

  await page.keyboard.press('Control+f')
  const panel = page.getByRole('search', { name: 'Find and replace' })
  await expect(panel).toBeVisible()
  await panel.getByLabel('Find', { exact: true }).fill('draft')
  await expect(panel.locator('.find-panel__count')).toHaveText('1 of 2')

  await panel.getByLabel('Replace with').fill('issued')
  await panel.getByRole('button', { name: 'Replace all' }).click()

  let state = await snapshot(page)
  expect(state.sheets[0].cells.A1.value).toBe('issued')
  expect(state.sheets[0].cells.A3.value).toBe('issued')
  expect(state.sheets[0].cells.A2.value).toBe('final')

  // Both replacements come back together, because they were one action.
  await page.keyboard.press('Escape')
  await page.keyboard.press('Control+z')
  state = await snapshot(page)
  expect(state.sheets[0].cells.A1.value).toBe('draft')
  expect(state.sheets[0].cells.A3.value).toBe('draft')
})

test('find matches formula source, not the number a formula shows', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '300')
  await typeInCell(page, '=A1')

  await page.keyboard.press('Control+f')
  const panel = page.getByRole('search', { name: 'Find and replace' })
  // "300" is A1's content and A2's displayed result; only A1 is a match.
  await panel.getByLabel('Find', { exact: true }).fill('300')
  await expect(panel.locator('.find-panel__count')).toHaveText('1 of 1')
})

/* --------------------------------------------------------------- autoscroll */

test('dragging past the bottom edge keeps extending the selection', async ({ page }) => {
  const start = await cellPoint(page, 0, 0)
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('no canvas')

  await page.mouse.move(start.x, start.y)
  await page.mouse.down()
  // Park the pointer below the grid and let the autoscroll loop run.
  await page.mouse.move(start.x, box.y + box.height + 40, { steps: 4 })
  await expect
    .poll(
      async () => {
        const text = await page.locator('.toolbar__status').textContent()
        const m = /A1:A(\d+)/.exec(text ?? '')
        return m ? Number(m[1]) : 0
      },
      { timeout: 5000 },
    )
    .toBeGreaterThan(30)
  await page.mouse.up()
})

/* ------------------------------------------------------------------ files */

test('saving produces a workbook that opens again with its formatting', async ({
  page,
}) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'total')
  await clickCell(page, 0, 0)
  await page.getByRole('button', { name: 'Bold' }).click()

  const download = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Save .xlsx' }).click()
  const file = await download
  const path = await file.path()

  await page.getByLabel('Open a workbook').setInputFiles(path)
  await expect
    .poll(async () => {
      const s = await snapshot(page)
      return s?.sheets[0]?.formats?.A1?.bold ?? false
    }, { timeout: 10_000 })
    .toBe(true)
  const state = await snapshot(page)
  expect(state.sheets[0].cells.A1.value).toBe('total')

  // A file we wrote ourselves has nothing the importer cannot model, so the
  // notes badge must stay away rather than appearing with an empty drawer.
  await expect(page.locator('.warn-badge')).toHaveCount(0)
})

test('opening a file resets the undo history', async ({ page }) => {
  // Import replays the file through apply(); without an explicit reset the
  // first Ctrl+Z after opening would un-type a cell the user never typed.
  await clickCell(page, 0, 0)
  await typeInCell(page, 'before')
  const download = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Save .xlsx' }).click()
  const path = await (await download).path()

  await page.getByLabel('Open a workbook').setInputFiles(path)
  await expect
    .poll(async () => (await snapshot(page))?.sheets[0]?.cells?.A1?.value, {
      timeout: 10_000,
    })
    .toBe('before')
  await expect(page.getByRole('button', { name: 'Undo' })).toBeDisabled()
})

/* ------------------------------------------------------- fill handle polish */

test('double-clicking the fill handle follows the neighbouring run', async ({
  page,
}) => {
  // A column of values, and one formula beside the first of them.
  await clickCell(page, 0, 0)
  for (const v of ['10', '20', '30', '40']) await typeInCell(page, v)
  await clickCell(page, 0, 1)
  await typeInCell(page, '=A1*2')

  await clickCell(page, 0, 1)
  const p = await cellPoint(page, 0, 1, 1, 1)
  await page.mouse.dblclick(p.x - 2, p.y - 2)

  const state = await snapshot(page)
  // Filled exactly as far as column A runs, and no further.
  expect(state.sheets[0].cells.B4.value).toBe('80')
  expect(state.sheets[0].cells.B5).toBeUndefined()
})

test('double-clicking a column border sizes it to its contents', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'a very long value indeed that overflows the column')

  const box = (await page.locator('canvas').boundingBox())!
  // 250px in is column C to start with; the assertion is that autofit moves
  // that boundary, so it has to be checked before and after.
  const at250 = async () => {
    await page.mouse.click(box.x + HEADER_W + 250, box.y + HEADER_H + ROW_H / 2)
    // The name box is an input now, so its address is a value, not text.
    return page.locator('.formula-bar__address').inputValue()
  }
  expect(await at250()).toBe('C1')

  await page.mouse.dblclick(box.x + HEADER_W + COL_W, box.y + HEADER_H / 2)
  // Column A now swallows the first 250px, so the same click lands in A.
  await expect.poll(at250, { timeout: 5000 }).toBe('A1')
})
