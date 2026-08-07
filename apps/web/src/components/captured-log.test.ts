import { describe, expect, it } from 'vitest'
import { summarize } from './CapturedLog'

describe('summarize', () => {
  it('renders a full-mode payload as what it is', () => {
    expect(summarize({ addr: 'A1', input: '42', is_formula: false })).toBe(
      'addr=A1 input=42 is_formula=false',
    )
  })

  it('shows a redacted value as a hash, never as a reconstruction', () => {
    // The most convincing possible demonstration that the literal was not
    // kept is showing the user the thing that replaced it.
    const line = summarize({
      addr: 'A1',
      input: { hash: '90020ebfd48797ad', len: 5, type: 'number' },
      is_formula: false,
    })
    expect(line).toContain('addr=A1')
    expect(line).toContain('⟨number:5 90020ebf…⟩')
    expect(line).not.toContain('90020ebfd48797ad')
  })

  it('does not try to render nested structure it cannot summarize', () => {
    expect(summarize({ spec: { columns: [1, 2] } })).toBe('spec={…}')
    expect(summarize({ ranges: ['A1:B2', 'C3'] })).toBe('ranges=[2]')
  })

  it('survives an empty or odd payload', () => {
    expect(summarize({})).toBe('—')
    expect(summarize(null)).toBe('')
    expect(summarize('text')).toBe('text')
  })
})
