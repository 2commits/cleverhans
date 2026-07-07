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

import { AgentProvider, useAgentSession, type SessionSnapshot } from "@cleverhans/react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";

import { AgentChat, type AgentChatProps } from "./chat";
import { HorseIcon } from "./icon";

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
/** Drag distance (px) past which releasing the grip closes the sheet. */
const SHEET_CLOSE_DRAG = 120;
/** Matches the sheet's snap/close transition in styles.css. */
const SHEET_SETTLE_MS = 220;
const MOBILE_QUERY = "(max-width: 640px)";

/** Tracks the mobile breakpoint; false where `matchMedia` is unavailable. */
function useIsMobile(): boolean {
  const [mobile, setMobile] = useState(
    () => typeof window !== "undefined" && !!window.matchMedia?.(MOBILE_QUERY).matches,
  );
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
      return;
    }
    const query = window.matchMedia(MOBILE_QUERY);
    const onChange = () => setMobile(query.matches);
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, []);
  return mobile;
}

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

/** What the closed pill shows, if anything. */
interface PillStatus {
  kind: "streaming" | "thinking" | "pending";
  text: string;
}

/** Last `max` characters of a message, flattened to one line. */
function tail(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length <= max ? flat : `…${flat.slice(-max)}`;
}

function pillStatus(snapshot: SessionSnapshot): PillStatus | null {
  if (snapshot.busy === "streaming") {
    const last = snapshot.transcript[snapshot.transcript.length - 1];
    const live = last?.role === "assistant" ? tail(last.text, 36) : "";
    return { kind: "streaming", text: live.length > 0 ? live : "Answering…" };
  }
  if (snapshot.busy === "thinking") {
    return { kind: "thinking", text: "Thinking" };
  }
  const pending = snapshot.pending.length;
  if (pending > 0) {
    return {
      kind: "pending",
      text: pending === 1 ? "1 action to review" : `${pending} actions to review`,
    };
  }
  return null;
}

function ThinkingDots(): ReactNode {
  return (
    <span className="ch-dots" aria-label="Thinking">
      <span />
      <span />
      <span />
    </span>
  );
}

/**
 * Launcher + togglable chat panel. Drag the knight to move the widget;
 * click it to open/close. Everything inside the panel is the same
 * {@link AgentChat} — same propose-only contract, same block components.
 *
 * The launcher is a living pill: while the panel is closed it morphs to
 * surface what the agent is doing — a thinking indicator, the live tail of
 * a streaming answer, or a count of proposals awaiting review.
 */
export function FloatingChat(props: FloatingChatProps): ReactNode {
  const { session, ...rest } = props;
  const inner = <FloatingChatInner {...rest} />;
  if (session) {
    return <AgentProvider session={session}>{inner}</AgentProvider>;
  }
  return inner;
}

