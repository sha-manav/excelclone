import { describe, expect, it } from 'vitest'
import { isStandalone } from './standalone'

describe('isStandalone', () => {
  it('is off unless the flag is explicitly set', () => {
    // The default has to be "there is a server", or a development build would
    // silently stop capturing and the whole pipeline would look broken again.
    expect(isStandalone()).toBe(false)
  })
})
