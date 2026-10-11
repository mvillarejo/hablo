import { barHeights, type IndicatorPhase } from "../lib/indicatorBars";

export interface IndicatorProps {
  phase: IndicatorPhase;
  /** Mic level 0..1. */
  level: number;
  /** Count of level updates, for the bars' wobble. */
  tick?: number;
}

const LABELS: Record<IndicatorPhase, string> = {
  hidden: "",
  listening: "Listening",
  transcribing: "Transcribing",
  error: "Dictation failed",
};

/** The floating pill: live mic bars, transcribing dots, or an error tint. */
export function Indicator({ phase, level, tick = 0 }: IndicatorProps) {
  const listening = phase === "listening" || phase === "hidden";
  return (
    <div
      className={`ttm-indicator ttm-indicator--${phase}`}
      role="status"
      aria-label={LABELS[phase]}
    >
      {phase === "transcribing" ? (
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
