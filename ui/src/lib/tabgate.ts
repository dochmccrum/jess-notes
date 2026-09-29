// One active tab (D9, DESIGN §11.6): Web Locks, with a BroadcastChannel "take over" handshake.
// Browsers without Web Locks fall back to a BroadcastChannel ping.

const LOCK = 'jess-active'
const CHANNEL = 'jess-tabs'

export interface TabGate {
  /** Resolves true once this tab holds the lock; false if another tab has it. */
  tryAcquire(): Promise<boolean>
  /** Asks the holder to hand over, then waits for the lock. */
  takeOver(): Promise<void>
  /** Called when another tab takes over: flush, then the lock is released. */
  onLost(cb: () => Promise<void> | void): void
}

export function createTabGate(): TabGate {
  const bc = typeof BroadcastChannel !== 'undefined' ? new BroadcastChannel(CHANNEL) : null
  const locks = typeof navigator !== 'undefined' ? navigator.locks : undefined
  let release: (() => void) | null = null
  let lost: (() => Promise<void> | void) | null = null
  let holding = false

  bc?.addEventListener('message', async (m) => {
    const d = m.data as { t: string }
    if (d.t === 'take' && holding) {
      holding = false
      await lost?.()
      release?.()
      release = null
      bc.postMessage({ t: 'released' })
    } else if (d.t === 'ping' && holding) {
      bc.postMessage({ t: 'here' })
    }
  })

  const hold = (): Promise<boolean> =>
    new Promise((resolve) => {
      void locks!.request(LOCK, { ifAvailable: true }, (lock) => {
        if (!lock) {
          resolve(false)
          return undefined
        }
        holding = true
        resolve(true)
        return new Promise<void>((r) => (release = r))
      })
    })

  return {
    async tryAcquire() {
      if (locks) return hold()
      if (!bc) return (holding = true)
      // Fallback: anyone out there?
      const answered = await new Promise<boolean>((resolve) => {
        const on = (m: MessageEvent) => {
          if ((m.data as { t: string }).t === 'here') resolve(true)
        }
        bc.addEventListener('message', on)
        bc.postMessage({ t: 'ping' })
        setTimeout(() => {
          bc.removeEventListener('message', on)
          resolve(false)
        }, 250)
      })
      holding = !answered
      return holding
    },
    async takeOver() {
      bc?.postMessage({ t: 'take' })
      if (locks) {
        await new Promise<void>((resolve) => {
          void locks.request(LOCK, () => {
            holding = true
            resolve()
            return new Promise<void>((r) => (release = r))
          })
        })
      } else {
        await new Promise((r) => setTimeout(r, 300))
        holding = true
      }
    },
    onLost(cb) {
      lost = cb
    },
  }
}
