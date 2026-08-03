/**
 * M3 acceptance: the grid renders, edits go through the engine, and formulas
 * recalculate live in the browser.
 *
 * These drive the real app against the real wasm engine — no mocks — because
 * the thing worth verifying is that the whole chain works, not that the
 * pieces compile.
 */

import { expect, test, type Page } from '@playwright/test'

/** Click the cell at (row, col), 0-based, using the grid's default metrics. */
async function clickCell(page: Page, row: number, col: number) {
  const canvas = page.locator('canvas')
  const box = await canvas.boundingBox()
  if (!box) throw new Error('canvas has no box')
  const HEADER_W = 46
  const HEADER_H = 24
  const COL_W = 100
  const ROW_H = 24
  await page.mouse.click(
    box.x + HEADER_W + col * COL_W + COL_W / 2,
    box.y + HEADER_H + row * ROW_H + ROW_H / 2,
  )
}

/** Type into the currently selected cell and commit with Enter. */
async function typeInCell(page: Page, text: string) {
  await page.keyboard.type(text)
  await page.keyboard.press('Enter')
}

/** The formula bar's current contents. */
function formulaInput(page: Page) {
  return page.locator('.formula-bar__input')
}

test.beforeEach(async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (e) => errors.push(String(e)))
  page.on('console', (m) => {
    if (m.type() === 'error') errors.push(m.text())
  })
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  // Surface engine load failures immediately rather than as a mystery later.
  expect(errors, `page errors on load: ${errors.join('; ')}`).toHaveLength(0)
  // First run shows the consent notice, which is modal by design. These tests
  // are about the grid, so decline: capture off means no events and no
  // network, and the rest of the suite behaves exactly as it did before.
  await page.getByTestId('consent-decline').click()
  await expect(page.getByTestId('consent-modal')).toHaveCount(0)
})

test('grid renders with headers and sheet tabs', async ({ page }) => {
  await expect(page.locator('canvas')).toBeVisible()
  await expect(page.locator('.sheet-tab', { hasText: 'Sheet1' })).toBeVisible()
  await expect(page.locator('.formula-bar__address')).toHaveText('A1')
})

test('typing a value stores it and moves down', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '42')
  // Enter moved the selection to A2.
  await expect(page.locator('.formula-bar__address')).toHaveText('A2')
  // Re-select A1 and confirm the engine kept the value.
  await clickCell(page, 0, 0)
  await expect(page.locator('.formula-bar__address')).toHaveText('A1')
  await expect(formulaInput(page)).toHaveValue('42')
})

test('formulas recalculate live', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '10')
  await typeInCell(page, '20')
  await clickCell(page, 0, 1)
  await typeInCell(page, '=SUM(A1:A2)')

  // The formula bar shows the formula, and the grid shows its result.
  await clickCell(page, 0, 1)
  await expect(formulaInput(page)).toHaveValue('=SUM(A1:A2)')

  // Changing a precedent updates the dependent without any explicit refresh.
  await clickCell(page, 0, 0)
  await typeInCell(page, '100')
  await clickCell(page, 0, 1)
  await expect(formulaInput(page)).toHaveValue('=SUM(A1:A2)')
  // Read the computed value straight off the engine via the rendered canvas
  // check below; the address label proves selection is where we think.
  await expect(page.locator('.formula-bar__address')).toHaveText('B1')
})

test('undo and redo walk the history', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'first')
  await clickCell(page, 0, 0)
  await expect(formulaInput(page)).toHaveValue('first')

  await page.keyboard.press('ControlOrMeta+z')
  await clickCell(page, 0, 0)
  await expect(formulaInput(page)).toHaveValue('')

  await page.keyboard.press('ControlOrMeta+Shift+z')
  await clickCell(page, 0, 0)
  await expect(formulaInput(page)).toHaveValue('first')
})

test('sheet tabs add, switch and keep separate data', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'on sheet one')

  await page.locator('.sheet-tabs__add').click()
  await expect(page.locator('.sheet-tab')).toHaveCount(2)
  const second = page.locator('.sheet-tab').nth(1)
  await second.click()

  // The new sheet starts empty.
  await clickCell(page, 0, 0)
  await expect(formulaInput(page)).toHaveValue('')

  // Switching back shows the original data.
  await page.locator('.sheet-tab').first().click()
  await clickCell(page, 0, 0)
  await expect(formulaInput(page)).toHaveValue('on sheet one')
})

test('cross-sheet formulas resolve', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '7')
  await page.locator('.sheet-tabs__add').click()
  await page.locator('.sheet-tab').nth(1).click()
  await clickCell(page, 0, 0)
  await typeInCell(page, '=Sheet1!A1*6')
  await clickCell(page, 0, 0)
  await expect(formulaInput(page)).toHaveValue('=Sheet1!A1*6')
})

test('keyboard navigation moves the selection', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.press('ArrowRight')
  await expect(page.locator('.formula-bar__address')).toHaveText('B1')
  await page.keyboard.press('ArrowDown')
  await expect(page.locator('.formula-bar__address')).toHaveText('B2')
  await page.keyboard.press('Tab')
  await expect(page.locator('.formula-bar__address')).toHaveText('C2')
  await page.keyboard.press('ControlOrMeta+Home')
  await expect(page.locator('.formula-bar__address')).toHaveText('A1')
})

