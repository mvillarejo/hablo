export type SttProvider = "gladia" | "openai_compat";

export const DEFAULT_STT_BASE_URL = "http://localhost:11434/v1";
export const DEFAULT_STT_MODEL = "gemma4:e4b";

/** Shape exchanged with the `get_stt_settings` / `save_stt_settings` commands. */
export interface SttSettings {
  provider: SttProvider;
  baseUrl: string;
  model: string;
  apiKey: string;
}

export const DEFAULT_STT_SETTINGS: SttSettings = {
  provider: "gladia",
  baseUrl: DEFAULT_STT_BASE_URL,
  model: DEFAULT_STT_MODEL,
  apiKey: "",
};

/** Gladia streams and finalizes within a few seconds of stop. */
const GLADIA_SESSION_END_TIMEOUT_MS = 5_000;
/** Batch providers transcribe after stop, one ≤28 s chunk at a time; local
 * inference on a long chunk can take tens of seconds. */
const BATCH_TIMEOUT_PER_CHUNK_MS = 60_000;
const BATCH_CHUNK_SECONDS = 28;

export function isBatchProvider(provider: SttProvider): boolean {
  return provider === "openai_compat";
}

/** How long to wait for `session-ended` after stop before forcing a reset. */
export function sessionEndFallbackMs(
  provider: SttProvider,
  recordingSeconds = 0,
): number {
  if (!isBatchProvider(provider)) return GLADIA_SESSION_END_TIMEOUT_MS;
  const chunks = Math.max(
    1,
    Math.ceil(Math.max(0, recordingSeconds) / BATCH_CHUNK_SECONDS),
  );
  return chunks * BATCH_TIMEOUT_PER_CHUNK_MS;
}

/** Gladia needs an API key; a local OpenAI-compatible endpoint does not. */
export function canDictate(provider: SttProvider, gladiaApiKey: string) {
  return isBatchProvider(provider) || gladiaApiKey.trim().length > 0;
}

/** Returns an error message, or null when the base URL is usable. */
export function validateSttBaseUrl(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return "Base URL is required";
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return "Enter a full URL, e.g. http://localhost:11434/v1";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return "Base URL must start with http:// or https://";
  }
  return null;
}
