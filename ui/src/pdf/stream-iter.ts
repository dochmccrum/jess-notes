// `for await` over a ReadableStream: PDF.js's `getTextContent` (PDF find) needs it, and Safari and
// Chromium < 124 (older Android WebViews; our minimum is 100) don't have it (DESIGN §22 item 59).

export async function* iterateStream<T>(stream: ReadableStream<T>): AsyncGenerator<T> {
  const reader = stream.getReader()
  let done = false
  try {
    for (;;) {
      const r = await reader.read()
      if (r.done) {
        done = true
        return
      }
      yield r.value
    }
  } finally {
    // Leaving the loop early cancels the stream, as the native iterator does.
    if (!done) await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}

export function installStreamIterator() {
  const proto = globalThis.ReadableStream?.prototype as (ReadableStream & { [Symbol.asyncIterator]?: unknown }) | undefined
  if (!proto || Symbol.asyncIterator in proto) return
  Object.defineProperty(proto, Symbol.asyncIterator, {
    configurable: true,
    writable: true,
    value: function (this: ReadableStream) {
      return iterateStream(this)
    },
  })
}

installStreamIterator()
