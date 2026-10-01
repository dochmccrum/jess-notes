// Frame pacing (DESIGN §23). Three tools, all keyed to the display's measured refresh interval
// (8.3 ms at 120 Hz) rather than an assumed 60 Hz:
//   - `measure`/`mutate`: DOM reads and writes batched into the next frame, all reads first, so a
//     frame never forces layout halfway through its own writes;
//   - `sliced`: long main-thread work cut into pieces that fit the frame budget, yielding between;
//   - `frameStats`: rAF deltas and long animation frames over an interaction (tests, benchmarks).
// No rAF loop runs while the app is idle: the interval is sampled at start-up and refined from
// consecutive frames the app requests anyway.

const raf: (cb: FrameRequestCallback) => number =
  typeof requestAnimationFrame === 'function' ? requestAnimationFrame : (cb) => setTimeout(() => cb(performance.now()), 16) as unknown as number

let interval = 1000 / 60
const recent: number[] = []
let lastFrame = 0

/** Feeds the estimate a frame timestamp; deltas only count between consecutive frames. */
function sample(t: number) {
  const d = t - lastFrame
  lastFrame = t
  // A gap of more than ~2 frames at 30 Hz means we weren't running every frame: not a delta.
  if (d <= 0 || d > 70) return
  recent.push(d)
  if (recent.length > 24) recent.shift()
  if (recent.length >= 6) {
    const s = [...recent].sort((a, b) => a - b)
    // The 25th percentile: dropped frames make deltas longer, never shorter than one interval.
    interval = s[Math.floor(s.length / 4)]
  }
}

/** The display's frame interval in ms, as measured (16.7 until known). */
export function frameInterval(): number {
  return interval
}

/** Main-thread time a frame can give our own work: the rest is style, layout, paint and input. */
export function frameBudget(): number {
  return Math.max(1, interval * 0.4)
}

/** Samples a dozen frames to learn the refresh rate (call once at start-up). */
export function calibrate(frames = 12): Promise<number> {
  return new Promise((resolve) => {
    let n = 0
    const step = (t: number) => {
      sample(t)
      if (++n < frames) raf(step)
      else resolve(interval)
    }
    raf(step)
  })
}

// ---------------------------------------------------------------- per-frame read/write phases

const reads: (() => void)[] = []
const writes: (() => void)[] = []
let scheduled = false

function flush(t: number) {
  sample(t)
  // Reads that queue writes (the usual pattern) get them run in this same frame.
  for (let i = 0; i < reads.length; i++) run(reads[i])
  reads.length = 0
  for (let i = 0; i < writes.length; i++) run(writes[i])
  writes.length = 0
  scheduled = false
  // A read queued by a write would force layout after it: it waits for the next frame.
  if (reads.length) schedule()
}

function run(fn: () => void) {
  try {
    fn()
  } catch (e) {
    console.error(e)
  }
}

function schedule() {
  if (scheduled) return
  scheduled = true
  raf(flush)
}

/** Runs `fn` (DOM reads only) at the start of the next frame. */
export function measure(fn: () => void) {
  reads.push(fn)
  schedule()
}

/** Runs `fn` (DOM writes only) in the next frame, after every queued read. */
export function mutate(fn: () => void) {
  writes.push(fn)
  schedule()
}

/** Coalesces repeated requests: `fn` runs once next frame however often this is called before it. */
export function oncePerFrame(fn: () => void): () => void {
  let queued = false
  return () => {
    if (queued) return
    queued = true
    mutate(() => {
      queued = false
      fn()
    })
  }
}

// ---------------------------------------------------------------- time slicing

type Sched = { yield?: () => Promise<void> }

/** Gives the browser a chance to render and handle input, then continues. */
export function yieldToMain(): Promise<void> {
  const s = (globalThis as unknown as { scheduler?: Sched }).scheduler
  if (s?.yield) return s.yield()
  // A message-channel task: unlike setTimeout(0) it isn't clamped to 4 ms after nesting.
  return new Promise((r) => {
    const ch = new MessageChannel()
    ch.port1.onmessage = () => r()
    ch.port2.postMessage(null)
  })
}

