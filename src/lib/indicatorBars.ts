/** Phases the floating indicator can show (mirrors `indicator::Phase`). */
export type IndicatorPhase =
  "hidden" | "idle" | "listening" | "transcribing" | "error";

export const BAR_WEIGHTS = [0.55, 0.8, 1, 0.8, 0.55];
export const BAR_MIN_PX = 4;
export const BAR_MAX_PX = 18;
/** Levels below this (room noise, about -48 dBFS) leave the bars at rest. */
const NOISE_GATE = 0.2;

/**
 * Bar heights in px for a mic level in 0..1 (RMS mapped from -60..0 dBFS).
 * `tick` (the count of level updates) adds a small per-bar wobble so the
 * bars read as live without inventing motion during silence.
 */
export function barHeights(level: number, tick = 0): number[] {
  const safe = Number.isFinite(level) ? level : 0;
  const gated = Math.min(
    1,
    Math.max(0, (safe - NOISE_GATE) / (1 - NOISE_GATE)),
  );
  return BAR_WEIGHTS.map((weight, i) => {
    const wobble = 0.85 + 0.15 * Math.sin(tick * 0.9 + i * 1.7);
    const span = (BAR_MAX_PX - BAR_MIN_PX) * gated * weight * wobble;
    return Math.round((BAR_MIN_PX + span) * 10) / 10;
  });
}

export function isIndicatorPhase(value: unknown): value is IndicatorPhase {
  return (
    value === "hidden" ||
    value === "idle" ||
    value === "listening" ||
    value === "transcribing" ||
    value === "error"
  );
}
