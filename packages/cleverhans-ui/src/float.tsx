/**
 * A floating chat widget: a draggable launcher bubble the user can park
 * anywhere in the app; clicking it toggles the chat panel, anchored to the
 * bubble. Position persists across reloads (localStorage) so the chat
 * floats wherever the user last put it.
 */

import { useCallback, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";

import { AgentChat, type AgentChatProps } from "./chat";

/** Pixel position of the launcher bubble (top-left corner). */
export interface FloatPosition {
  x: number;
  y: number;
}

/** Props for {@link FloatingChat}. */
export interface FloatingChatProps extends AgentChatProps {
  /** Where the bubble starts when no stored position exists. */
  defaultPosition?: FloatPosition;
  /**
   * localStorage key for persisting the bubble position. Pass `null` to
   * disable persistence.
   */
  storageKey?: string | null;
  /** Whether the panel starts open. */
  defaultOpen?: boolean;
  /** Accessible label for the launcher bubble. */
  label?: string;
}

const DEFAULT_STORAGE_KEY = "cleverhans:float-position";
const BUBBLE = 56;
const MARGIN = 8;
const DRAG_THRESHOLD = 4;

function viewport(): { w: number; h: number } {
  if (typeof window === "undefined") {
    return { w: 1024, h: 768 };
  }
  return { w: window.innerWidth, h: window.innerHeight };
}

function clamp(pos: FloatPosition): FloatPosition {
  const { w, h } = viewport();
  return {
    x: Math.min(Math.max(pos.x, MARGIN), w - BUBBLE - MARGIN),
    y: Math.min(Math.max(pos.y, MARGIN), h - BUBBLE - MARGIN),
  };
}

function loadPosition(key: string): FloatPosition | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) {
      return null;
    }
    const parsed: unknown = JSON.parse(raw);
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      typeof (parsed as FloatPosition).x === "number" &&
      typeof (parsed as FloatPosition).y === "number"
    ) {
      return clamp(parsed as FloatPosition);
    }
  } catch {
    // Storage unavailable or corrupt — fall back to the default position.
  }
  return null;
}

function storePosition(key: string | null, pos: FloatPosition): void {
  if (key === null) {
    return;
  }
  try {
    localStorage.setItem(key, JSON.stringify(pos));
  } catch {
    // Best effort only.
  }
}

/**
 * Launcher bubble + togglable chat panel. Drag the bubble to move the whole
 * widget; click it to open/close. Everything inside the panel is the same
 * {@link AgentChat} — same propose-only contract, same block components.
 */
export function FloatingChat(props: FloatingChatProps): ReactNode {
  const {
    defaultPosition,
    storageKey = DEFAULT_STORAGE_KEY,
    defaultOpen = false,
    label = "Toggle assistant",
    ...chatProps
  } = props;

  const [open, setOpen] = useState(defaultOpen);
  const [position, setPosition] = useState<FloatPosition>(() => {
    const stored = storageKey === null ? null : loadPosition(storageKey);
    if (stored) {
      return stored;
    }
    const { w, h } = viewport();
    return clamp(defaultPosition ?? { x: w - BUBBLE - 24, y: h - BUBBLE - 24 });
  });
  const drag = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    originX: number;
    originY: number;
    moved: boolean;
  } | null>(null);

  const onPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLButtonElement>) => {
      event.currentTarget.setPointerCapture?.(event.pointerId);
      drag.current = {
        pointerId: event.pointerId,
        startX: event.clientX,
        startY: event.clientY,
        originX: position.x,
        originY: position.y,
        moved: false,
      };
    },
    [position],
  );

  const onPointerMove = useCallback((event: ReactPointerEvent<HTMLButtonElement>) => {
    const state = drag.current;
    if (!state || state.pointerId !== event.pointerId) {
      return;
    }
    const dx = event.clientX - state.startX;
    const dy = event.clientY - state.startY;
    if (!state.moved && Math.abs(dx) < DRAG_THRESHOLD && Math.abs(dy) < DRAG_THRESHOLD) {
      return;
    }
    state.moved = true;
    setPosition(clamp({ x: state.originX + dx, y: state.originY + dy }));
  }, []);

  const onPointerUp = useCallback(
    (event: ReactPointerEvent<HTMLButtonElement>) => {
      const state = drag.current;
      if (!state || state.pointerId !== event.pointerId) {
        return;
      }
      drag.current = null;
      if (state.moved) {
        setPosition((pos) => {
          storePosition(storageKey, pos);
          return pos;
        });
      } else {
        // A press without movement is a click: toggle the panel.
        setOpen((value) => !value);
      }
    },
    [storageKey],
  );

  const { w, h } = viewport();
  const placement = [
    position.y + BUBBLE / 2 > h / 2 ? "ch-float-panel--above" : "ch-float-panel--below",
    position.x + BUBBLE / 2 > w / 2 ? "ch-float-panel--right" : "ch-float-panel--left",
  ].join(" ");

  return (
    <div
      className="ch-float"
      style={{ left: position.x, top: position.y }}
      data-cleverhans-float
    >
      {open && (
        <div className={`ch-float-panel ${placement}`} data-cleverhans-float-panel>
          <AgentChat {...chatProps} />
        </div>
      )}
      <button
        type="button"
        className="ch-fab"
        aria-label={label}
        aria-expanded={open}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        {open ? "×" : "✦"}
      </button>
    </div>
  );
}
