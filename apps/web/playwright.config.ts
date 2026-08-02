import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: process.env.CI ? 'line' : 'list',
  use: {
    baseURL: 'http://localhost:5173',
    trace: 'retain-on-failure',
  },
  // CI installs Playwright's own Chromium; locally we drive the system
  // Chrome so a developer does not need the extra 150MB download.
  //
  // GRIDLINE_CHROMIUM overrides both. Some sandboxes ship a Chromium whose
  // build number does not match the one this Playwright version expects, and
  // cannot reach the download host to fetch the matching one; pointing at the
  // binary that is already there beats not running the suite at all.
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        ...(process.env.GRIDLINE_CHROMIUM
          ? { launchOptions: { executablePath: process.env.GRIDLINE_CHROMIUM } }
          : process.env.CI
            ? {}
            : { channel: 'chrome' }),
      },
    },
  ],
  webServer: {
    command: 'npm run dev',
    url: 'http://localhost:5173',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
})