/**
 * Calls `fn` on each item, yielding whenever the frame budget is used up. `signal` stops it early
 * (e.g. a newer search superseded this one). Returns false if it was aborted.
 */
export async function sliced<T>(items: Iterable<T>, fn: (item: T) => void, signal?: AbortSignal): Promise<boolean> {
  let start = performance.now()
  for (const item of items) {
    if (signal?.aborted) return false
    fn(item)
    if (performance.now() - start >= frameBudget()) {
      await yieldToMain()
      start = performance.now()
    }
  }
  return !signal?.aborted
}

// ---------------------------------------------------------------- measurement

export interface FrameStats {
  frames: number
  /** The refresh interval the run was judged against, ms. */
  interval: number
  p50: number
  p95: number
  p99: number
  max: number
  /** Frames that took more than 1.5 intervals: at least one refresh was missed. */
  late: number
  /** Long animation frames (Chromium 123+, others report none), with their main-thread time. */
  longFrames: { duration: number; blocking: number }[]
}

export interface BusyStats {
  /** Main-thread stretches over 2 ms (script, style, layout, paint recording), longest first. */
  stretches: number[]
  p99: number
  max: number
  /** Stretches longer than one 120 Hz frame (8.3 ms): each would cost a frame at 120 Hz. */
  over120: number
  /** Wall time recorded, ms. */
  span: number
}

/**
 * Records how long the main thread is busy at a stretch, with a message-channel heartbeat: every
 * gap between beats is time the thread spent on something else, rendering included. Unlike frame
 * deltas it doesn't depend on the display (or on a headless browser's 60 Hz cap), so CI can check
 * that interactions fit a 120 Hz frame. Stops when the returned function is called.
 */
export function busyStats(): () => BusyStats {
  const gaps: number[] = []
  const ch = new MessageChannel()
  let on = true
  const t0 = performance.now()
  let last = t0
  ch.port1.onmessage = () => {
    const t = performance.now()
    if (t - last > 2) gaps.push(t - last)
    last = t
    if (on) ch.port2.postMessage(null)
  }
  ch.port2.postMessage(null)
  return () => {
    on = false
    ch.port1.close()
    const s = gaps.sort((a, b) => b - a)
    const span = performance.now() - t0
    // p99 over 1 ms slots of the recorded span: the stretch a 1-in-100 moment falls inside.
    let acc = 0
    let p99 = 0
    for (const g of s) {
      acc += g
      if (acc >= span * 0.01) {
        p99 = g
        break
      }
    }
    return { stretches: s.slice(0, 20), p99, max: s[0] ?? 0, over120: s.filter((g) => g > 1000 / 120).length, span }
  }
}

/** Records every frame until the returned function is called. */
export function frameStats(): () => FrameStats {
  const deltas: number[] = []
  const longFrames: FrameStats['longFrames'] = []
  let last = 0
  let on = true
  const loop = (t: number) => {
    if (!on) return
    if (last) deltas.push(t - last)
    last = t
    sample(t)
    raf(loop)
  }
  raf(loop)
  let po: PerformanceObserver | undefined
  try {
    po = new PerformanceObserver((l) => {
      for (const e of l.getEntries()) longFrames.push({ duration: e.duration, blocking: (e as unknown as { blockingDuration?: number }).blockingDuration ?? 0 })
    })
    po.observe({ type: 'long-animation-frame', buffered: false })
  } catch {
    po = undefined
  }
  return () => {
    on = false
    po?.disconnect()
    const s = [...deltas].sort((a, b) => a - b)
    const at = (q: number) => (s.length ? s[Math.min(s.length - 1, Math.floor(s.length * q))] : 0)
    const iv = interval
    return { frames: s.length, interval: iv, p50: at(0.5), p95: at(0.95), p99: at(0.99), max: s[s.length - 1] ?? 0, late: s.filter((d) => d > iv * 1.5).length, longFrames }
  }
}
