/**
 * Pointing mode: picking references off the grid while a formula is open.
 *
 * The gesture under test is the one everybody does without being taught —
 * type `=AVERAGE(`, drag across the numbers, close the bracket, press Enter.
 * Before this existed the drag committed the half-written formula instead, so
 * the app answered a perfectly ordinary action with `formula parse error:
 * unexpected end of formula`.
 *
 * The tests below also pin the *other* half of the rule, which is easier to
 * break: a formula that is already complete must still commit when you click
 * away, or "click somewhere else to finish" stops working and every formula
 * becomes a trap.
 */

import { expect, test, type Page } from '@playwright/test'

const HEADER_W = 46
const HEADER_H = 24
const COL_W = 100
const ROW_H = 24

async function cellPoint(page: Page, row: number, col: number) {
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  return {
    x: box.x + HEADER_W + col * COL_W + COL_W / 2,
    y: box.y + HEADER_H + row * ROW_H + ROW_H / 2,
  }
}

async function clickCell(page: Page, row: number, col: number) {
  const p = await cellPoint(page, row, col)
  await page.mouse.click(p.x, p.y)
}

async function dragCells(page: Page, from: [number, number], to: [number, number]) {
  const a = await cellPoint(page, from[0], from[1])
  const b = await cellPoint(page, to[0], to[1])
  await page.mouse.move(a.x, a.y)
  await page.mouse.down()
  await page.mouse.move(b.x, b.y, { steps: 10 })
  await page.mouse.up()
}

const editor = (page: Page) => page.locator('[data-testid=cell-editor]')
const address = (page: Page) => page.locator('.formula-bar__address')
const formula = (page: Page) => page.locator('.formula-bar__input')

/** What the engine holds for a cell, read through the formula bar. */
async function inputAt(page: Page, row: number, col: number) {
  await clickCell(page, row, col)
  return formula(page).inputValue()
}

/** Put 1, 3 and 4 in A1:A3 — enough for an average nobody has to compute. */
async function seed(page: Page) {
  for (const [row, text] of [
    [0, '1'],
    [1, '3'],
    [2, '4'],
  ] as const) {
    await clickCell(page, row, 0)
    await page.keyboard.type(text)
    await page.keyboard.press('Enter')
  }
}

test.beforeEach(async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (e) => errors.push(String(e)))
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  expect(errors, `page errors on load: ${errors.join('; ')}`).toHaveLength(0)
  await page.getByTestId('consent-decline').click()
  await expect(page.getByTestId('consent-modal')).toHaveCount(0)
})

test('dragging a range into an open function call writes the reference', async ({ page }) => {
  // The exact sequence from the bug report.
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=AVERAGE(')
  await dragCells(page, [0, 0], [2, 0])

  await expect(editor(page)).toHaveValue('=AVERAGE(A1:A3')
  await page.keyboard.type(')')
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 3, 0)).toBe('=AVERAGE(A1:A3)')
})

test('the caret stays where the reference ended, so typing continues the formula', async ({
  page,
}) => {
  // A controlled input jumps the caret to the end when its value is replaced
  // from outside. That is invisible here — until the closing bracket lands in
  // the wrong place.
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=SUM()')
  // Caret back inside the brackets, so what follows the pointed reference is
  // text that must survive — and must stay to the right of the caret.
  await page.keyboard.press('ArrowLeft')
  await clickCell(page, 0, 0)
  await expect(editor(page)).toHaveValue('=SUM(A1)')

  await page.keyboard.type('+1')
  // The caret jumping to the end would give `=SUM(A1)+1`, which is a different
  // formula that happens to look plausible.
  await expect(editor(page)).toHaveValue('=SUM(A1+1)')
})

test('clicking a single cell mid-formula inserts it rather than committing', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=')
  await clickCell(page, 0, 0)

  await expect(editor(page)).toHaveValue('=A1')
  // Still editing A4, not moved to A1.
  await expect(address(page)).toHaveValue('A4')
})

test('pointing twice replaces the reference instead of appending another', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=')
  await clickCell(page, 0, 0)
  await clickCell(page, 2, 0)

  await expect(editor(page)).toHaveValue('=A3')
})

test('an operator after a pointed reference starts a new one', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=')
  await clickCell(page, 0, 0)
  await page.keyboard.type('+')
  await clickCell(page, 1, 0)
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 3, 0)).toBe('=A1+A2')
})

test('a finished formula still commits when you click away', async ({ page }) => {
  // The rule that keeps pointing from swallowing every click: once the formula
  // is complete, nothing is expecting a reference, so a click means "go there".
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=SUM(A1:A3)')
  await clickCell(page, 5, 2)

  await expect(editor(page)).toHaveCount(0)
  await expect(address(page)).toHaveValue('C6')
  expect(await inputAt(page, 3, 0)).toBe('=SUM(A1:A3)')
})

test('a plain value still commits when you click away', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('hello')
  await clickCell(page, 3, 2)

  await expect(editor(page)).toHaveCount(0)
  expect(await inputAt(page, 0, 0)).toBe('hello')
})

test('pointing works from the formula bar as well as the cell', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await formula(page).click()
  await formula(page).fill('=SUM(')
  await dragCells(page, [0, 0], [2, 0])

  await expect(formula(page)).toHaveValue('=SUM(A1:A3')
})

test('Escape after pointing leaves the cell alone', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=AVERAGE(')
  await dragCells(page, [0, 0], [2, 0])
  await page.keyboard.press('Escape')

  await expect(editor(page)).toHaveCount(0)
  expect(await inputAt(page, 3, 0)).toBe('')
})
