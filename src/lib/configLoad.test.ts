import { describe, expect, it, vi } from "vitest";
import { loadSavedProfiles, resetCorruptedConfig } from "./configLoad";
import type { ProfilesState } from "./sttProvider";

const keyless: ProfilesState = {
  profiles: [
    {
      id: "local-ollama",
      name: "Local Ollama",
      kind: "openai_compat",
      base_url: "http://localhost:11434/v1",
      model: "gemma4:e4b",
      api_key: "",
    },
  ],
  active_profile_id: "local-ollama",
};

describe("loadSavedProfiles", () => {
  it("distinguishes a keyless setup from a load failure", async () => {
    await expect(loadSavedProfiles(async () => keyless)).resolves.toEqual({
      ok: true,
      profiles: keyless,
    });

    const failure = await loadSavedProfiles(async () => {
      throw new Error("malformed config");
    });
    expect(failure).toEqual({
      ok: false,
      message:
        "TTM couldn't read your saved settings. Your API key has not been changed.",
    });
  });

  it("returns the saved profiles without logging their keys", async () => {
    const saved: ProfilesState = {
      profiles: [
        {
          id: "gladia",
          name: "Gladia",
          kind: "gladia",
          api_key: "secret-key",
          region: "auto",
          endpointing: 0.1,
          code_switching: false,
        },
      ],
      active_profile_id: "gladia",
    };
    const invokeProfiles = vi.fn(async () => saved);
    const log = vi.spyOn(console, "log");

    await expect(loadSavedProfiles(invokeProfiles)).resolves.toEqual({
      ok: true,
      profiles: saved,
    });
    expect(invokeProfiles).toHaveBeenCalledOnce();
    expect(log).not.toHaveBeenCalled();
  });

  it("passes explicit confirmation only when reset is called", async () => {
    const invokeReset = vi.fn(async (confirmed: boolean) => "/backup.json");

    await expect(resetCorruptedConfig(invokeReset)).resolves.toEqual({
      ok: true,
      backupPath: "/backup.json",
    });
    expect(invokeReset).toHaveBeenCalledWith(true);
  });

  it("returns a safe reset error without claiming settings were deleted", async () => {
    const result = await resetCorruptedConfig(async () => {
      throw new Error("disk failure");
    });

    expect(result).toEqual({
      ok: false,
      message:
        "TTM couldn't reset your settings. Your original settings file was not deleted.",
    });
  });
});
