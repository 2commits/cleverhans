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
    const before = float?.style.left;

    press(100, 100);
    move(160, 140);
    release(160, 140);

    expect(float?.style.left).not.toBe(before);
    expect(screen.queryByLabelText("Message")).toBeNull();
  });

  it("persists the dragged position under the storage key", () => {
    const { unmount } = mount("test:pos");
    press(100, 100);
    move(200, 160);
    release(200, 160);
    const stored = localStorage.getItem("test:pos");
    expect(stored).not.toBeNull();
    unmount();

    const { container } = mount("test:pos");

    const float = container.querySelector<HTMLElement>("[data-cleverhans-float]");
    const saved = JSON.parse(stored ?? "{}") as { x: number; y: number };
    expect(float?.style.left).toBe(`${saved.x}px`);
  });

  it("clamps the bubble inside the viewport", () => {
    const { container } = mount();
    const float = container.querySelector<HTMLElement>("[data-cleverhans-float]");

    press(100, 100);
    move(-1500, -1500);
    release(-1500, -1500);

    expect(float?.style.left).toBe("8px");
    expect(float?.style.top).toBe("8px");
  });
});
