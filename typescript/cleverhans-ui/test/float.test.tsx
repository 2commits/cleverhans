import {
  AgentSession,
  type AgentTransport,
  type ClientEvent,
  type ServerEvent,
} from "@cleverhans/react";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FloatingChat } from "../src";

class FakeTransport implements AgentTransport {
  sent: ClientEvent[] = [];
  #handlers = new Set<(event: ServerEvent) => void>();
  send(event: ClientEvent): void {
    this.sent.push(event);
  }
  subscribe(onEvent: (event: ServerEvent) => void): () => void {
    this.#handlers.add(onEvent);
    return () => this.#handlers.delete(onEvent);
  }
  emit(event: ServerEvent): void {
    for (const handler of this.#handlers) {
      handler(event);
    }
  }
}

function mount(storageKey: string | null = null) {
  const transport = new FakeTransport();
  const session = new AgentSession(transport, { context: { route: "/" } });
  return { transport, ...render(<FloatingChat session={session} storageKey={storageKey} />) };
}

function bubble(): HTMLElement {
  return screen.getByLabelText("Toggle assistant");
}

const press = (x: number, y: number) => {
  fireEvent.pointerDown(bubble(), { pointerId: 1, clientX: x, clientY: y });
};
const move = (x: number, y: number) => {
  fireEvent.pointerMove(bubble(), { pointerId: 1, clientX: x, clientY: y });
};
const release = (x: number, y: number) => {
  fireEvent.pointerUp(bubble(), { pointerId: 1, clientX: x, clientY: y });
};

describe("FloatingChat", () => {
  beforeEach(() => {
    localStorage.clear();
    window.innerWidth = 1024;
    window.innerHeight = 768;
  });

  it("starts closed and a click opens the chat", () => {
    mount();
    expect(screen.queryByLabelText("Message")).toBeNull();

    press(10, 10);
    release(10, 10);

    expect(screen.getByLabelText("Message")).toBeDefined();
    expect(bubble().getAttribute("aria-expanded")).toBe("true");
  });

  it("a second click closes the chat again", () => {
    mount();
    press(10, 10);
    release(10, 10);

    press(10, 10);
    release(10, 10);

    expect(screen.queryByLabelText("Message")).toBeNull();
  });

  it("dragging moves the bubble and does not toggle the panel", () => {
    const { container } = mount();
    const float = container.querySelector<HTMLElement>("[data-cleverhans-float]");
    expect(float?.style.right).toBe("24px");

    // Drag 60px left / 40px up: distance to the corner grows.
    press(500, 500);
    move(440, 460);
    release(440, 460);

    expect(float?.style.right).toBe("84px");
    expect(float?.style.bottom).toBe("64px");
    expect(screen.queryByLabelText("Message")).toBeNull();
  });

  it("persists the dragged position under the storage key", () => {
    const { unmount } = mount("test:pos");
    press(500, 500);
    move(400, 400);
    release(400, 400);
    const stored = localStorage.getItem("test:pos");
    expect(stored).not.toBeNull();
    unmount();

    const { container } = mount("test:pos");

    const float = container.querySelector<HTMLElement>("[data-cleverhans-float]");
    const saved = JSON.parse(stored ?? "{}") as { right: number; bottom: number };
    expect(float?.style.right).toBe(`${saved.right}px`);
    expect(float?.style.bottom).toBe(`${saved.bottom}px`);
  });

  it("clamps the bubble inside the viewport", () => {
    const { container } = mount();
    const float = container.querySelector<HTMLElement>("[data-cleverhans-float]");

    // Drag far beyond the top-left corner.
    press(500, 500);
    move(-2000, -2000);
    release(-2000, -2000);

    // 1024 - 52 (bubble) - 8 (margin) = 964; 768 - 52 - 8 = 708.
    expect(float?.style.right).toBe("964px");
    expect(float?.style.bottom).toBe("708px");
  });

  it("the closed pill surfaces a streaming answer's tail", () => {
    const { transport } = mount();

    act(() => {
      transport.emit({
        type: "chat_message",
        msg_id: "m-1",
        text: "Renaming the document",
        done: false,
      });
    });

    const fab = bubble();
    expect(fab.className).toContain("ch-fab--pill");
    expect(fab.getAttribute("data-status")).toBe("streaming");
    expect(fab.textContent).toContain("Renaming the document");
  });

  it("the closed pill counts proposals awaiting review", () => {
    const { transport } = mount();

    act(() => {
      transport.emit({
        type: "action_proposal",
        proposal_id: "prop-1",
        action_id: "document.rename",
        params: {},
        block_type: "confirm",
        slots: { title: "Rename document" },
        context_seq: 0,
      });
    });

    const fab = bubble();
    expect(fab.getAttribute("data-status")).toBe("pending");
    expect(fab.textContent).toContain("1 action to review");
  });

  it("the pill collapses back to a circle when the answer completes", () => {
    const { transport } = mount();
    act(() => {
      transport.emit({ type: "chat_message", msg_id: "m-1", text: "Hi", done: false });
    });
    expect(bubble().className).toContain("ch-fab--pill");

    act(() => {
      transport.emit({ type: "chat_message", msg_id: "m-1", text: "Hi there.", done: true });
    });

    expect(bubble().className).not.toContain("ch-fab--pill");
    expect(bubble().getAttribute("data-status")).toBeNull();
  });

  it("opening the panel hides the status pill", () => {
    const { transport } = mount();
    act(() => {
      transport.emit({ type: "chat_message", msg_id: "m-1", text: "Hi", done: false });
    });

    press(10, 10);
    release(10, 10);

    expect(bubble().className).not.toContain("ch-fab--pill");
    expect(screen.getByLabelText("Message")).toBeDefined();
  });

  it("re-clamps when the window shrinks", () => {
    const { container } = mount();
    const float = container.querySelector<HTMLElement>("[data-cleverhans-float]");
    press(500, 500);
    move(-2000, -2000);
    release(-2000, -2000);
    expect(float?.style.right).toBe("964px");

    window.innerWidth = 400;
    window.innerHeight = 400;
    fireEvent(window, new Event("resize"));

    // 400 - 52 - 8 = 340 on both axes.
    expect(float?.style.right).toBe("340px");
    expect(float?.style.bottom).toBe("340px");
  });
});

