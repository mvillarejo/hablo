export type SttProvider = "gladia" | "openai_compat";
export type GladiaRegion = "auto" | "eu-west" | "us-west";

export const DEFAULT_STT_BASE_URL = "http://localhost:11434/v1";
export const DEFAULT_STT_MODEL = "gemma4:e4b";
export const DEFAULT_ENDPOINTING = 0.1;
export const DEFAULT_LOCAL_PROFILE_NAME = "Local Ollama";
export const DEFAULT_GLADIA_PROFILE_NAME = "Gladia";
const MAX_PROFILE_NAME_CHARS = 60;

export interface GladiaProfileSettings {
  kind: "gladia";
  api_key: string;
  region: GladiaRegion;
  endpointing: number;
  code_switching: boolean;
}

export interface OpenAiCompatProfileSettings {
  kind: "openai_compat";
  base_url: string;
  model: string;
  api_key: string;
}

export type ProfileSettings =
  GladiaProfileSettings | OpenAiCompatProfileSettings;

/** Shape exchanged with the profile commands (snake_case, as on disk). */
export type Profile = { id: string; name: string } & ProfileSettings;
export type NewProfile = { name: string } & ProfileSettings;

export interface ProfilesState {
  profiles: Profile[];
  active_profile_id: string;
}

export const EMPTY_PROFILES_STATE: ProfilesState = {
  profiles: [],
  active_profile_id: "",
};

export const PROFILE_KIND_LABELS: Record<SttProvider, string> = {
  gladia: "Gladia",
  openai_compat: "OpenAI-compatible",
};

export function getActiveProfile(state: ProfilesState): Profile | null {
  return (
    state.profiles.find((profile) => profile.id === state.active_profile_id) ??
    null
  );
}

export function defaultProfileSettings(kind: SttProvider): ProfileSettings {
  return kind === "gladia"
    ? {
        kind,
        api_key: "",
        region: "auto",
        endpointing: DEFAULT_ENDPOINTING,
        code_switching: false,
      }
    : {
        kind,
        base_url: DEFAULT_STT_BASE_URL,
        model: DEFAULT_STT_MODEL,
        api_key: "",
      };
}

/** `base`, or `base 2`, `base 3`… — the first name no profile uses yet. */
export function uniqueProfileName(base: string, profiles: Profile[]): string {
  const taken = new Set(profiles.map((p) => p.name.trim().toLowerCase()));
  for (let n = 1; ; n++) {
    const candidate = n === 1 ? base : `${base} ${n}`;
    if (!taken.has(candidate.toLowerCase())) return candidate;
  }
}

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

/** A Gladia profile needs a saved API key; an OpenAI-compatible one does not. */
export function canDictate(profile: Profile | null): boolean {
  if (!profile) return false;
  return isBatchProvider(profile.kind) || profile.api_key.trim().length > 0;
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

export interface ProfileDraftErrors {
  name?: string;
  base_url?: string;
  model?: string;
}

/** Mirrors the backend rules so the form can explain them before saving. */
export function validateProfileDraft(
  draft: NewProfile,
  profiles: Profile[],
  editingId: string | null,
): ProfileDraftErrors {
  const errors: ProfileDraftErrors = {};
  const name = draft.name.trim();
  if (!name) {
    errors.name = "Name is required";
  } else if (name.length > MAX_PROFILE_NAME_CHARS) {
    errors.name = `Use at most ${MAX_PROFILE_NAME_CHARS} characters`;
  } else if (
    profiles.some(
      (p) =>
        p.id !== editingId &&
        p.name.trim().toLowerCase() === name.toLowerCase(),
    )
  ) {
    errors.name = "Another profile already uses this name";
  }
  if (draft.kind === "openai_compat") {
    const urlError = validateSttBaseUrl(draft.base_url);
    if (urlError) errors.base_url = urlError;
    if (!draft.model.trim()) errors.model = "Model is required";
  }
  return errors;
}

export function hasDraftErrors(errors: ProfileDraftErrors): boolean {
  return Object.keys(errors).length > 0;
}
