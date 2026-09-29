import { describe, it, expect, vi } from 'vitest'
import { register, bind, handleKey, normalise, applyOverrides, run, keyFor } from '../src/lib/commands'

const key = (k: string, mods: Partial<KeyboardEventInit> = {}) => new KeyboardEvent('keydown', { key: k, cancelable: true, ...mods })

describe('commands', () => {
  it('normalises Mod to Ctrl off Apple', () => {
    expect(normalise('Mod-\\')).toBe('Ctrl-\\')
    expect(normalise('Shift-Mod-P')).toBe('Ctrl-Shift-p')
    expect(normalise('F2')).toBe('F2')
  })
  it('runs bound commands and respects `when`', async () => {
    const f = vi.fn()
    let enabled = false
    register({ id: 't.a', title: 'A', run: f, when: () => enabled })
    bind('Mod-k', 't.a')
    expect(handleKey(key('k', { ctrlKey: true }))).toBe(false)
    enabled = true
    const e = key('k', { ctrlKey: true })
    expect(handleKey(e)).toBe(true)
    expect(e.defaultPrevented).toBe(true)
    expect(f).toHaveBeenCalledTimes(1)
    expect(await run('t.a', { target: 'x' })).toBe(true)
    expect(f).toHaveBeenLastCalledWith({ target: 'x' })
  })
  it('user overrides replace the default binding', () => {
    const f = vi.fn()
    register({ id: 't.b', title: 'B', run: f })
    bind('Mod-j', 't.b')
    applyOverrides({ 'Alt-j': 't.b' })
    expect(handleKey(key('j', { ctrlKey: true }))).toBe(false)
    expect(handleKey(key('j', { altKey: true }))).toBe(true)
    expect(keyFor('t.b')).toBe('Alt+j')
  })
})
