// jsdom has no PointerEvent; back it with MouseEvent so pointer coordinates
// and pointerId survive fireEvent.pointer*() in tests.
class PointerEventPolyfill extends MouseEvent {
  readonly pointerId: number;

  constructor(type: string, params: PointerEventInit = {}) {
    super(type, params);
    this.pointerId = params.pointerId ?? 0;
  }
}

if (typeof window !== "undefined" && !("PointerEvent" in window)) {
  Object.defineProperty(window, "PointerEvent", { value: PointerEventPolyfill });
}
