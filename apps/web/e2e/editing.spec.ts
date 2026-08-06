/**
 * Editing and selection, held to Excel's behaviour.
 *
 * The gestures here are the ones people do without thinking, which is exactly
 * why getting them wrong makes an app feel broken rather than incomplete: you
 * type a value, click somewhere else, and expect the value to be *there*. Any
 * app that instead leaves an editor floating over the old cell reads as
 * unresponsive, because the thing that looks selected has stopped following
 * the mouse.
 *
 * Excel's two editing modes are the substance of this file and worth naming,
 * because the difference is invisible until you use the arrow keys:
 *
 *   * **Enter mode** — you started typing over a selected cell. Arrow keys
 *     commit what you typed and move, so you can fill a row by typing and
 *     arrowing without ever reaching for Enter.
 *   * **Edit mode** — you pressed F2 or double-clicked into an existing
 *     value. Arrow keys move the caret *within* the text, because you are
 *     amending rather than entering.
 *
 * A spreadsheet that only implements the second feels sticky in exactly the
 * way this suite exists to prevent.
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

/** The cell editor overlay, which only exists while editing. */
function editor(page: Page) {
  return page.locator('[data-testid=cell-editor]')
}

function address(page: Page) {
  return page.locator('.formula-bar__address')
}

function formula(page: Page) {
  return page.locator('.formula-bar__input')
}

/** What the engine holds for a cell, read through the formula bar. */
async function valueAt(page: Page, row: number, col: number) {
  await clickCell(page, row, col)
  return formula(page).inputValue()
}

test.beforeEach(async ({ page }) => {
  await openGrid(page)
})

test('clicking another cell while typing commits the edit and moves there', async ({ page }) => {
  // The gesture that made the app feel broken: type, click away, and the
  // editor stayed pinned to the old cell while the selection moved
  // underneath it — so nothing looked like it was following the mouse.
  await clickCell(page, 0, 0)
  await page.keyboard.type('hello')
  await expect(editor(page)).toBeVisible()

  await clickCell(page, 3, 2)

  await expect(editor(page)).toHaveCount(0)
  await expect(address(page)).toHaveValue('C4')
  expect(await valueAt(page, 0, 0)).toBe('hello')
})

test('dragging a range while typing commits the edit and selects the range', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('123')
  await expect(editor(page)).toBeVisible()

  await dragCells(page, [2, 1], [4, 3])

  await expect(editor(page)).toHaveCount(0)
  await expect(page.locator('.toolbar__status')).toContainText('B3:D5')
  expect(await valueAt(page, 0, 0)).toBe('123')
})

test('a dragged range can be cleared in one keystroke', async ({ page }) => {
  // The user's actual complaint, in the order they hit it: the last value is
  // typed but *not* committed, and the drag that follows has to both land it
  // and select the range. With the editor still open the Delete went into the
  // input, so the cells the user had just highlighted stayed exactly as they
  // were.
  for (const [row, text] of [
    [0, 'a'],
    [1, 'b'],
  ] as const) {
    await clickCell(page, row, 0)
    await page.keyboard.type(text)
    await page.keyboard.press('Enter')
  }
  await clickCell(page, 2, 0)
  await page.keyboard.type('c')
  await expect(editor(page)).toBeVisible()

  await dragCells(page, [0, 0], [2, 0])
  await expect(editor(page)).toHaveCount(0)
  await expect(page.locator('.toolbar__status')).toContainText('A1:A3')
  await page.keyboard.press('Delete')

  expect(await valueAt(page, 0, 0)).toBe('')
  expect(await valueAt(page, 1, 0)).toBe('')
  expect(await valueAt(page, 2, 0)).toBe('')
})

test('typing then arrowing commits and moves, without reaching for Enter', async ({ page }) => {
  // Excel's "enter mode". Filling a row should not require a keystroke
  // between every value.
  await clickCell(page, 0, 0)
  await page.keyboard.type('one')
  await page.keyboard.press('ArrowRight')
  await expect(address(page)).toHaveValue('B1')

  await page.keyboard.type('two')
  await page.keyboard.press('ArrowRight')
  await expect(address(page)).toHaveValue('C1')

  await page.keyboard.type('three')
  await page.keyboard.press('ArrowDown')
  await expect(address(page)).toHaveValue('C2')

  expect(await valueAt(page, 0, 0)).toBe('one')
  expect(await valueAt(page, 0, 1)).toBe('two')
  expect(await valueAt(page, 0, 2)).toBe('three')
})

