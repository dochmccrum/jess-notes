// Reconnect backoff (DESIGN §5.5): immediate, 250 ms, 500 ms, 1 s, 2 s, 4 s, 8 s, 15 s cap
// (60 s in the background), ±20 % jitter. Mirrors core::client::reconnect_delay_ms.
const STEPS = [0, 250, 500, 1000, 2000, 4000, 8000, 15000]

export function reconnectDelay(attempt: number, foreground: boolean, jitter = Math.random()): number {
  let base = attempt < STEPS.length ? STEPS[attempt] : foreground ? 15000 : Math.min(60000, 15000 * 2 ** Math.min(2, attempt - STEPS.length + 1))
  base = Math.min(base, foreground ? 15000 : 60000)
  return Math.floor(base * (0.8 + 0.4 * Math.min(1, Math.max(0, jitter))))
}
