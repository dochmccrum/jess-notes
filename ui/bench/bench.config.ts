import { defineConfig, devices } from '@playwright/test'

// The DESIGN §18 benchmarks, against a server whose data dir holds the generated vault
// (`scripts/bench.sh` prepares it and runs this). Results go to $BENCH_OUT (JSON lines).
const port = Number(process.env.BENCH_PORT ?? 18950)
const bin = process.env.JESS_BIN ?? '../target/release/jess'
export const PASSWORD = 'correct horse battery'

export default defineConfig({
  testDir: '.',
  testMatch: /\.bench\.ts$/,
  timeout: 20 * 60_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: { ...devices['Desktop Chrome'], baseURL: `http://127.0.0.1:${port}`, trace: 'off' },
  webServer: {
    command: `${bin} serve`,
    cwd: '..', // ui/, for JESS_UI_DIR=dist
    url: `http://127.0.0.1:${port}/healthz`,
    reuseExistingServer: false,
    timeout: 120_000,
    env: {
      JESS_DATA_DIR: process.env.BENCH_DATA ?? '',
      PORT: String(port),
      JESS_UI_DIR: 'dist',
      JESS_ADMIN_PASSWORD: PASSWORD,
      RUST_LOG: 'warn',
      JESS_LOGIN_RATE_PER_MINUTE: '1000',
      // A server on another machine in real use: keep its background work (git, mirror,
      // thumbnails for the 20k imported images) out of the numbers.
      JESS_GIT_ENABLED: 'false',
      JESS_MIRROR_ENABLED: 'false',
      JESS_DERIVE: 'false',
    },
  },
})
