const MODIFIER_MAP: Record<string, string> = {
  MetaLeft: "Cmd",
  MetaRight: "Cmd",
  ShiftLeft: "Shift",
  ShiftRight: "Shift",
  ControlLeft: "Ctrl",
  ControlRight: "Ctrl",
  AltLeft: "Alt",
  AltRight: "Alt",
};

// Read before KeyboardEvent.key, since macOS Option composes: Opt+Space emits
// U+00A0, not " ". Position-invariant codes only — punctuation and digits move
// between layouts, so those must still come from the key.
const STABLE_LOGICAL_FROM_CODE: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  Escape: "Escape",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
};

/** Stable logical names for non-printable keys from KeyboardEvent.key */
const LOGICAL_KEY_FROM_EVENT_KEY: Record<string, string> = {
  " ": "Space",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Escape: "Escape",
};

const SPECIAL_KEY_MAP: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

const MODIFIER_SYMBOLS: Record<string, string> = {
  Cmd: "⌘",
  Shift: "⇧",
  Ctrl: "⌃",
  Alt: "⌥",
};

// Side-qualified single-modifier tokens, captured when the user binds a lone
// modifier (e.g. Right Command). Maps the browser KeyboardEvent.code → token,
// and each token → its base modifier for display/fallback.
const MODIFIER_CODE_TO_SIDE_TOKEN: Record<string, string> = {
  MetaLeft: "CmdLeft",
  MetaRight: "CmdRight",
  ControlLeft: "CtrlLeft",
  ControlRight: "CtrlRight",
  AltLeft: "AltLeft",
  AltRight: "AltRight",
  ShiftLeft: "ShiftLeft",
  ShiftRight: "ShiftRight",
};

const SIDE_TOKEN_TO_BASE: Record<string, string> = {
  CmdLeft: "Cmd",
  CmdRight: "Cmd",
  CtrlLeft: "Ctrl",
  CtrlRight: "Ctrl",
  AltLeft: "Alt",
  AltRight: "Alt",
  ShiftLeft: "Shift",
  ShiftRight: "Shift",
};

// Non-modifier keys that may be bound on their own (macOS only), keyed by the
// physical KeyboardEvent.code so the token doesn't depend on the layout. Keys
// that type text in normal use (letters, digits, Space…) are excluded on purpose.
const BARE_KEY_FROM_CODE: Record<string, string> = {
  IntlBackslash: "Section", // § on Mac ISO keyboards
  F13: "F13",
  F14: "F14",
  F15: "F15",
  F16: "F16",
  F17: "F17",
  F18: "F18",
  F19: "F19",
};

const KEY_SYMBOLS: Record<string, string> = {
  Section: "§",
};

/** Stable token for a key that can be a hotkey on its own (e.g. § → "Section"), else null. */
export function bareKeyToken(event: KeyboardEvent): string | null {
  return BARE_KEY_FROM_CODE[event.code] ?? null;
}

/** True for a hotkey that is a single allowlisted non-modifier key. */
export function isBareKeyToken(token: string): boolean {
  return Object.values(BARE_KEY_FROM_CODE).includes(token);
}

// Extra mouse buttons that may be the hotkey on their own (macOS only). Tokens use
// the human 1-based button number; left and right click (Mouse1/Mouse2) are never
// allowed, since swallowing them would make the mouse unusable.
export const MOUSE_BUTTON_TOKENS = [
  "Mouse3",
  "Mouse4",
  "Mouse5",
  "Mouse6",
  "Mouse7",
  "Mouse8",
];

const MOUSE_BUTTON_NAMES: Record<string, string> = {
  Mouse3: "Middle",
  Mouse4: "Back",
  Mouse5: "Forward",
};

/** True for a hotkey that is an allowlisted extra mouse button (e.g. "Mouse4"). */
export function isMouseButtonToken(token: string): boolean {
  return MOUSE_BUTTON_TOKENS.includes(token);
}

/** Display label for a mouse-button hotkey, e.g. "Mouse button 4 (Back)". */
export function mouseButtonLabel(token: string): string {
  const n = token.slice("Mouse".length);
  const name = MOUSE_BUTTON_NAMES[token];
  return name ? `Mouse button ${n} (${name})` : `Mouse button ${n}`;
}

/** Short label for the mouse-button picker, e.g. "Back (4)" or "Button 6". */
export function mouseButtonOptionLabel(token: string): string {
  const n = token.slice("Mouse".length);
  const name = MOUSE_BUTTON_NAMES[token];
  return name ? `${name} (${n})` : `Button ${n}`;
}

