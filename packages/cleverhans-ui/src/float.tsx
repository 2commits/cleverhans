/**
 * A floating chat widget: a draggable launcher (the CleverHans knight) the
 * user can park anywhere; clicking it toggles the chat panel, anchored to
 * the launcher.
 *
 * Position is stored as offsets from the bottom-right corner, so the widget
 * follows the window when it resizes — a bubble parked near a corner stays
 * near that corner instead of drifting off-screen. It also re-clamps on
 * resize and persists across reloads (localStorage).
 */

import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";

import { AgentChat, type AgentChatProps } from "./chat";

/** Launcher position as pixel offsets from the bottom-right viewport corner. */
export interface FloatPosition {
  right: number;
  bottom: number;
}

/** Props for {@link FloatingChat}. */
export interface FloatingChatProps extends AgentChatProps {
  /** Where the launcher starts when no stored position exists. */
  defaultPosition?: FloatPosition;
  /**
   * localStorage key for persisting the launcher position. Pass `null` to
   * disable persistence.
   */
  storageKey?: string | null;
  /** Whether the panel starts open. */
  defaultOpen?: boolean;
  /** Accessible label for the launcher. */
  label?: string;
}

const DEFAULT_STORAGE_KEY = "cleverhans:float-position:v2";
const BUBBLE = 52;
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
    right: Math.min(Math.max(pos.right, MARGIN), w - BUBBLE - MARGIN),
    bottom: Math.min(Math.max(pos.bottom, MARGIN), h - BUBBLE - MARGIN),
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
      typeof (parsed as FloatPosition).right === "number" &&
      typeof (parsed as FloatPosition).bottom === "number"
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
 * Launcher + togglable chat panel. Drag the knight to move the widget;
 * click it to open/close. Everything inside the panel is the same
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
    return stored ?? clamp(defaultPosition ?? { right: 24, bottom: 24 });
  });
  const drag = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    origin: FloatPosition;
    moved: boolean;
  } | null>(null);

  // Corner offsets already track the bottom-right edge; re-clamping on
  // resize keeps the widget inside a *shrinking* window too.
  useEffect(() => {
    const onResize = () => setPosition((pos) => clamp(pos));
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const onPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLButtonElement>) => {
      event.currentTarget.setPointerCapture?.(event.pointerId);
      drag.current = {
        pointerId: event.pointerId,
        startX: event.clientX,
        startY: event.clientY,
        origin: position,
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
    // Dragging right/down shrinks the distance to the bottom-right corner.
    setPosition(clamp({ right: state.origin.right - dx, bottom: state.origin.bottom - dy }));
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
    // Launcher in the lower half → panel opens upward, and vice versa.
    position.bottom + BUBBLE / 2 < h / 2 ? "ch-float-panel--above" : "ch-float-panel--below",
    // Launcher in the right half → panel extends leftward, and vice versa.
    position.right + BUBBLE / 2 < w / 2 ? "ch-float-panel--right" : "ch-float-panel--left",
  ].join(" ");

  return (
    <div
      className="ch-float"
      style={{ right: position.right, bottom: position.bottom }}
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
        <span aria-hidden="true">{open ? "×" : "♞"}</span>
      </button>
    </div>
  );
}
