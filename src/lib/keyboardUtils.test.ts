import { describe, it, expect } from "vitest";
import {
  normalizeCapturedKey,
  captureKeyToken,
  normalizeModifierFromCode,
  formatHotkeyLabel,
  formatKeySymbol,
  parseHotkeyParts,
  bareKeyToken,
  isBareKeyToken,
} from "./keyboardUtils";

function keyEvent(
  key: string,
  code: string,
  overrides: Partial<KeyboardEvent> = {},
): KeyboardEvent {
  return {
    key,
    code,
    metaKey: false,
    shiftKey: false,
    ctrlKey: false,
    altKey: false,
    ...overrides,
  } as KeyboardEvent;
}

describe("normalizeCapturedKey", () => {
  it("uses logical letter from event.key (AZERTY A on KeyQ)", () => {
    expect(normalizeCapturedKey(keyEvent("a", "KeyQ"))).toBe("A");
    expect(normalizeCapturedKey(keyEvent("A", "KeyQ"))).toBe("A");
  });

  it("preserves punctuation from event.key", () => {
    expect(normalizeCapturedKey(keyEvent(";", "Semicolon"))).toBe(";");
    expect(normalizeCapturedKey(keyEvent("é", "KeyQ"))).toBe("é");
  });

  it("maps named special keys", () => {
    expect(normalizeCapturedKey(keyEvent(" ", "Space"))).toBe("Space");
    expect(normalizeCapturedKey(keyEvent("ArrowUp", "ArrowUp"))).toBe("Up");
    expect(normalizeCapturedKey(keyEvent("F5", "F5"))).toBe("F5");
  });

  it("resolves position-invariant keys from the code, not the character", () => {
    expect(
      normalizeCapturedKey(keyEvent("\u00A0", "Space", { altKey: true })),
    ).toBe("Space");
    expect(
      normalizeCapturedKey(keyEvent("ArrowUp", "ArrowUp", { altKey: true })),
    ).toBe("Up");
    expect(normalizeCapturedKey(keyEvent("F5", "F5", { altKey: true }))).toBe(
      "F5",
    );
  });

  it("keeps reading layout-dependent keys from the character", () => {
    // Semicolon is `m` and Digit1 is `&` on AZERTY.
    expect(normalizeCapturedKey(keyEvent("m", "Semicolon"))).toBe("M");
    expect(normalizeCapturedKey(keyEvent("&", "Digit1"))).toBe("&");
  });

  it("rejects dead keys and IME", () => {
    expect(normalizeCapturedKey(keyEvent("Dead", "KeyQ"))).toBeNull();
    expect(normalizeCapturedKey(keyEvent("Process", "KeyA"))).toBeNull();
  });
});

describe("captureKeyToken", () => {
  it("returns modifier tokens from code", () => {
    expect(captureKeyToken(keyEvent("Meta", "MetaLeft"))).toBe("Cmd");
    expect(captureKeyToken(keyEvent("Shift", "ShiftRight"))).toBe("Shift");
  });

  it("returns logical key for composites", () => {
    expect(captureKeyToken(keyEvent("a", "KeyQ"))).toBe("A");
  });
});

describe("normalizeModifierFromCode", () => {
  it("maps physical modifier codes", () => {
    expect(normalizeModifierFromCode("MetaLeft")).toBe("Cmd");
    expect(normalizeModifierFromCode("ControlRight")).toBe("Ctrl");
  });
});

describe("formatHotkeyLabel", () => {
  it("formats logical shortcuts", () => {
    expect(formatHotkeyLabel("Cmd+A")).toBe("⌘ A");
    expect(formatHotkeyLabel("Ctrl+Shift+;")).toBe("⌃ ⇧ ;");
  });

  it("shows the Section token as §", () => {
    expect(formatHotkeyLabel("Section")).toBe("§");
    expect(formatKeySymbol("Section")).toBe("§");
    expect(parseHotkeyParts("Section")).toEqual(["Section"]);
  });
});

describe("bareKeyToken", () => {
  it("serializes the § key as Section whatever the layout prints", () => {
    expect(bareKeyToken(keyEvent("§", "IntlBackslash"))).toBe("Section");
    expect(bareKeyToken(keyEvent("±", "IntlBackslash"))).toBe("Section");
    expect(bareKeyToken(keyEvent("<", "IntlBackslash"))).toBe("Section");
  });

  it("allows F13–F19 on their own", () => {
    expect(bareKeyToken(keyEvent("F13", "F13"))).toBe("F13");
    expect(bareKeyToken(keyEvent("F19", "F19"))).toBe("F19");
  });

  it("rejects keys that would block typing", () => {
    expect(bareKeyToken(keyEvent("a", "KeyA"))).toBeNull();
    expect(bareKeyToken(keyEvent("1", "Digit1"))).toBeNull();
    expect(bareKeyToken(keyEvent(" ", "Space"))).toBeNull();
    expect(bareKeyToken(keyEvent("F12", "F12"))).toBeNull();
    // § typed from another physical key (e.g. Shift+3 on German) is not the § key.
    expect(bareKeyToken(keyEvent("§", "Digit3"))).toBeNull();
  });

  it("recognises stored bare tokens", () => {
    expect(isBareKeyToken("Section")).toBe(true);
    expect(isBareKeyToken("F15")).toBe(true);
    expect(isBareKeyToken("§")).toBe(false);
    expect(isBareKeyToken("A")).toBe(false);
  });
});