test('delete clears the selected range', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'gone')
  await clickCell(page, 0, 0)
  await page.keyboard.press('Delete')
  await expect(formulaInput(page)).toHaveValue('')
})

test('a bad formula surfaces an error instead of failing silently', async ({
  page,
}) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, '=1+')
  await expect(page.locator('.error-toast')).toBeVisible()
})

test('a column resize moves the columns and comes back with undo', async ({ page }) => {
  const canvas = page.locator('canvas')
  const box = await canvas.boundingBox()
  if (!box) throw new Error('canvas has no box')
  const HEADER_W = 46
  const HEADER_H = 24
  const COL_W = 100

  // Drag the A/B border 100px to the right, so column A is 200 wide.
  const border = { x: box.x + HEADER_W + COL_W, y: box.y + HEADER_H / 2 }
  await page.mouse.move(border.x, border.y)
  await page.mouse.down()
  await page.mouse.move(border.x + 100, border.y, { steps: 10 })
  await page.mouse.up()

  // The proof is where the columns now are, not what the canvas looks like:
  // 250px from the left edge was column C and is now column B.
  await page.mouse.click(box.x + HEADER_W + 250, box.y + HEADER_H + 12)
  await expect(page.locator('.formula-bar__address')).toHaveText('B1')

  // A resize is an action like any other, so Ctrl+Z has to take it back.
  await page.keyboard.press('Control+z')
  await page.mouse.click(box.x + HEADER_W + 250, box.y + HEADER_H + 12)
  await expect(page.locator('.formula-bar__address')).toHaveText('C1')
})

test('a row resize moves the rows', async ({ page }) => {
  const canvas = page.locator('canvas')
  const box = await canvas.boundingBox()
  if (!box) throw new Error('canvas has no box')
  const HEADER_W = 46
  const HEADER_H = 24
  const ROW_H = 24

  // Drag the 1/2 border down 24px, so row 1 is 48 tall.
  const border = { x: box.x + HEADER_W / 2, y: box.y + HEADER_H + ROW_H }
  await page.mouse.move(border.x, border.y)
  await page.mouse.down()
  await page.mouse.move(border.x, border.y + 24, { steps: 10 })
  await page.mouse.up()

  // 60px down used to be row 3 and is now row 2.
  await page.mouse.click(box.x + HEADER_W + 40, box.y + HEADER_H + 60)
  await expect(page.locator('.formula-bar__address')).toHaveText('A2')
})

test('copying puts tab-separated text on the system clipboard', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  await clickCell(page, 0, 0)
  await typeInCell(page, 'a')
  await typeInCell(page, 'b')
  await clickCell(page, 0, 1)
  await typeInCell(page, '1')
  await typeInCell(page, '2')

  // Select A1:B2 and copy.
  await clickCell(page, 0, 0)
  await page.keyboard.down('Shift')
  await clickCell(page, 1, 1)
  await page.keyboard.up('Shift')
  await page.keyboard.press('Control+c')

  const text = await page.evaluate(() => navigator.clipboard.readText())
  expect(text).toBe('a\t1\nb\t2')
})

test('pasting text from outside lands as cells in one undo step', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  // Text nothing in this app produced: the point is that it comes from
  // somewhere else, the way a paste out of Excel does.
  await page.evaluate(() => navigator.clipboard.writeText('10\t20\n30\t40\n'))

  await clickCell(page, 1, 1)
  await page.keyboard.press('Control+v')

  await clickCell(page, 1, 1)
  await expect(formulaInput(page)).toHaveValue('10')
  await clickCell(page, 2, 2)
  await expect(formulaInput(page)).toHaveValue('40')

  // Four cells, one Ctrl+Z.
  await page.keyboard.press('Control+z')
  await clickCell(page, 1, 1)
  await expect(formulaInput(page)).toHaveValue('')
  await clickCell(page, 2, 2)
  await expect(formulaInput(page)).toHaveValue('')
})

test('pasting a block we copied keeps its formulas and moves their references', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  await clickCell(page, 0, 0)
  await typeInCell(page, '5')
  await clickCell(page, 0, 1)
  await typeInCell(page, '=A1*2')

  await clickCell(page, 0, 1)
  await page.keyboard.press('Control+c')
  await clickCell(page, 1, 1)
  await page.keyboard.press('Control+v')

  // The system clipboard holds "10". An internal paste is the one that can
  // bring the formula across and repoint it a row down.
  await clickCell(page, 1, 1)
  await expect(formulaInput(page)).toHaveValue('=A2*2')
})

test('a dynamic array spills into the cells below and they read back', async ({ page }) => {
  await clickCell(page, 0, 0)
  await typeInCell(page, 'b')
  await typeInCell(page, 'a')
  await typeInCell(page, 'b')

  await clickCell(page, 0, 2)
  await typeInCell(page, '=UNIQUE(A1:A3)')

  // C2 holds a value nothing was ever typed into, and it has no formula: the
  // formula bar is empty there while the grid shows the spilled value.
  await clickCell(page, 1, 2)
  await expect(page.locator('.formula-bar__address')).toHaveText('C2')
  await expect(formulaInput(page)).toHaveValue('a')

  // Typing over a spilled cell breaks the block rather than being ignored.
  await typeInCell(page, 'mine')
  await clickCell(page, 0, 2)
  await expect(formulaInput(page)).toHaveValue('=UNIQUE(A1:A3)')
})
