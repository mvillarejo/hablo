import { invoke } from "@tauri-apps/api/core";
import type { ProfilesState } from "./sttProvider";

export const CONFIG_LOAD_ERROR_MESSAGE =
  "TTM couldn't read your saved settings. Your API key has not been changed.";
export const CONFIG_RESET_ERROR_MESSAGE =
  "TTM couldn't reset your settings. Your original settings file was not deleted.";

export type ProfilesLoadResult =
  { ok: true; profiles: ProfilesState } | { ok: false; message: string };

export async function loadSavedProfiles(
  invokeProfiles: () => Promise<ProfilesState> = () =>
    invoke<ProfilesState>("list_profiles"),
): Promise<ProfilesLoadResult> {
  try {
    return { ok: true, profiles: await invokeProfiles() };
  } catch {
    return { ok: false, message: CONFIG_LOAD_ERROR_MESSAGE };
  }
}

export type ConfigResetResult =
  { ok: true; backupPath: string } | { ok: false; message: string };

export async function resetCorruptedConfig(
  invokeReset: (confirmed: boolean) => Promise<string> = (confirmed) =>
    invoke<string>("reset_corrupted_config", { confirmed }),
): Promise<ConfigResetResult> {
  try {
    return { ok: true, backupPath: await invokeReset(true) };
  } catch {
    return { ok: false, message: CONFIG_RESET_ERROR_MESSAGE };
  }
}
