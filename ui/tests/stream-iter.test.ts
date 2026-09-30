import { it, expect } from 'vitest'
import { iterateStream } from '../src/pdf/stream-iter'

const numbers = (n: number, onCancel?: () => void) => {
  let i = 0
  return new ReadableStream<number>({
    pull(c) {
      if (i < n) c.enqueue(i++)
      else c.close()
    },
    cancel: onCancel,
  })
}

it('iterates a ReadableStream to the end and releases it', async () => {
  const s = numbers(5)
  const got: number[] = []
  for await (const x of iterateStream(s)) got.push(x)
  expect(got).toEqual([0, 1, 2, 3, 4])
  expect(s.locked).toBe(false)
})

it('cancels the stream when the loop exits early', async () => {
  let cancelled = false
  const s = numbers(100, () => (cancelled = true))
  for await (const x of iterateStream(s)) if (x === 2) break
  expect(cancelled).toBe(true)
  expect(s.locked).toBe(false)
})
