import {
  AgentSession,
  type AgentTransport,
  type ClientEvent,
  type ServerEvent,
} from "@cleverhans/react";
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { FloatingChat } from "../src";

class FakeTransport implements AgentTransport {
  sent: ClientEvent[] = [];
  send(event: ClientEvent): void {
    this.sent.push(event);
  }
  subscribe(_onEvent: (event: ServerEvent) => void): () => void {
    return () => {};
  }
}

function mount(storageKey: string | null = null) {
  const session = new AgentSession(new FakeTransport(), { context: { route: "/" } });
  return render(<FloatingChat session={session} storageKey={storageKey} />);
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
