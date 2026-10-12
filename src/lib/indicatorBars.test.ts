import { describe, expect, it } from "vitest";
import {
  BAR_MAX_PX,
  BAR_MIN_PX,
  BAR_WEIGHTS,
  barHeights,
  isIndicatorPhase,
} from "./indicatorBars";

describe("barHeights", () => {
  it("rests at the minimum for silence and room noise", () => {
    for (const level of [0, 0.1, 0.2]) {
      expect(barHeights(level, 3)).toEqual(BAR_WEIGHTS.map(() => BAR_MIN_PX));
    }
  });

  it("grows with the level and peaks in the middle bar", () => {
    const soft = barHeights(0.45);
    const loud = barHeights(0.9);
    soft.forEach((h, i) => expect(loud[i]).toBeGreaterThan(h));
    expect(Math.max(...loud)).toBe(loud[2]);
    expect(loud.every((h) => h <= BAR_MAX_PX)).toBe(true);
  });

  it("clamps out-of-range and non-numeric levels", () => {
    expect(barHeights(5)).toEqual(barHeights(1));
    expect(barHeights(-1)).toEqual(barHeights(0));
    expect(barHeights(Number.NaN)).toEqual(barHeights(0));
  });

  it("wobbles between updates only while there is sound", () => {
    expect(barHeights(0.8, 1)).not.toEqual(barHeights(0.8, 2));
    expect(barHeights(0, 1)).toEqual(barHeights(0, 2));
  });
});

describe("isIndicatorPhase", () => {
  it("accepts only the phases the backend emits", () => {
    for (const phase of [
      "hidden",
      "idle",
      "listening",
      "transcribing",
      "error",
    ]) {
      expect(isIndicatorPhase(phase)).toBe(true);
    }
    for (const value of ["recording", "", null, undefined, 3]) {
      expect(isIndicatorPhase(value)).toBe(false);
    }
  });
});
