/** When the floating pill is shown (mirrors `config::IndicatorMode`). */
export type IndicatorMode = "off" | "while-dictating" | "always";

export const DEFAULT_INDICATOR_MODE: IndicatorMode = "while-dictating";

export const INDICATOR_MODE_OPTIONS: ReadonlyArray<{
  value: IndicatorMode;
  label: string;
}> = [
  { value: "off", label: "Off" },
  { value: "while-dictating", label: "While dictating" },
  { value: "always", label: "Always" },
];

export function isIndicatorMode(value: unknown): value is IndicatorMode {
  return INDICATOR_MODE_OPTIONS.some((option) => option.value === value);
}
