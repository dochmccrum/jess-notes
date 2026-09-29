import 'fake-indexeddb/auto'

// jsdom gaps used by components.
if (typeof window !== 'undefined') {
  if (!('PointerEvent' in window)) {
    class PointerEventPolyfill extends MouseEvent {
      pointerType: string
      constructor(type: string, init: PointerEventInit = {}) {
        super(type, init)
        this.pointerType = init.pointerType ?? 'mouse'
      }
    }
    ;(window as unknown as Record<string, unknown>).PointerEvent = PointerEventPolyfill
  }
  if (!('ResizeObserver' in window)) {
    ;(window as unknown as Record<string, unknown>).ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  }
}

// CodeMirror measures text ranges; jsdom has no layout.
if (typeof Range !== 'undefined' && !Range.prototype.getClientRects) {
  Range.prototype.getClientRects = () => ({ length: 0, item: () => null, [Symbol.iterator]: [][Symbol.iterator] }) as unknown as DOMRectList
  Range.prototype.getBoundingClientRect = () => ({ x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, toJSON() {} }) as DOMRect
}
