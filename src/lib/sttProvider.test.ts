import { describe, expect, it } from "vitest";
import {
  canDictate,
  defaultProfileSettings,
  getActiveProfile,
  hasDraftErrors,
  sessionEndFallbackMs,
  uniqueProfileName,
  validateProfileDraft,
  validateSttBaseUrl,
  type NewProfile,
  type Profile,
  type ProfilesState,
} from "./sttProvider";

const gladia = (id: string, name: string, apiKey: string): Profile => ({
  id,
  name,
  ...defaultProfileSettings("gladia"),
  api_key: apiKey,
});
const local = (id: string, name: string): Profile => ({
  id,
  name,
  ...defaultProfileSettings("openai_compat"),
});
const state = (active: string, ...profiles: Profile[]): ProfilesState => ({
  profiles,
  active_profile_id: active,
});

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
    expect(canDictate(gladia("g", "G", ""))).toBe(false);
    expect(canDictate(gladia("g", "G", "   "))).toBe(false);
    expect(canDictate(gladia("g", "G", "key"))).toBe(true);
    expect(canDictate(local("l", "L"))).toBe(true);
    expect(canDictate(null)).toBe(false);
  });

  it("follows the active profile, not any other profile", () => {
    const keyed = gladia("g", "Work", "key");
    const keyless = gladia("h", "Personal", "");
    const ollama = local("l", "Ollama");
    expect(canDictate(getActiveProfile(state("h", keyed, keyless)))).toBe(
      false,
    );
    expect(canDictate(getActiveProfile(state("g", keyed, keyless)))).toBe(true);
    expect(canDictate(getActiveProfile(state("l", keyless, ollama)))).toBe(
      true,
    );
    expect(canDictate(getActiveProfile(state("gone", keyed)))).toBe(false);
  });
});

describe("timeout by active profile kind", () => {
  it("uses the active profile's kind", () => {
    const s = state("l", gladia("g", "G", "k"), local("l", "L"));
    expect(sessionEndFallbackMs(getActiveProfile(s)!.kind, 60)).toBe(180_000);
    s.active_profile_id = "g";
    expect(sessionEndFallbackMs(getActiveProfile(s)!.kind, 60)).toBe(5_000);
  });
});

describe("validateProfileDraft", () => {
  const profiles = [gladia("g", "Gladia", "k"), local("l", "Local Ollama")];
  const draft = (patch: Partial<NewProfile> = {}): NewProfile =>
    ({
      name: "Groq",
      ...defaultProfileSettings("openai_compat"),
      ...patch,
    }) as NewProfile;

  it("accepts a valid new profile", () => {
    expect(validateProfileDraft(draft(), profiles, null)).toEqual({});
  });

  it.each(["", "   ", "gladia", " LOCAL ollama ", "x".repeat(61)])(
    "rejects the name %j",
    (name) => {
      expect(
        validateProfileDraft(draft({ name }), profiles, null).name,
      ).toBeDefined();
    },
  );

  it("lets a profile keep its own name when editing", () => {
    expect(
      validateProfileDraft(draft({ name: "local ollama" }), profiles, "l"),
    ).toEqual({});
  });

  it("checks base URL and model only for OpenAI-compatible profiles", () => {
    const errors = validateProfileDraft(
      draft({ base_url: "localhost:11434", model: " " } as Partial<NewProfile>),
      profiles,
      null,
    );
    expect(errors.base_url).toBeDefined();
    expect(errors.model).toBeDefined();
    expect(hasDraftErrors(errors)).toBe(true);
    const gladiaDraft: NewProfile = {
      name: "Gladia 2",
      ...defaultProfileSettings("gladia"),
    };
    expect(validateProfileDraft(gladiaDraft, profiles, null)).toEqual({});
  });
});

describe("uniqueProfileName", () => {
  it("numbers clashing names case-insensitively", () => {
    expect(uniqueProfileName("Groq", [])).toBe("Groq");
    expect(
      uniqueProfileName("Gladia", [
        gladia("a", "gladia", ""),
        gladia("b", "Gladia 2", ""),
      ]),
    ).toBe("Gladia 3");
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
