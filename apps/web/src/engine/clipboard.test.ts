// @vitest-environment jsdom
//
// The HTML half needs a DOMParser, which the default node environment does
// not have. Only this file pays for it.

/**
 * Clipboard round trips, and the cases that make TSV harder than a split.
 *
 * The adversarial ones are the point: a cell containing a tab, a cell
 * containing a newline, and text that came from somewhere that quotes
 * differently from us.
 */

import { describe, expect, it } from 'vitest'
import { parseHtmlTable, parseTsv, readClipboard, toHtml, toTsv } from './clipboard'

describe('tsv', () => {
  it('round-trips a plain block', () => {
    const block = [
      ['a', 'b'],
      ['1', '2'],
    ]
    expect(toTsv(block)).toBe('a\tb\n1\t2')
    expect(parseTsv(toTsv(block))).toEqual(block)
  })

  it('round-trips cells containing tabs, newlines and quotes', () => {
    const block = [['has\ttab', 'has\nnewline'], ['say "hi"', '"quoted"']]
    const tsv = toTsv(block)
    expect(parseTsv(tsv)).toEqual(block)
  })

  it('reads a trailing newline as the end of the last row, not a new one', () => {
    // Excel ends its TSV with a line break. Treating it as an empty row would
    // clear the cell below every paste.
    expect(parseTsv('a\tb\n1\t2\n')).toEqual([
      ['a', 'b'],
      ['1', '2'],
    ])
  })

  it('accepts CRLF, which is what Windows Excel writes', () => {
    expect(parseTsv('a\tb\r\n1\t2\r\n')).toEqual([
      ['a', 'b'],
      ['1', '2'],
    ])
  })

  it('pads a ragged block to a rectangle', () => {
    expect(parseTsv('a\tb\tc\n1')).toEqual([
      ['a', 'b', 'c'],
      ['1', '', ''],
    ])
  })

  it('keeps empty cells rather than collapsing them', () => {
    expect(parseTsv('a\t\tc')).toEqual([['a', '', 'c']])
  })

  it('does not treat a quote in the middle of a field as quoting', () => {
    // 5" is a measurement, not an unterminated quoted field.
    expect(parseTsv('5"\tx')).toEqual([['5"', 'x']])
  })
})

describe('html', () => {
  it('round-trips a block through a table', () => {
    const block = [
      ['a & b', '<c>'],
      ['1', '2'],
    ]
    expect(parseHtmlTable(toHtml(block))).toEqual(block)
  })

  it('reads a cell that holds a line break', () => {
    expect(parseHtmlTable('<table><tr><td>one<br>two</td></tr></table>')).toEqual([
      ['one\ntwo'],
    ])
  })

  it('ignores html that is not a table', () => {
    expect(parseHtmlTable('<p>hello</p>')).toBeNull()
  })

  it('pads a ragged table', () => {
    expect(
      parseHtmlTable('<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>'),
    ).toEqual([
      ['a', 'b'],
      ['c', ''],
    ])
  })
})

/** A minimal DataTransfer stand-in; jsdom's is not constructible. */
function transfer(parts: Record<string, string>): DataTransfer {
  return { getData: (t: string) => parts[t] ?? '' } as unknown as DataTransfer
}

describe('reading the clipboard', () => {
  it('prefers the table when both formats are present', () => {
    // The two disagree on purpose: the html is the one that can say a cell
    // holds a newline, so it has to win.
    const data = transfer({
      'text/html': '<table><tr><td>one<br>two</td></tr></table>',
      'text/plain': 'one\ntwo',
    })
    expect(readClipboard(data)).toEqual([['one\ntwo']])
  })

  it('falls back to tsv when the html is not a table', () => {
    const data = transfer({ 'text/html': '<p>x</p>', 'text/plain': 'a\tb' })
    expect(readClipboard(data)).toEqual([['a', 'b']])
  })

  it('is null when there is nothing to paste', () => {
    expect(readClipboard(transfer({}))).toBeNull()
    expect(readClipboard(null)).toBeNull()
  })

  it('reads a single value as a one-cell block', () => {
    expect(readClipboard(transfer({ 'text/plain': '42' }))).toEqual([['42']])
  })
})