function FloatingChatInner(props: Omit<FloatingChatProps, "session">): ReactNode {
  const {
    defaultPosition,
    storageKey = DEFAULT_STORAGE_KEY,
    defaultOpen = false,
    label = "Toggle assistant",
    ...chatProps
  } = props;

  const { snapshot } = useAgentSession();
  const mobile = useIsMobile();
  const [open, setOpen] = useState(defaultOpen);
  // Bottom-sheet drag state (mobile only): live translateY offset, and
  // whether that offset is animating (snap back / settle closed).
  const [dragY, setDragY] = useState(0);
  const [snapping, setSnapping] = useState(false);
  const sheetDrag = useRef<{ pointerId: number; startY: number; moved: boolean } | null>(null);
  const suppressGripClick = useRef(false);
  const closeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (closeTimer.current !== null) {
        clearTimeout(closeTimer.current);
      }
    };
  }, []);

  /** Animates the sheet off-screen, then unmounts it. */
  const settleClose = useCallback(() => {
    setSnapping(true);
    setDragY(viewport().h);
    if (closeTimer.current !== null) {
      clearTimeout(closeTimer.current);
    }
    closeTimer.current = setTimeout(() => {
      closeTimer.current = null;
      setOpen(false);
      setSnapping(false);
      setDragY(0);
    }, SHEET_SETTLE_MS);
  }, []);
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
        // A press without movement is a click: toggle the panel. A pending
        // sheet-close settle must not fire into the fresh panel.
        if (closeTimer.current !== null) {
          clearTimeout(closeTimer.current);
          closeTimer.current = null;
        }
        setSnapping(false);
        setDragY(0);
        setOpen((value) => !value);
      }
    },
    [storageKey],
  );

  const onGripPointerDown = useCallback((event: ReactPointerEvent<HTMLButtonElement>) => {
    event.currentTarget.setPointerCapture?.(event.pointerId);
    sheetDrag.current = {
      pointerId: event.pointerId,
      startY: event.clientY,
      moved: false,
    };
    setSnapping(false);
  }, []);

  const onGripPointerMove = useCallback((event: ReactPointerEvent<HTMLButtonElement>) => {
    const state = sheetDrag.current;
    if (!state || state.pointerId !== event.pointerId) {
      return;
    }
    const dy = event.clientY - state.startY;
    if (!state.moved && Math.abs(dy) < DRAG_THRESHOLD) {
      return;
    }
    state.moved = true;
    // The sheet only follows downward drags.
    setDragY(Math.max(0, dy));
  }, []);

  const onGripPointerUp = useCallback(
    (event: ReactPointerEvent<HTMLButtonElement>) => {
      const state = sheetDrag.current;
      if (!state || state.pointerId !== event.pointerId) {
        return;
      }
      sheetDrag.current = null;
      if (!state.moved) {
        return; // A tap: the grip's click handler closes.
      }
      suppressGripClick.current = true;
      if (event.clientY - state.startY > SHEET_CLOSE_DRAG) {
        settleClose();
      } else {
        setSnapping(true);
        setDragY(0);
      }
    },
    [settleClose],
  );

  const onGripClick = useCallback(() => {
    if (suppressGripClick.current) {
      suppressGripClick.current = false;
      return;
    }
    settleClose();
  }, [settleClose]);

  const { w, h } = viewport();
  const placement = [
    // Launcher in the lower half → panel opens upward, and vice versa.
    position.bottom + BUBBLE / 2 < h / 2 ? "ch-float-panel--above" : "ch-float-panel--below",
    // Launcher in the right half → panel extends leftward, and vice versa.
    position.right + BUBBLE / 2 < w / 2 ? "ch-float-panel--right" : "ch-float-panel--left",
  ].join(" ");

  const status = open ? null : pillStatus(snapshot);

  return (
    <div
      className="ch-float"
      style={{ right: position.right, bottom: position.bottom }}
      data-cleverhans-float
    >
      {open && mobile && (
        <>
          <div
            className="ch-float-backdrop"
            data-cleverhans-float-backdrop
            onClick={settleClose}
          />
          <div
            className="ch-float-sheet"
            data-cleverhans-float-panel
            style={{
              transform: `translateY(${dragY}px)`,
              transition: snapping
                ? `transform ${SHEET_SETTLE_MS}ms cubic-bezier(0.2, 0.8, 0.2, 1)`
                : "none",
            }}
          >
            <button
              type="button"
              className="ch-sheet-grip"
              aria-label="Close assistant"
              onClick={onGripClick}
              onPointerDown={onGripPointerDown}
              onPointerMove={onGripPointerMove}
              onPointerUp={onGripPointerUp}
            >
              <span className="ch-sheet-grip-bar" aria-hidden="true" />
            </button>
            <AgentChat {...chatProps} />
          </div>
        </>
      )}
      {open && !mobile && (
        <div className={`ch-float-panel ${placement}`} data-cleverhans-float-panel>
          <AgentChat {...chatProps} />
        </div>
      )}
      <button
        type="button"
        hidden={open && mobile}
        className={status ? "ch-fab ch-fab--pill" : "ch-fab"}
        data-status={status?.kind}
        aria-label={label}
        aria-expanded={open}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        {open ? (
          <span aria-hidden="true">×</span>
        ) : (
          <>
            <HorseIcon />
            {status && (
              <span className="ch-fab-status" role="status">
                {status.kind === "pending" && <i className="ch-fab-dot" aria-hidden="true" />}
                {status.kind === "thinking" ? <ThinkingDots /> : status.text}
              </span>
            )}
          </>
        )}
      </button>
    </div>
  );
}
