import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useState } from "react";
import {
  DEFAULT_GLADIA_PROFILE_NAME,
  DEFAULT_LOCAL_PROFILE_NAME,
  defaultProfileSettings,
  PROFILE_KIND_LABELS,
  uniqueProfileName,
  validateProfileDraft,
  type NewProfile,
  type Profile,
  type ProfilesState,
  type SttProvider,
} from "../lib/sttProvider";
import type { AppSettings } from "../types";
import { ConfirmDialog } from "./ConfirmDialog";
import { InfoTooltip } from "./InfoTooltip";
import { ProfileEditor, type ConnectionTest } from "./ProfileEditor";
import { SettingsPageLayout } from "./SettingsPageLayout";

type EditorTarget = { mode: "create" } | { mode: "edit"; id: string };

const IDLE_TEST: ConnectionTest = { state: "idle", message: "" };

function toDraft(profile: Profile): NewProfile {
  const { id: _id, ...draft } = profile;
  return draft as NewProfile;
}

const LANGUAGE_PAGE_SIZE = 5;

export function TranscriptionSettingsView({
  settings,
  setSettings,
  languageDropdownOpen,
  setLanguageDropdownOpen,
  languageDropdownRef,
  handleOpenDropdown,
  handleLanguageToggle,
  languageSearch,
  setLanguageSearch,
  filteredLanguageOptions,
  selectedLanguageSummary,
  profilesState,
  onProfilesChange,
  isDictating,
  onDone,
}: {
  settings: AppSettings;
  setSettings: React.Dispatch<React.SetStateAction<AppSettings>>;
  languageDropdownOpen: boolean;
  setLanguageDropdownOpen: (v: boolean) => void;
  languageDropdownRef: React.RefObject<HTMLDivElement | null>;
  handleOpenDropdown: (d: "languages") => void;
  handleLanguageToggle: (code: string, checked: boolean) => void;
  languageSearch: string;
  setLanguageSearch: (v: string) => void;
  filteredLanguageOptions: readonly { code: string; label: string }[];
  selectedLanguageSummary: string;
  profilesState: ProfilesState;
  onProfilesChange: (state: ProfilesState) => void;
  isDictating: boolean;
  onDone: () => void;
}) {
  const { profiles, active_profile_id: activeId } = profilesState;
  const findProfile = (id: string) => profiles.find((p) => p.id === id);

  const [editor, setEditor] = useState<EditorTarget | null>(null);
  const [draft, setDraft] = useState<NewProfile | null>(null);
  const radioGroupName = useId();
  const [isSaving, setIsSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [connectionTest, setConnectionTest] =
    useState<ConnectionTest>(IDLE_TEST);
  const [listError, setListError] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Profile | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const editingProfile =
    editor?.mode === "edit" ? findProfile(editor.id) : undefined;
  const draftErrors = draft
    ? validateProfileDraft(
        draft,
        profiles,
        editor?.mode === "edit" ? editor.id : null,
      )
    : {};
  const isDirty =
    !!draft &&
    (!editingProfile ||
      JSON.stringify(draft) !== JSON.stringify(toDraft(editingProfile)));

  const closeEditor = () => {
    setEditor(null);
    setDraft(null);
    setSaveError(null);
    setConnectionTest(IDLE_TEST);
  };

  const openEditor = (profile: Profile) => {
    setEditor({ mode: "edit", id: profile.id });
    setDraft(toDraft(profile));
    setSaveError(null);
    setConnectionTest(IDLE_TEST);
  };

  const defaultNameFor = (kind: SttProvider) =>
    uniqueProfileName(
      kind === "gladia"
        ? DEFAULT_GLADIA_PROFILE_NAME
        : DEFAULT_LOCAL_PROFILE_NAME,
      profiles,
    );

  const startCreate = (kind: SttProvider) => {
    setEditor({ mode: "create" });
    setDraft({ name: defaultNameFor(kind), ...defaultProfileSettings(kind) });
    setSaveError(null);
    setConnectionTest(IDLE_TEST);
  };

  /** New profiles pick their kind first; keep a name the user typed. */
  const changeDraftKind = (kind: SttProvider) => {
    if (!draft || draft.kind === kind) return;
    const name =
      draft.name === defaultNameFor(draft.kind)
        ? defaultNameFor(kind)
        : draft.name;
    setDraft({ name, ...defaultProfileSettings(kind) });
    setSaveError(null);
  };

  const handleSave = async () => {
    if (!draft || !editor) return;
    setIsSaving(true);
    setSaveError(null);
    try {
      if (editor.mode === "create") {
        const next = await invoke<ProfilesState>("create_profile", {
          profile: draft,
        });
        onProfilesChange(next);
        const created = next.profiles[next.profiles.length - 1];
        setEditor({ mode: "edit", id: created.id });
        setDraft(toDraft(created));
      } else {
        const next = await invoke<ProfilesState>("update_profile", {
          profile: { ...draft, id: editor.id },
        });
        onProfilesChange(next);
        const saved = next.profiles.find((p) => p.id === editor.id);
        if (saved) setDraft(toDraft(saved));
      }
      setConnectionTest(IDLE_TEST);
    } catch (error) {
      setSaveError(String(error));
    } finally {
      setIsSaving(false);
    }
  };

  const handleTest = async () => {
    if (editor?.mode !== "edit") return;
    setConnectionTest({ state: "testing", message: "" });
    try {
      const message = await invoke<string>("test_profile_connection", {
        profileId: editor.id,
      });
      setConnectionTest({ state: "ok", message });
    } catch (error) {
      setConnectionTest({ state: "error", message: String(error) });
    }
  };

  const handleSetActive = async (id: string) => {
    if (id === activeId) return;
    setListError(null);
    try {
      onProfilesChange(
        await invoke<ProfilesState>("set_active_profile", { profileId: id }),
      );
    } catch (error) {
      setListError(String(error));
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setIsDeleting(true);
    setDeleteError(null);
    try {
      onProfilesChange(
        await invoke<ProfilesState>("delete_profile", {
          profileId: deleteTarget.id,
        }),
      );
      if (editor?.mode === "edit" && editor.id === deleteTarget.id) {
        closeEditor();
      }
      setDeleteTarget(null);
    } catch (error) {
      setDeleteError(String(error));
    } finally {
      setIsDeleting(false);
    }
  };
  const [languagePage, setLanguagePage] = useState(1);
  const totalLanguagePages = Math.max(
    1,
    Math.ceil(filteredLanguageOptions.length / LANGUAGE_PAGE_SIZE),
  );
  const visibleLanguageOptions = filteredLanguageOptions.slice(
    (languagePage - 1) * LANGUAGE_PAGE_SIZE,
    languagePage * LANGUAGE_PAGE_SIZE,
  );

  useEffect(() => {
    setLanguagePage(1);
  }, [languageSearch]);

  useEffect(() => {
    setLanguagePage((page) => Math.min(page, totalLanguagePages));
  }, [totalLanguagePages]);

  return (
    <SettingsPageLayout title="Transcription settings">
      <div className="form-group">
        <label className="form-label" id="profiles-label">
          Profiles
          <InfoTooltip label="About profiles">
            <strong>Profiles</strong>
            Each profile is a saved transcription setup. Gladia streams live and
            shows text while you speak. An OpenAI-compatible endpoint (local
            Ollama, Groq, OpenAI) transcribes the whole recording after you
            stop; with a local model the audio never leaves this Mac. You can
            also switch profiles from the menu bar icon.
          </InfoTooltip>
        </label>
        <div
          className="profile-list"
          role="radiogroup"
          aria-labelledby="profiles-label"
        >
          {profiles.map((profile) => {
            const isActive = profile.id === activeId;
            const deleteBlocked = isActive || profiles.length <= 1;
            return (
              <div
                key={profile.id}
                className={`profile-row${isActive ? " profile-row--active" : ""}${
                  editor?.mode === "edit" && editor.id === profile.id
                    ? " profile-row--editing"
                    : ""
                }`}
              >
                <label className="profile-row-main">
                  <input
                    type="radio"
                    name={radioGroupName}
                    className="profile-radio"
                    checked={isActive}
                    onChange={() => void handleSetActive(profile.id)}
                  />
                  <span className="profile-row-name">{profile.name}</span>
                  <span className="profile-kind-badge">
                    {PROFILE_KIND_LABELS[profile.kind]}
                  </span>
                  {isActive && (
                    <span className="profile-kind-badge profile-kind-badge--active">
                      Active
                    </span>
                  )}
                </label>
                <div className="profile-row-actions">
                  <button
                    type="button"
                    className="btn btn-ghost btn-sm"
                    aria-label={`Edit profile ${profile.name}`}
                    onClick={() => openEditor(profile)}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost btn-sm"
                    aria-label={`Delete profile ${profile.name}`}
                    disabled={deleteBlocked}
                    title={
                      profiles.length <= 1
                        ? "You need at least one profile"
                        : isActive
                          ? "Switch to another profile before deleting this one"
                          : undefined
                    }
                    onClick={() => {
                      setDeleteError(null);
                      setDeleteTarget(profile);
                    }}
                  >
                    Delete
                  </button>
                </div>
              </div>
            );
          })}
        </div>
        {listError && (
          <p className="form-footnote form-footnote--danger" role="alert">
            {listError}
          </p>
        )}
        {isDictating && (
          <p className="form-footnote text-secondary">
            Profile changes apply from your next dictation.
          </p>
        )}
      </div>

      {editor && draft && (
        <ProfileEditor
          draft={draft}
          onChange={(next) => {
            setDraft(next);
            setSaveError(null);
            setConnectionTest(IDLE_TEST);
          }}
          errors={draftErrors}
          isNew={editor.mode === "create"}
          isDirty={isDirty}
          isSaving={isSaving}
          saveError={saveError}
          connectionTest={connectionTest}
          onSave={() => void handleSave()}
          onCancel={closeEditor}
          onTest={() => void handleTest()}
          onKindChange={editor.mode === "create" ? changeDraftKind : undefined}
        />
      )}

      <div>
        <div className="form-group">
          <label className="form-label">
            Languages
            <InfoTooltip label="About languages">
              <strong>Languages</strong>
              Shared by every profile, like your custom vocabulary.
            </InfoTooltip>
          </label>
          <div
            className={`multi-select ${languageDropdownOpen ? "open" : ""}`}
            ref={languageDropdownRef}
          >
            <button
              type="button"
              className="form-input multi-select-trigger"
              onClick={() => {
                if (languageDropdownOpen) {
                  setLanguageDropdownOpen(false);
                } else {
                  handleOpenDropdown("languages");
                }
              }}
              aria-haspopup="listbox"
              aria-expanded={languageDropdownOpen}
            >
              <span>{selectedLanguageSummary}</span>
            </button>
            {languageDropdownOpen && (
              <div className="multi-select-dropdown">
                <input
                  type="text"
                  value={languageSearch}
                  onChange={(e) => setLanguageSearch(e.target.value)}
                  className="form-input multi-select-search"
                  placeholder="Search languages..."
                  autoCorrect="off"
                  autoCapitalize="none"
                  autoComplete="off"
                  spellCheck={false}
                />
                <div
                  className="multi-select-options"
                  role="listbox"
                  aria-multiselectable="true"
                >
                  {filteredLanguageOptions.length > 0 ? (
                    visibleLanguageOptions.map((language) => (
                      <label
                        key={language.code}
                        className="form-checkbox multi-select-option"
                      >
                        <input
                          type="checkbox"
                          checked={settings.languages.includes(language.code)}
                          onChange={(e) =>
                            handleLanguageToggle(
                              language.code,
                              e.target.checked,
                            )
                          }
                        />
                        {language.label}
                      </label>
                    ))
                  ) : (
                    <div className="multi-select-empty">
                      No matching languages
                    </div>
                  )}
                </div>
                {filteredLanguageOptions.length > LANGUAGE_PAGE_SIZE && (
                  <div className="language-pagination" aria-live="polite">
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      disabled={languagePage <= 1}
                      onClick={() => setLanguagePage((page) => page - 1)}
                    >
                      Previous
                    </button>
                    <span>
                      {languagePage} / {totalLanguagePages}
                    </span>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      disabled={languagePage >= totalLanguagePages}
                      onClick={() => setLanguagePage((page) => page + 1)}
                    >
                      Next
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      </div>

      <div className="setup-nav setup-nav-center">
        <button
          type="button"
          className="btn btn-ghost"
          onClick={() => startCreate("openai_compat")}
        >
          Add profile
        </button>
        <button className="btn btn-primary" onClick={onDone}>
          Done
        </button>
      </div>
      <ConfirmDialog
        open={deleteTarget !== null}
        title={`Delete "${deleteTarget?.name ?? ""}"?`}
        confirmLabel="Delete profile"
        busyLabel="Deleting..."
        isBusy={isDeleting}
        error={deleteError}
        onCancel={() => {
          setDeleteTarget(null);
          setDeleteError(null);
        }}
        onConfirm={() => void handleDelete()}
      >
        <p>
          This removes the profile and any API key saved in it. Your languages
          and vocabulary are not affected.
        </p>
      </ConfirmDialog>
    </SettingsPageLayout>
  );
}