test('F2 edits in place, where the arrow keys move the caret instead', async ({ page }) => {
  // The other half of the rule. If arrows committed here too, correcting a
  // typo in the middle of a value would be impossible.
  await clickCell(page, 0, 0)
  await page.keyboard.type('abcd')
  await page.keyboard.press('Enter')

  await clickCell(page, 0, 0)
  await page.keyboard.press('F2')
  await expect(editor(page)).toBeVisible()

  // Caret starts at the end; walk it left and insert.
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await expect(editor(page)).toBeVisible()
  await expect(address(page)).toHaveValue('A1')
  await page.keyboard.type('X')
  await page.keyboard.press('Enter')

  expect(await valueAt(page, 0, 0)).toBe('abXcd')
})

test('double-clicking a value edits it rather than replacing it', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('total')
  await page.keyboard.press('Enter')

  const p = await cellPoint(page, 0, 0)
  await page.mouse.dblclick(p.x, p.y)
  await expect(editor(page)).toHaveValue('total')

  await page.keyboard.press('ArrowLeft')
  await page.keyboard.type('!')
  await page.keyboard.press('Enter')
  expect(await valueAt(page, 0, 0)).toBe('tota!l')
})

test('Escape while typing leaves the cell as it was', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('keep')
  await page.keyboard.press('Enter')

  await clickCell(page, 0, 0)
  await page.keyboard.type('discard')
  await page.keyboard.press('Escape')

  await expect(editor(page)).toHaveCount(0)
  await expect(address(page)).toHaveValue('A1')
  expect(await valueAt(page, 0, 0)).toBe('keep')
})

test('clicking the formula bar while typing does not throw the edit away', async ({ page }) => {
  // Moving to the formula bar is continuing the same edit, not abandoning
  // it. A blur handler that committed or cancelled indiscriminately would
  // make the formula bar unusable.
  await clickCell(page, 1, 1)
  await page.keyboard.type('=1+')
  await formula(page).click()
  await formula(page).fill('=1+2')
  await formula(page).press('Enter')

  expect(await valueAt(page, 1, 1)).toBe('=1+2')
})

test('clicking a toolbar button while typing keeps what was typed', async ({ page }) => {
  // Focus can leave the editor without any grid gesture at all. Anything that
  // takes focus has to land the value, or it disappears with the editor and
  // the user is left looking at the cell they thought they had just filled.
  await clickCell(page, 0, 0)
  await page.keyboard.type('9')
  await page.getByRole('button', { name: 'Bold' }).click()

  await expect(editor(page)).toHaveCount(0)
  expect(await valueAt(page, 0, 0)).toBe('9')
})

test('shift-clicking extends the selection from the anchor', async ({ page }) => {
  await clickCell(page, 1, 1)
  const p = await cellPoint(page, 4, 3)
  // `mouse.click` has no `modifiers` option — it takes button, clickCount and
  // delay only — so the key has to be held around it by hand. Passing one is
  // silently ignored, which produces a plain click and a test that reports the
  // product broken for extending nothing.
  await page.keyboard.down('Shift')
  await page.mouse.click(p.x, p.y)
  await page.keyboard.up('Shift')
  await expect(page.locator('.toolbar__status')).toContainText('B2:D5')
})

test('typing over a multi-cell selection replaces only the active cell', async ({ page }) => {
  // Excel keeps the range selected and writes into the anchor, so Enter
  // walks down the selection rather than leaving it.
  await dragCells(page, [0, 0], [2, 0])
  await page.keyboard.type('x')
  await page.keyboard.press('Enter')

  expect(await valueAt(page, 0, 0)).toBe('x')
  expect(await valueAt(page, 1, 0)).toBe('')
})
