import { describe, it, expect } from 'vitest'
import { openDb, commit, loadAll, scanPrefix, putChunk, getChunk, deleteChunks, metaPut, metaGet } from '../src/worker/idb'

const b = (...x: number[]) => new Uint8Array(x)

describe('IndexedDB storage layer', () => {
  it('commits atomically and keeps bytewise key order', async () => {
    const db = await openDb('t1')
    await commit(db, [
      [b(2, 0), b(20)],
      [b(1, 255), b(19)],
      [b(1), b(1)],
      [b(0xff), b(9)],
    ])
    await commit(db, [[b(1), null]])
    const [k, v] = await loadAll(db)
    expect(k.map((x) => [...x])).toEqual([[1, 255], [2, 0], [0xff]])
    expect(v.map((x) => [...x])).toEqual([[19], [20], [9]])
  })
  it('scans a prefix, including a 0xff tail', async () => {
    const db = await openDb('t2')
    await commit(db, [
      [b(5, 1), b(1)],
      [b(5, 2), b(2)],
      [b(6), b(3)],
      [b(0xff, 1), b(4)],
      [b(0xff, 0xff), b(5)],
    ])
    expect((await scanPrefix(db, b(5))).map((x) => x[0])).toEqual([1, 2])
    expect((await scanPrefix(db, b(0xff))).map((x) => x[0])).toEqual([4, 5])
  })
  it('stores blob chunks and meta', async () => {
    const db = await openDb('t3')
    await putChunk(db, 'h1', 0, b(1, 2))
    await putChunk(db, 'h1', 1, b(3))
    await putChunk(db, 'h2', 0, b(9))
    expect([...(await getChunk(db, 'h1', 1))!]).toEqual([3])
    await deleteChunks(db, 'h1')
    expect(await getChunk(db, 'h1', 0)).toBeNull()
    expect([...(await getChunk(db, 'h2', 0))!]).toEqual([9])
    await metaPut(db, 'token', 'abc')
    expect(await metaGet(db, 'token')).toBe('abc')
  })
})
