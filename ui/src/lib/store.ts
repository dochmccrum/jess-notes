// Minimal observable compatible with Svelte's store contract.
export interface Readable<T> {
  subscribe(run: (v: T) => void): () => void
  get(): T
}

export function writable<T>(initial: T): Readable<T> & { set(v: T): void; update(f: (v: T) => T): void } {
  let value = initial
  const subs = new Set<(v: T) => void>()
  return {
    subscribe(run) {
      subs.add(run)
      run(value)
      return () => subs.delete(run)
    },
    get: () => value,
    set(v) {
      value = v
      for (const s of subs) s(v)
    },
    update(f) {
      this.set(f(value))
    },
  }
}