describe("FloatingChat bottom sheet (mobile)", () => {
  beforeEach(() => {
    localStorage.clear();
    window.innerWidth = 390;
    window.innerHeight = 844;
    vi.useFakeTimers();
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      configurable: true,
      value: (media: string) => ({
        matches: true,
        media,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    delete (window as { matchMedia?: unknown }).matchMedia;
  });

  function openSheet() {
    const mounted = mount();
    press(10, 10);
    release(10, 10);
    return mounted;
  }

  function grip(): HTMLElement {
    return screen.getByLabelText("Close assistant");
  }

  function sheet(container: HTMLElement): HTMLElement | null {
    return container.querySelector<HTMLElement>(".ch-float-sheet");
  }

  it("opens as a bottom sheet and hides the launcher", () => {
    const { container } = openSheet();

    expect(sheet(container)).not.toBeNull();
    expect(container.querySelector(".ch-float-backdrop")).not.toBeNull();
    expect(screen.getByLabelText("Message")).toBeDefined();
    expect((bubble() as HTMLButtonElement).hidden).toBe(true);
  });

  it("dragging the grip past the threshold closes the sheet", () => {
    const { container } = openSheet();

    fireEvent.pointerDown(grip(), { pointerId: 2, clientY: 100 });
    fireEvent.pointerMove(grip(), { pointerId: 2, clientY: 400 });
    fireEvent.pointerUp(grip(), { pointerId: 2, clientY: 400 });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(sheet(container)).toBeNull();
    expect((bubble() as HTMLButtonElement).hidden).toBe(false);
  });

  it("a short drag snaps back instead of closing", () => {
    const { container } = openSheet();

    fireEvent.pointerDown(grip(), { pointerId: 2, clientY: 100 });
    fireEvent.pointerMove(grip(), { pointerId: 2, clientY: 160 });
    fireEvent.pointerUp(grip(), { pointerId: 2, clientY: 160 });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(sheet(container)).not.toBeNull();
    expect(sheet(container)?.style.transform).toBe("translateY(0px)");
  });

  it("the drag follows the pointer downward only", () => {
    const { container } = openSheet();

    fireEvent.pointerDown(grip(), { pointerId: 2, clientY: 300 });
    fireEvent.pointerMove(grip(), { pointerId: 2, clientY: 380 });
    expect(sheet(container)?.style.transform).toBe("translateY(80px)");

    fireEvent.pointerMove(grip(), { pointerId: 2, clientY: 100 });
    expect(sheet(container)?.style.transform).toBe("translateY(0px)");
  });

  it("tapping the grip closes the sheet", () => {
    const { container } = openSheet();

    fireEvent.click(grip());
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(sheet(container)).toBeNull();
  });

  it("tapping the backdrop closes the sheet", () => {
    const { container } = openSheet();

    fireEvent.click(container.querySelector(".ch-float-backdrop") as HTMLElement);
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(sheet(container)).toBeNull();
  });
});
