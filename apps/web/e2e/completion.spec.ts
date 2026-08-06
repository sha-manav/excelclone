/**
 * Function-name completion.
 *
 * Nobody remembers whether it is `NETWORKDAYS` or `WORKDAYS`, and a
 * spreadsheet that answers a near miss with `#NAME?` and no hint puts the user
 * in a guessing game. The menu exists so the names are discoverable from the
 * keyboard, the way they are in Excel.
 *
 * The keys are the substance here. Enter, Tab, Escape and the arrows all mean
 * something to the editor underneath, and the menu is only allowed to take
 * them while it is open — so each test below checks both what the menu did and
 * that the editor still behaves when it is closed.
 */

import { expect, test, type Page } from '@playwright/test'

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

const editor = (page: Page) => page.locator('[data-testid=cell-editor]')
const menu = (page: Page) => page.locator('[data-testid=function-menu]')
const items = (page: Page) => page.locator('.fn-menu__item')
const formula = (page: Page) => page.locator('.formula-bar__input')

async function inputAt(page: Page, row: number, col: number) {
  await clickCell(page, row, col)
  return formula(page).inputValue()
}

test.beforeEach(async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('canvas')).toBeVisible({ timeout: 30_000 })
  await page.getByTestId('consent-decline').click()
  await expect(page.getByTestId('consent-modal')).toHaveCount(0)
})

test('typing a prefix offers the matching functions with their signatures', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=AVE')

  await expect(menu(page)).toBeVisible()
  // AVERAGE, AVERAGEIF, AVERAGEIFS — shortest first.
  await expect(items(page)).toHaveCount(3)
  await expect(items(page).first()).toContainText('AVERAGE(number1, [number2], …)')
  await expect(items(page).first()).toContainText('the arithmetic mean')
})

test('Tab accepts the highlighted function and opens the bracket', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=AVE')
  await page.keyboard.press('Tab')

  await expect(menu(page)).toHaveCount(0)
  await expect(editor(page)).toHaveValue('=AVERAGE(')
  // The caret has to land inside the call, or the next thing typed goes
  // outside the function it was meant for.
  await page.keyboard.type('1)')
  await expect(editor(page)).toHaveValue('=AVERAGE(1)')
})

test('the arrows move down the list rather than the caret', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=SUMI')
  await expect(items(page)).toHaveCount(2)
  await page.keyboard.press('ArrowDown')
  await page.keyboard.press('Enter')

  await expect(editor(page)).toHaveValue('=SUMIFS(')
})

test('Escape closes the menu without abandoning the edit', async ({ page }) => {
  // Two different meanings for one key, and getting this wrong loses work:
  // dismissing a menu by reflex must not throw away the formula behind it.
  await clickCell(page, 0, 0)
  await page.keyboard.type('=AVE')
  await page.keyboard.press('Escape')

  await expect(menu(page)).toHaveCount(0)
  await expect(editor(page)).toHaveValue('=AVE')

  await page.keyboard.press('Escape')
  await expect(editor(page)).toHaveCount(0)
})

test('Enter still commits when no menu is open', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=1+1')
  await expect(menu(page)).toHaveCount(0)
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 0, 0)).toBe('=1+1')
})

test('the menu goes away once the name is complete and unambiguous', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=COUNTBLANK')
  await expect(menu(page)).toHaveCount(0)
})

test('a reference is not treated as a function name', async ({ page }) => {
  // `=A` must not put a menu over the grid the user is about to point at, and
  // `=A1` is a reference however many functions start with A.
  await clickCell(page, 0, 0)
  await page.keyboard.type('=A')
  await expect(menu(page)).toHaveCount(0)
  await page.keyboard.type('B')
  await expect(menu(page)).toBeVisible()
  await page.keyboard.press('Backspace')
  await page.keyboard.type('1')
  await expect(menu(page)).toHaveCount(0)
})

test('completion works inside a nested call', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=IF(A1>0,SU')
  await expect(menu(page)).toBeVisible()
  await page.keyboard.press('Tab')

  await expect(editor(page)).toHaveValue('=IF(A1>0,SUM(')
})

test('clicking a suggestion accepts it without ending the edit', async ({ page }) => {
  await clickCell(page, 0, 0)
  await page.keyboard.type('=AVE')
  await page.locator('[data-testid=function-menu-item-AVERAGEIF]').click()

  await expect(editor(page)).toHaveValue('=AVERAGEIF(')
})

test('completion works in the formula bar too', async ({ page }) => {
  await clickCell(page, 1, 1)
  await formula(page).click()
  await formula(page).fill('=NETW')
  await expect(menu(page)).toBeVisible()
  await page.keyboard.press('Tab')

  await expect(formula(page)).toHaveValue('=NETWORKDAYS(')
})

test('completion hands over to pointing', async ({ page }) => {
  // The two features meet here: accept a function, then drag the range into
  // the brackets it just opened.
  for (const [row, text] of [
    [0, '1'],
    [1, '3'],
    [2, '4'],
  ] as const) {
    await clickCell(page, row, 0)
    await page.keyboard.type(text)
    await page.keyboard.press('Enter')
  }
  await clickCell(page, 3, 0)
  await page.keyboard.type('=AVER')
  await page.keyboard.press('Tab')

  const box = await page.locator('canvas').boundingBox()
  if (!box) throw new Error('canvas has no box')
  const at = (r: number) => ({
    x: box.x + HEADER_W + COL_W / 2,
    y: box.y + HEADER_H + r * ROW_H + ROW_H / 2,
  })
  await page.mouse.move(at(0).x, at(0).y)
  await page.mouse.down()
  await page.mouse.move(at(2).x, at(2).y, { steps: 8 })
  await page.mouse.up()
  await page.keyboard.type(')')
  await page.keyboard.press('Enter')

  expect(await inputAt(page, 3, 0)).toBe('=AVERAGE(A1:A3)')
})
