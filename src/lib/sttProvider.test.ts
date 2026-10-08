import { describe, expect, it } from "vitest";
import {
  canDictate,
  sessionEndFallbackMs,
  validateSttBaseUrl,
} from "./sttProvider";

describe("sessionEndFallbackMs", () => {
  it("keeps Gladia at 5 s regardless of recording length", () => {
    expect(sessionEndFallbackMs("gladia")).toBe(5_000);
    expect(sessionEndFallbackMs("gladia", 120)).toBe(5_000);
  });

  it.each([
    [0, 60_000],
    [10, 60_000],
    [28, 60_000],
    [28.1, 120_000],
    [90, 240_000],
    [-5, 60_000],
  ])("gives batch %s s of audio %s ms", (seconds, expected) => {
    expect(sessionEndFallbackMs("openai_compat", seconds)).toBe(expected);
  });
});

describe("canDictate", () => {
  it("requires a Gladia key only for Gladia", () => {
    expect(canDictate("gladia", "")).toBe(false);
    expect(canDictate("gladia", "   ")).toBe(false);
    expect(canDictate("gladia", "key")).toBe(true);
    expect(canDictate("openai_compat", "")).toBe(true);
  });
});

describe("validateSttBaseUrl", () => {
  it.each([
    "http://localhost:11434/v1",
    " https://api.groq.com/openai/v1/ ",
    "http://192.168.1.20:8000/v1",
  ])("accepts %s", (url) => {
    expect(validateSttBaseUrl(url)).toBeNull();
  });

  it.each(["", "   ", "localhost:11434/v1", "not a url", "ftp://host/v1"])(
    "rejects %j",
    (url) => {
      expect(validateSttBaseUrl(url)).not.toBeNull();
    },
  );
});
