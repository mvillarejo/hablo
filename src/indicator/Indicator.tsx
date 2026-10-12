import { barHeights, type IndicatorPhase } from "../lib/indicatorBars";

export interface IndicatorProps {
  phase: IndicatorPhase;
  /** Mic level 0..1. */
  level: number;
  /** Count of level updates, for the bars' wobble. */
  tick?: number;
  /** Primary-button press on the pill (starts a window drag). */
  onMouseDown?: React.MouseEventHandler<HTMLDivElement>;
}

const LABELS: Record<IndicatorPhase, string> = {
  hidden: "",
  idle: "Ready to dictate",
  listening: "Listening",
  transcribing: "Transcribing",
  error: "Dictation failed",
};

/** The floating pill: a dimmed mic at rest, live mic bars, transcribing
 * dots, or an error tint. */
export function Indicator({
  phase,
  level,
  tick = 0,
  onMouseDown,
}: IndicatorProps) {
  const listening = phase === "listening" || phase === "hidden";
  return (
    <div
      className={`ttm-indicator ttm-indicator--${phase}`}
      role="status"
      aria-label={LABELS[phase]}
      onMouseDown={onMouseDown}
    >
      {phase === "idle" ? (
        <svg
          className="ttm-indicator-mic"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          <rect x="9" y="3" width="6" height="11" rx="3" />
          <path d="M6 11a6 6 0 0 0 12 0M12 17v4" />
        </svg>
      ) : phase === "transcribing" ? (
        <span className="ttm-indicator-dots" aria-hidden="true">
          <span />
          <span />
          <span />
        </span>
      ) : (
        <span className="ttm-indicator-bars" aria-hidden="true">
          {barHeights(listening ? level : 0, tick).map((height, i) => (
            <span key={i} style={{ height }} />
          ))}
        </span>
      )}
    </div>
  );
}