/**
 * Hotkey token for a DOM MouseEvent.button (0 left, 1 middle, 2 right, 3 back,
 * 4 forward…), else null. Left and right click never map to a token.
 */
export function mouseButtonToken(button: number): string | null {
  if (button === 0 || button === 2) return null;
  const token = button === 1 ? "Mouse3" : `Mouse${button + 1}`;
  return isMouseButtonToken(token) ? token : null;
}

/** Returns the side-qualified token for a modifier KeyboardEvent.code, else null. */
export function modifierSideToken(code: string): string | null {
  return MODIFIER_CODE_TO_SIDE_TOKEN[code] ?? null;
}

/** True for a hotkey that is a single modifier key (lone, possibly side-qualified). */
export function isSingleModifierToken(token: string): boolean {
  return token === "Fn" || token in SIDE_TOKEN_TO_BASE;
}

const MODIFIER_SORT_ORDER = ["Ctrl", "Alt", "Shift", "Cmd"];

/** Modifier token from a physical KeyboardEvent.code (left/right collapsed). */
export function normalizeModifierFromCode(code: string): string | null {
  return MODIFIER_MAP[code] ?? null;
}

/**
 * Logical non-modifier key token, read from KeyboardEvent.key so it follows the
 * active layout — except for position-invariant keys, which come from the code.
 * Returns null for modifiers, dead keys, IME composition, or unsupported keys.
 */
export function normalizeCapturedKey(event: KeyboardEvent): string | null {
  const { key, code } = event;

  const stable = STABLE_LOGICAL_FROM_CODE[code];
  if (stable) return stable;
  if (/^F\d{1,2}$/.test(code)) return code;

  if (key === "Dead" || key === "Process" || key === "Unidentified") {
    return null;
  }

  const named = LOGICAL_KEY_FROM_EVENT_KEY[key];
  if (named) return named;

  if (/^F\d{1,2}$/i.test(key)) {
    return key.toUpperCase();
  }

  if (key.length === 1) {
    if (/[a-zA-Z]/.test(key)) return key.toUpperCase();
    return key;
  }

  return null;
}

/** Compact debug string for a captured KeyboardEvent (physical code + layout key). */
export function formatKeyboardEventForLog(event: KeyboardEvent): string {
  const mods = [
    event.metaKey && "meta",
    event.ctrlKey && "ctrl",
    event.shiftKey && "shift",
    event.altKey && "alt",
  ]
    .filter(Boolean)
    .join("+");
  const modifierSuffix = mods ? ` modifiers=${mods}` : "";
  return `code=${event.code} key=${JSON.stringify(event.key)} location=${event.location}${modifierSuffix}`;
}

/** Modifier or logical key token for hotkey capture tracking. */
export function captureKeyToken(event: KeyboardEvent): string | null {
  const modifier = normalizeModifierFromCode(event.code);
  if (modifier) return modifier;
  return normalizeCapturedKey(event);
}

/** @deprecated Physical-code normalization; prefer captureKeyToken for hotkey capture. */
export function normalizeKey(code: string): string {
  if (MODIFIER_MAP[code]) return MODIFIER_MAP[code];
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  if (SPECIAL_KEY_MAP[code]) return SPECIAL_KEY_MAP[code];
  if (/^F\d{1,2}$/.test(code)) return code;
  return code;
}

export function isModifier(key: string): boolean {
  return ["Cmd", "Shift", "Ctrl", "Alt"].includes(key);
}

export function formatKeySymbol(key: string): string {
  if (isMouseButtonToken(key)) return mouseButtonLabel(key);
  const base = SIDE_TOKEN_TO_BASE[key];
  if (base) {
    const side = key.endsWith("Right") ? "R" : "L";
    return `${MODIFIER_SYMBOLS[base]}${side}`;
  }
  return MODIFIER_SYMBOLS[key] ?? KEY_SYMBOLS[key] ?? key;
}

export function sortKeys(keys: string[]): string[] {
  const mods = keys
    .filter(isModifier)
    .sort(
      (a, b) => MODIFIER_SORT_ORDER.indexOf(a) - MODIFIER_SORT_ORDER.indexOf(b),
    );
  const others = keys.filter((k) => !isModifier(k));
  return [...mods, ...others];
}

export function formatHotkeyLabel(hotkey: string): string {
  if (hotkey === "Fn") return "Fn";
  const parts = hotkey.split("+");
  return sortKeys(parts).map(formatKeySymbol).join(" ");
}

export function parseHotkeyParts(hotkey: string): string[] {
  if (hotkey === "Fn") return ["Fn"];
  return hotkey.split("+");
}
