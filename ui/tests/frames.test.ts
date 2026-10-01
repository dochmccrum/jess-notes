import { it, expect, vi, beforeEach } from 'vitest'

// A manual display: `tick(dt)` runs every queued rAF callback with a timestamp `dt` ms later.
let queue: FrameRequestCallback[] = []
let now = 1000
function tick(dt: number) {
  now += dt
  const q = queue
  queue = []
  for (const cb of q) cb(now)
}

beforeEach(() => {
  queue = []
  now = 1000
  vi.resetModules()
  vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => queue.push(cb))
})

const load = () => import('../src/lib/frames')

it('learns a 120 Hz refresh interval, and ignores frames the app skipped', async () => {
  const f = await load()
  expect(f.frameInterval()).toBeCloseTo(16.67, 1)
  const done = f.calibrate(12)
  for (let i = 0; i < 12; i++) tick(8.33)
  expect(await done).toBeCloseTo(8.33, 2)
  // One dropped frame (16.7) doesn't move it; an idle gap of seconds isn't a delta at all.
  f.mutate(() => {})
  tick(16.67)
  f.mutate(() => {})
  tick(3000)
  expect(f.frameInterval()).toBeCloseTo(8.33, 2)
  expect(f.frameBudget()).toBeCloseTo(3.33, 1)
})

it('runs every queued read before any write, in one frame', async () => {
  const f = await load()
  const log: string[] = []
  f.mutate(() => log.push('w1'))
  f.measure(() => {
    log.push('r1')
    f.mutate(() => log.push('w-from-r1'))
  })
  f.measure(() => log.push('r2'))
  expect(queue.length).toBe(1)
  tick(8.33)
  expect(log).toEqual(['r1', 'r2', 'w1', 'w-from-r1'])
  expect(queue.length).toBe(0)
})

it('oncePerFrame coalesces calls made before the frame', async () => {
  const f = await load()
  let n = 0
  const bump = f.oncePerFrame(() => n++)
  bump()
  bump()
  bump()
  tick(8.33)
  expect(n).toBe(1)
  bump()
  tick(8.33)
  expect(n).toBe(2)
})

it('sliced yields once the frame budget is used, and stops when aborted', async () => {
  const f = await load()
  let clock = 0
  vi.spyOn(performance, 'now').mockImplementation(() => clock)
  let yields = 0
  vi.stubGlobal('scheduler', { yield: () => (yields++, Promise.resolve()) })
  const seen: number[] = []
  // Each item costs 1 ms; the budget at the default 16.7 ms interval is 6.7 ms.
  expect(await f.sliced([...Array(20).keys()], (i) => (seen.push(i), (clock += 1)))).toBe(true)
  expect(seen.length).toBe(20)
  expect(yields).toBe(2)
  const ac = new AbortController()
  const out = await f.sliced([...Array(20).keys()], (i) => {
    clock += 1
    if (i === 3) ac.abort()
  }, ac.signal)
  expect(out).toBe(false)
  vi.unstubAllGlobals()
})

it('frameStats reports percentiles and late frames against the measured interval', async () => {
  const f = await load()
  const done = f.calibrate(12)
  for (let i = 0; i < 12; i++) tick(8.33)
  await done
  const stop = f.frameStats()
  tick(8.33) // first frame: no delta yet
  for (let i = 0; i < 97; i++) tick(8.33)
  tick(16.67) // one missed refresh
  tick(25) // two missed
  const s = stop()
  expect(s.frames).toBe(99)
  expect(s.interval).toBeCloseTo(8.33, 2)
  expect(s.p50).toBeCloseTo(8.33, 2)
  expect(s.max).toBe(25)
  expect(s.late).toBe(2)
})
