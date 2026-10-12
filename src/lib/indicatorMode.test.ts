import { describe, expect, it } from "vitest";
import {
  DEFAULT_INDICATOR_MODE,
  INDICATOR_MODE_OPTIONS,
  isIndicatorMode,
} from "./indicatorMode";

describe("isIndicatorMode", () => {
  it("accepts only the modes the backend stores", () => {
    for (const mode of ["off", "while-dictating", "always"]) {
      expect(isIndicatorMode(mode)).toBe(true);
    }
    for (const value of ["while_dictating", "on", "", null, undefined, true]) {
      expect(isIndicatorMode(value)).toBe(false);
    }
  });

  it("offers the three modes in order, defaulting to while dictating", () => {
    expect(INDICATOR_MODE_OPTIONS.map((o) => o.value)).toEqual([
      "off",
      "while-dictating",
      "always",
    ]);
    expect(isIndicatorMode(DEFAULT_INDICATOR_MODE)).toBe(true);
    expect(DEFAULT_INDICATOR_MODE).toBe("while-dictating");
  });
});
