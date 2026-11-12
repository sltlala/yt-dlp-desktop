import { describe, expect, it } from 'vitest'
import { shortcutFor, type KeyInput } from './shortcuts'

function key(k: string, extra: Partial<KeyInput> = {}): KeyInput {
  return {
    key: k,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    target: document.body,
    ...extra,
  }
}

describe('快捷键', () => {
  it('列表上的基本映射', () => {
    expect(shortcutFor(key('v', { ctrlKey: true }), false)).toBe('paste')
    expect(shortcutFor(key('V', { ctrlKey: true }), false)).toBe('paste')
    expect(shortcutFor(key(' '), false)).toBe('togglePause')
    expect(shortcutFor(key('Delete'), false)).toBe('remove')
    expect(shortcutFor(key('a', { ctrlKey: true }), false)).toBe('selectAll')
    expect(shortcutFor(key('Escape'), false)).toBe('escape')
  })

  it('输入框里一律不拦截', () => {
    for (const tag of ['input', 'textarea', 'select']) {
      const target = document.createElement(tag)
      expect(shortcutFor(key(' ', { target }), false)).toBeNull()
      expect(shortcutFor(key('v', { ctrlKey: true, target }), false)).toBeNull()
      expect(shortcutFor(key('Delete', { target }), false)).toBeNull()
      expect(shortcutFor(key('Escape', { target }), false)).toBeNull()
    }
  })

  it('开着弹窗时只认 Esc', () => {
    expect(shortcutFor(key(' '), true)).toBeNull()
    expect(shortcutFor(key('Delete'), true)).toBeNull()
    expect(shortcutFor(key('Escape'), true)).toBe('escape')
  })

  it('带修饰键的空格和 Delete 不触发', () => {
    expect(shortcutFor(key(' ', { shiftKey: true }), false)).toBeNull()
    expect(shortcutFor(key('Delete', { ctrlKey: true }), false)).toBeNull()
    expect(shortcutFor(key('v', { ctrlKey: true, shiftKey: true }), false)).toBeNull()
    expect(shortcutFor(key('v', { ctrlKey: true, altKey: true }), false)).toBeNull()
    expect(shortcutFor(key('x'), false)).toBeNull()
  })
})
