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
