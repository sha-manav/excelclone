/**
 * Signatures and one-line descriptions for the completion menu.
 *
 * The *names* come from the engine — `functionNames()` reads the same list the
 * parity harness reads and a Rust test pins to the dispatcher — because a
 * hand-kept copy would drift the first time somebody added a function without
 * thinking about the UI. What the engine does not carry is what each function
 * is *for*, and inventing an argument model in Rust purely to render a tooltip
 * would be a large change for a small purpose.
 *
 * So the split is: the engine owns which functions exist, this file owns how
 * to explain them, and `function_help.test.ts` fails if the two disagree in
 * either direction. A function with no entry still completes — it just
 * completes without a hint, which is better than not appearing at all.
 */

export interface FunctionHelp {
  /** Arguments as Excel writes them, brackets meaning optional. */
  args: string
  /** One line, lower case, no full stop — it sits beside the name. */
  about: string
}

export const FUNCTION_HELP: Record<string, FunctionHelp> = {
  // Math and statistics
  SUM: { args: 'number1, [number2], …', about: 'adds its arguments' },
  PRODUCT: { args: 'number1, [number2], …', about: 'multiplies its arguments' },
  AVERAGE: { args: 'number1, [number2], …', about: 'the arithmetic mean' },
  MIN: { args: 'number1, [number2], …', about: 'the smallest number' },
  MAX: { args: 'number1, [number2], …', about: 'the largest number' },
  COUNT: { args: 'value1, [value2], …', about: 'how many cells hold numbers' },
  COUNTA: { args: 'value1, [value2], …', about: 'how many cells are not empty' },
  COUNTBLANK: { args: 'range', about: 'how many cells in a range are empty' },
  ROUND: { args: 'number, digits', about: 'rounds to a number of digits' },
  ROUNDUP: { args: 'number, digits', about: 'rounds away from zero' },
  ROUNDDOWN: { args: 'number, digits', about: 'rounds towards zero' },
  ABS: { args: 'number', about: 'the size of a number, sign discarded' },
  INT: { args: 'number', about: 'rounds down to a whole number' },
  MOD: { args: 'number, divisor', about: 'the remainder after division' },
  POWER: { args: 'number, power', about: 'raises a number to a power' },
  SQRT: { args: 'number', about: 'the square root' },
  CEILING: { args: 'number, significance', about: 'rounds up to a multiple' },
  FLOOR: { args: 'number, significance', about: 'rounds down to a multiple' },
  MROUND: { args: 'number, multiple', about: 'rounds to the nearest multiple' },
  TRUNC: { args: 'number, [digits]', about: 'cuts off digits without rounding' },
  SIGN: { args: 'number', about: '1, 0 or -1 for the sign of a number' },
  EXP: { args: 'number', about: 'e raised to a power' },
  LN: { args: 'number', about: 'the natural logarithm' },
  LOG: { args: 'number, [base]', about: 'the logarithm in a given base' },
  LOG10: { args: 'number', about: 'the base-10 logarithm' },
  GCD: { args: 'number1, [number2], …', about: 'the greatest common divisor' },
  LCM: { args: 'number1, [number2], …', about: 'the least common multiple' },
  MEDIAN: { args: 'number1, [number2], …', about: 'the middle value' },
  LARGE: { args: 'array, k', about: 'the k-th largest value' },
  SMALL: { args: 'array, k', about: 'the k-th smallest value' },
  RANK: { args: 'number, ref, [order]', about: 'where a number places in a list' },
  SUMPRODUCT: { args: 'array1, [array2], …', about: 'multiplies then adds' },
  MODE: { args: 'number1, [number2], …', about: 'the most common value' },
  STDEV: { args: 'number1, [number2], …', about: 'the sample standard deviation' },
  LET: { args: 'name, value, …, result', about: 'names a value for reuse' },
  SUBTOTAL: { args: 'function_num, ref1, …', about: 'aggregates, skipping subtotals' },
  AGGREGATE: {
    args: 'function_num, options, ref1, …',
    about: 'aggregates with errors and hidden rows optional',
  },

  // Finance
  PMT: { args: 'rate, nper, pv, [fv], [type]', about: 'the payment on a loan' },
  FV: { args: 'rate, nper, pmt, [pv], [type]', about: 'the future value' },
  PV: { args: 'rate, nper, pmt, [fv], [type]', about: 'the present value' },
  NPER: { args: 'rate, pmt, pv, [fv], [type]', about: 'how many periods' },
  RATE: { args: 'nper, pmt, pv, [fv], [type]', about: 'the interest rate per period' },
  NPV: { args: 'rate, value1, …', about: 'the net present value' },
  IRR: { args: 'values, [guess]', about: 'the internal rate of return' },

  // Logic
  IF: { args: 'test, then, [else]', about: 'one value or another' },
  IFS: { args: 'test1, value1, …', about: 'the first test that holds' },
  AND: { args: 'logical1, [logical2], …', about: 'TRUE when all are true' },
  OR: { args: 'logical1, [logical2], …', about: 'TRUE when any is true' },
  NOT: { args: 'logical', about: 'reverses TRUE and FALSE' },
  IFERROR: { args: 'value, if_error', about: 'a fallback when a formula errors' },
  ISBLANK: { args: 'value', about: 'TRUE for an empty cell' },
  ISNUMBER: { args: 'value', about: 'TRUE for a number' },
  ISTEXT: { args: 'value', about: 'TRUE for text' },
  ISERROR: { args: 'value', about: 'TRUE for any error' },
  ISNA: { args: 'value', about: 'TRUE for #N/A' },
  ISERR: { args: 'value', about: 'TRUE for any error except #N/A' },
  ISLOGICAL: { args: 'value', about: 'TRUE for TRUE or FALSE' },
  IFNA: { args: 'value, if_na', about: 'a fallback when a formula gives #N/A' },
  NA: { args: '', about: 'the #N/A error' },
  TYPE: { args: 'value', about: 'a number naming the kind of value' },
  ISREF: { args: 'value', about: 'TRUE for a reference' },

  // Lookup and reference
  VLOOKUP: {
    args: 'lookup, table, col_index, [approx]',
    about: 'finds a row and reads across it',
  },
  HLOOKUP: {
    args: 'lookup, table, row_index, [approx]',
    about: 'finds a column and reads down it',
  },
  INDEX: { args: 'array, row, [col]', about: 'the value at a position' },
  MATCH: { args: 'lookup, array, [type]', about: 'the position of a value' },
  XLOOKUP: {
    args: 'lookup, lookup_array, return_array, [if_missing]',
    about: 'finds a value and returns beside it',
  },
  CHOOSE: { args: 'index, value1, …', about: 'picks a value by number' },
  ROW: { args: '[reference]', about: 'the row number' },
  COLUMN: { args: '[reference]', about: 'the column number' },
  ROWS: { args: 'array', about: 'how many rows' },
  COLUMNS: { args: 'array', about: 'how many columns' },
  XMATCH: { args: 'lookup, array, [match_mode]', about: 'the position of a value' },
  LOOKUP: { args: 'lookup, vector, [result]', about: 'the older one-argument lookup' },
  OFFSET: {
    args: 'reference, rows, cols, [height], [width]',
    about: 'a range shifted from another',
  },
  INDIRECT: { args: 'ref_text', about: 'a reference written as text' },
  UNIQUE: { args: 'array, [by_col], [once]', about: 'the distinct values' },
  SORT: { args: 'array, [index], [order], [by_col]', about: 'sorts a range' },
  SORTBY: { args: 'array, by_array1, …', about: 'sorts by another range' },
  FILTER: { args: 'array, include, [if_empty]', about: 'the rows that pass a test' },
  SEQUENCE: { args: 'rows, [cols], [start], [step]', about: 'a run of numbers' },
  TRANSPOSE: { args: 'array', about: 'swaps rows and columns' },
  TEXTSPLIT: { args: 'text, col_delim, [row_delim]', about: 'splits text across cells' },

  // Text
  CONCAT: { args: 'text1, [text2], …', about: 'joins text together' },
  CONCATENATE: { args: 'text1, [text2], …', about: 'joins text together' },
  TEXTJOIN: { args: 'delimiter, ignore_empty, text1, …', about: 'joins with a separator' },
  LEFT: { args: 'text, [count]', about: 'characters from the start' },
  RIGHT: { args: 'text, [count]', about: 'characters from the end' },
  MID: { args: 'text, start, count', about: 'characters from the middle' },
  LEN: { args: 'text', about: 'how many characters' },
  TRIM: { args: 'text', about: 'removes extra spaces' },
  UPPER: { args: 'text', about: 'converts to capitals' },
  LOWER: { args: 'text', about: 'converts to lower case' },
  PROPER: { args: 'text', about: 'capitalises each word' },
  SUBSTITUTE: { args: 'text, old, new, [which]', about: 'replaces text by content' },
  REPLACE: { args: 'text, start, count, new', about: 'replaces text by position' },
  FIND: { args: 'find, within, [start]', about: 'where text appears, case-sensitive' },
  SEARCH: { args: 'find, within, [start]', about: 'where text appears, any case' },
  TEXT: { args: 'value, format', about: 'formats a number as text' },
  VALUE: { args: 'text', about: 'reads a number out of text' },
  REPT: { args: 'text, count', about: 'repeats text' },
  EXACT: { args: 'text1, text2', about: 'TRUE when two strings match exactly' },
  CHAR: { args: 'number', about: 'the character with a code' },
  CODE: { args: 'text', about: 'the code of the first character' },
  CLEAN: { args: 'text', about: 'removes unprintable characters' },
  TEXTBEFORE: { args: 'text, delimiter, [instance]', about: 'what comes before a marker' },
  TEXTAFTER: { args: 'text, delimiter, [instance]', about: 'what comes after a marker' },
  NUMBERVALUE: { args: 'text, [decimal], [group]', about: 'reads a number with set separators' },

  // Conditional aggregation
  COUNTIF: { args: 'range, criteria', about: 'counts what matches' },
  COUNTIFS: { args: 'range1, criteria1, …', about: 'counts what matches every test' },
  SUMIF: { args: 'range, criteria, [sum_range]', about: 'adds what matches' },
  SUMIFS: { args: 'sum_range, range1, criteria1, …', about: 'adds what matches every test' },
  AVERAGEIF: { args: 'range, criteria, [average_range]', about: 'averages what matches' },
  AVERAGEIFS: {
    args: 'average_range, range1, criteria1, …',
    about: 'averages what matches every test',
  },

  // Date and time
  TODAY: { args: '', about: "today's date" },
  NOW: { args: '', about: 'the date and time now' },
  DATE: { args: 'year, month, day', about: 'builds a date' },
  YEAR: { args: 'date', about: 'the year of a date' },
  MONTH: { args: 'date', about: 'the month of a date' },
  DAY: { args: 'date', about: 'the day of a date' },
  EOMONTH: { args: 'date, months', about: 'the last day of a month' },
  DATEDIF: { args: 'start, end, unit', about: 'the gap between two dates' },
  WEEKDAY: { args: 'date, [type]', about: 'the day of the week as a number' },
  RAND: { args: '', about: 'a random number below 1' },
  RANDBETWEEN: { args: 'bottom, top', about: 'a random whole number in a range' },
  TIME: { args: 'hour, minute, second', about: 'builds a time' },
  HOUR: { args: 'time', about: 'the hour of a time' },
  MINUTE: { args: 'time', about: 'the minute of a time' },
  SECOND: { args: 'time', about: 'the second of a time' },
  DATEVALUE: { args: 'date_text', about: 'reads a date out of text' },
  EDATE: { args: 'date, months', about: 'the same day, months away' },
  DAYS: { args: 'end, start', about: 'days between two dates' },
  TIMEVALUE: { args: 'time_text', about: 'reads a time out of text' },
  NETWORKDAYS: { args: 'start, end, [holidays]', about: 'working days between dates' },
  WORKDAY: { args: 'start, days, [holidays]', about: 'the date after so many working days' },
  YEARFRAC: { args: 'start, end, [basis]', about: 'the fraction of a year between dates' },
}

/** `AVERAGE(number1, [number2], …)`, or just the name when nothing is known. */
export function signature(name: string): string {
  const help = FUNCTION_HELP[name]
  return help ? `${name}(${help.args})` : name
}
