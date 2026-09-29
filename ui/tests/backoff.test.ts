import { it, expect } from 'vitest'
import { reconnectDelay } from '../src/worker/backoff'

it('backoff matches core::client::reconnect_delay_ms (no jitter)', () => {
  const mid = 0.5 // jitter factor 1.0
  expect([0, 1, 2, 3, 4, 5, 6, 7, 8, 20].map((a) => reconnectDelay(a, true, mid))).toEqual([0, 250, 500, 1000, 2000, 4000, 8000, 15000, 15000, 15000])
  expect(reconnectDelay(30, false, mid)).toBe(60000)
  for (let i = 0; i < 100; i++) {
    const d = reconnectDelay(3, true)
    expect(d).toBeGreaterThanOrEqual(800)
    expect(d).toBeLessThanOrEqual(1200)
  }
})
