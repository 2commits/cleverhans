/**
 * The CleverHans mark: a minimal horse-head silhouette (long blunt muzzle,
 * upright ear, cropped mane, arched neck). Single even-odd path — the eye
 * and nostril are cutouts, so it sits on any background — filled with
 * `currentColor`, so it themes with the surrounding text color.
 */

import type { ReactNode } from "react";

const HORSE_PATH =
  "M8.6 21 V18.4 Q8.8 16 7.4 14.4 Q5.8 14 4.9 12.9 Q3.6 12.4 3.6 11.2 " +
  "L3.6 10 Q3.6 9.3 4.4 9.1 L11 6.4 Q11.6 5 12.6 4.4 L13.4 2.2 " +
  "Q14.3 3 14.4 4.6 L16 5.4 L15.4 7 L17.1 7.5 L16.7 9.1 L18.2 9.7 " +
  "Q19 13.4 18.2 21 Z " +
  "M10.7 7 a0.7 0.7 0 1 0 1.4 0 a0.7 0.7 0 1 0 -1.4 0 " +
  "M4.05 10.4 a0.55 0.55 0 1 0 1.1 0 a0.55 0.55 0 1 0 -1.1 0";

/** Props for {@link HorseIcon}. */
export interface HorseIconProps {
  /** Rendered width/height in px. */
  size?: number;
}

/** The horse-head mark, sized for buttons and headers. */
export function HorseIcon(props: HorseIconProps): ReactNode {
  const size = props.size ?? 26;
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      aria-hidden="true"
      focusable="false"
    >
      <path fill="currentColor" fillRule="evenodd" d={HORSE_PATH} />
    </svg>
  );
}
