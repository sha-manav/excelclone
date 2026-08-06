/**
 * The bridge between the grid and the system clipboard.
 *
 * Everything here is pure: a block of cells in, text out, or text in and a
 * block of cells out. The DOM half — the `copy`/`cut`/`paste` listeners — is
 * three lines in `App.tsx`, because clipboard *events* are the only route to
 * the clipboard that does not need a permission prompt, and because the
 * parsing is where all the mistakes are.
 *
 * Two formats, which is what Excel and Sheets both put on the clipboard:
 *
 * - `text/plain` is tab-separated, newline-terminated, with a field quoted
 *   (RFC 4180 style, doubling internal quotes) when it contains a tab, a
 *   newline or a leading quote. This is what a text editor pastes.
 * - `text/html` is a `<table>`. It is preferred when reading, because a cell
 *   containing a newline is unambiguous there and merely plausible in TSV —
 *   and because Excel's own TSV quoting is not something to rely on.
 */

/** A rectangular block of cell text, row-major. Ragged rows are padded. */
export type Block = string[][]

const needsQuoting = (s: string): boolean =>
  s.includes('\t') || s.includes('\n') || s.includes('\r') || s.startsWith('"')

export function toTsv(block: Block): string {
  return block
    .map((row) =>
      row.map((cell) => (needsQuoting(cell) ? `"${cell.replace(/"/g, '""')}"` : cell)).join('\t'),
    )
    .join('\n')
}

const escapeHtml = (s: string): string =>
  s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')

export function toHtml(block: Block): string {
  const rows = block
    .map(
      (row) => `<tr>${row.map((c) => `<td>${escapeHtml(c) || '&nbsp;'}</td>`).join('')}</tr>`,
    )
    .join('')
  return `<table>${rows}</table>`
}

/**
 * Split TSV into a block, honouring quoted fields.
 *
 * A quoted field may contain tabs and newlines, which is the whole reason
 * this is a scanner and not `text.split('\n').map(l => l.split('\t'))`.
 */
export function parseTsv(text: string): Block {
  const block: Block = []
  let row: string[] = []
  let field = ''
  let quoted = false
  let i = 0
  const endField = () => {
    row.push(field)
    field = ''
  }
  const endRow = () => {
    endField()
    block.push(row)
    row = []
  }
  while (i < text.length) {
    const ch = text[i]
    if (quoted) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          field += '"'
          i += 2
          continue
        }
        quoted = false
        i++
        continue
      }
      field += ch
      i++
      continue
    }
    if (ch === '"' && field === '') {
      quoted = true
      i++
      continue
    }
    if (ch === '\t') {
      endField()
      i++
      continue
    }
    if (ch === '\r') {
      // Treat CRLF and a lone CR as one line break.
      if (text[i + 1] === '\n') i++
      endRow()
      i++
      continue
    }
    if (ch === '\n') {
      endRow()
      i++
      continue
    }
    field += ch
    i++
  }
  // A trailing newline ends the last row rather than starting an empty one.
  if (field !== '' || row.length > 0) endRow()
  return pad(block)
}

/**
 * A `<table>` from the clipboard into a block.
 *
 * Only tables are read. Pasting a paragraph of HTML as a spreadsheet has no
 * sensible answer, and the TSV alongside it is the better one.
 */
export function parseHtmlTable(html: string): Block | null {
  const doc = new DOMParser().parseFromString(html, 'text/html')
  const table = doc.querySelector('table')
  if (!table) return null
  const block: Block = []
  for (const tr of Array.from(table.querySelectorAll('tr'))) {
    const cells = Array.from(tr.querySelectorAll('td, th'))
    if (cells.length === 0) continue
    block.push(
      cells.map((td) =>
        // `innerText` is not available on a detached document, so line breaks
        // are restored by hand; without this a two-line cell arrives as one
        // word jammed against the next.
        (td.innerHTML ?? '')
          .replace(/<br\s*\/?>/gi, '\n')
          .replace(/<[^>]+>/g, '')
          .replace(/&nbsp;/g, ' ')
          .replace(/&lt;/g, '<')
          .replace(/&gt;/g, '>')
          .replace(/&quot;/g, '"')
          .replace(/&amp;/g, '&')
          .trim(),
      ),
    )
  }
  return block.length > 0 ? pad(block) : null
}

/** Pad ragged rows so the block is a rectangle. */
function pad(block: Block): Block {
  const width = block.reduce((w, r) => Math.max(w, r.length), 0)
  for (const row of block) while (row.length < width) row.push('')
  return block
}

/**
 * What the clipboard holds, as a block.
 *
 * HTML wins when it is a table, because a cell containing a newline survives
 * there and is a guess in TSV. Returns null when there is nothing to paste.
 */
export function readClipboard(data: DataTransfer | null): Block | null {
  if (!data) return null
  const html = data.getData('text/html')
  if (html) {
    const fromHtml = parseHtmlTable(html)
    if (fromHtml) return fromHtml
  }
  const text = data.getData('text/plain')
  if (!text) return null
  const block = parseTsv(text)
  return block.length > 0 ? block : null
}
