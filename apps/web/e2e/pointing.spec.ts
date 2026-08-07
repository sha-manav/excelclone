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
import { openGrid } from './support'

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

/** The middle of a column header. */
async function colHeaderPoint(page: Page, col: number) {
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  return { x: box.x + HEADER_W + col * COL_W + COL_W / 2, y: box.y + HEADER_H / 2 }
}

/** The middle of a row header. */
async function rowHeaderPoint(page: Page, row: number) {
  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  return { x: box.x + HEADER_W / 2, y: box.y + HEADER_H + row * ROW_H + ROW_H / 2 }
}

async function clickColHeader(page: Page, col: number) {
  const p = await colHeaderPoint(page, col)
  await page.mouse.click(p.x, p.y)
}

async function clickRowHeader(page: Page, row: number) {
  const p = await rowHeaderPoint(page, row)
  await page.mouse.click(p.x, p.y)
}

async function dragColHeaders(page: Page, from: number, to: number) {
  const a = await colHeaderPoint(page, from)
  const b = await colHeaderPoint(page, to)
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

/**
 * A cell's computed value, from the engine's own snapshot.
 *
 * The formula bar shows the input; the grid is a canvas. This is the read-only
 * probe the rest of the end-to-end suite uses for the same reason.
 */
async function valueAt(page: Page, cell: string) {
  const state = await page.evaluate(() => {
    const w = window as unknown as { __gridline__?: { stateSnapshot(): string } }
    return w.__gridline__?.stateSnapshot() ?? ''
  })
  return JSON.parse(state).sheets[0].cells[cell]?.value ?? ''
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
  await openGrid(page)
  expect(errors, `page errors on load: ${errors.join('; ')}`).toHaveLength(0)
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

test('an arrow key picks the cell next to the one being edited', async ({ page }) => {
  // Excel's other way of naming a cell, and the one people who never reach
  // for the mouse use.
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=')
  await page.keyboard.press('ArrowUp')
  await expect(editor(page)).toHaveValue('=A3')

  await page.keyboard.press('ArrowUp')
  await expect(editor(page)).toHaveValue('=A2')
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 3, 0)).toBe('=A2')
})

test('shift and an arrow widen the pointed reference into a range', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=SUM(')
  await page.keyboard.press('ArrowUp')
  await expect(editor(page)).toHaveValue('=SUM(A3')

  await page.keyboard.press('Shift+ArrowUp')
  await page.keyboard.press('Shift+ArrowUp')
  await expect(editor(page)).toHaveValue('=SUM(A1:A3')

  // Narrowing again has to work too, or the anchor is being moved rather
  // than held.
  await page.keyboard.press('Shift+ArrowDown')
  await expect(editor(page)).toHaveValue('=SUM(A2:A3')

  await page.keyboard.type(')')
  await page.keyboard.press('Enter')
  expect(await inputAt(page, 3, 0)).toBe('=SUM(A2:A3)')
})

test('an operator ends the run so the next arrow starts a new reference', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=')
  await page.keyboard.press('ArrowUp')
  await page.keyboard.type('+')
  await page.keyboard.press('ArrowUp')
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 3, 0)).toBe('=A3+A3')
})

test('the arrows go back to the caret once the formula has its operand', async ({ page }) => {
  // `=A1+1` is not expecting a reference, so an arrow there is somebody
  // correcting a typo — and stealing it would make that impossible.
  await clickCell(page, 0, 0)
  await page.keyboard.type('=1+2')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.type('9')
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 0, 0)).toBe('=19+2')
})

test('an edit opened with F2 never points, it amends', async ({ page }) => {
  // F2 means "change what is here". If the arrows pointed, correcting a
  // reference in the middle of an existing formula would be impossible.
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=SUM(')
  await page.keyboard.press('Escape')

  await clickCell(page, 3, 0)
  await page.keyboard.type('=1+2')
  await page.keyboard.press('Enter')
  await clickCell(page, 3, 0)
  await page.keyboard.press('F2')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.type('0')
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 3, 0)).toBe('=10+2')
})

test('arrows keep committing and moving when the cell is not a formula', async ({ page }) => {
  // Enter mode still has to work: the pointing rule is about formulas only.
  await clickCell(page, 0, 0)
  await page.keyboard.type('one')
  await page.keyboard.press('ArrowRight')
  await expect(address(page)).toHaveValue('B1')
  expect(await inputAt(page, 0, 0)).toBe('one')
})

test('arrow pointing and mouse pointing agree about the same reference', async ({ page }) => {
  await seed(page)
  await clickCell(page, 3, 0)
  await page.keyboard.type('=')
  await page.keyboard.press('ArrowUp')
  await expect(editor(page)).toHaveValue('=A3')
  // The mouse takes over the same slot rather than appending to it.
  await clickCell(page, 0, 0)
  await expect(editor(page)).toHaveValue('=A1')
})

test('clicking a column header writes a whole-column reference', async ({ page }) => {
  // `A:A`, not `A1:A3`: the point of pointing at a header is a reference that
  // keeps working when rows are added below.
  await seed(page)
  await clickCell(page, 4, 2)
  await page.keyboard.type('=SUM(')
  await clickColHeader(page, 0)
  await expect(editor(page)).toHaveValue('=SUM(A:A')

  await page.keyboard.type(')')
  await page.keyboard.press('Enter')
  expect(await inputAt(page, 4, 2)).toBe('=SUM(A:A)')
  expect(await valueAt(page, 'C5')).toBe('8')
})

test('a whole-column reference picks up rows added later', async ({ page }) => {
  await seed(page)
  await clickCell(page, 4, 2)
  await page.keyboard.type('=SUM(')
  await clickColHeader(page, 0)
  await page.keyboard.type(')')
  await page.keyboard.press('Enter')

  await clickCell(page, 8, 0)
  await page.keyboard.type('10')
  await page.keyboard.press('Enter')
  expect(await valueAt(page, 'C5')).toBe('18')
})

test('dragging across column headers writes the span', async ({ page }) => {
  await seed(page)
  await clickCell(page, 4, 3)
  await page.keyboard.type('=SUM(')
  await dragColHeaders(page, 0, 2)
  await expect(editor(page)).toHaveValue('=SUM(A:C')
})

test('clicking a row header writes a whole-row reference', async ({ page }) => {
  await seed(page)
  await clickCell(page, 6, 2)
  await page.keyboard.type('=SUM(')
  await clickRowHeader(page, 0)
  await expect(editor(page)).toHaveValue('=SUM(1:1')

  await page.keyboard.type(')')
  await page.keyboard.press('Enter')
  expect(await inputAt(page, 6, 2)).toBe('=SUM(1:1)')
})

test('a header click still selects the column when no formula is open', async ({ page }) => {
  // Pointing may not steal the ordinary gesture: clicking a header with
  // nothing being edited selects that column, as it always did.
  await seed(page)
  await clickColHeader(page, 1)
  await expect(page.locator('.toolbar__status')).toContainText('B')
  await expect(editor(page)).toHaveCount(0)
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
