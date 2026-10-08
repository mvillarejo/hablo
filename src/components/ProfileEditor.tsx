import { useEffect, useRef } from "react";
import {
  PROFILE_KIND_LABELS,
  type GladiaRegion,
  type NewProfile,
  type ProfileDraftErrors,
  type SttProvider,
} from "../lib/sttProvider";
import { InfoTooltip } from "./InfoTooltip";

export type ConnectionTest = {
  state: "idle" | "testing" | "ok" | "error";
  message: string;
};

const REGION_OPTIONS: { value: GladiaRegion; label: string }[] = [
  { value: "auto", label: "Automatic" },
  { value: "eu-west", label: "Europe (eu-west)" },
  { value: "us-west", label: "US (us-west)" },
];

const inputProps = {
  autoCorrect: "off",
  autoCapitalize: "none",
  autoComplete: "off",
  spellCheck: false,
} as const;

export function ProfileEditor({
  draft,
  onChange,
  errors,
  isNew,
  isDirty,
  isSaving,
  saveError,
  connectionTest,
  onSave,
  onCancel,
  onTest,
  onKindChange,
}: {
  draft: NewProfile;
  onChange: (draft: NewProfile) => void;
  errors: ProfileDraftErrors;
  isNew: boolean;
  isDirty: boolean;
  isSaving: boolean;
  saveError: string | null;
  connectionTest: ConnectionTest;
  onSave: () => void;
  onCancel: () => void;
  onTest: () => void;
  /** Only for new profiles: the kind is chosen first and fixed afterwards. */
  onKindChange?: (kind: SttProvider) => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const nameInputRef = useRef<HTMLInputElement>(null);

  // Same modal lifecycle as the vocabulary editor.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    window.requestAnimationFrame(() => nameInputRef.current?.focus());
    return () => dialog.close();
  }, []);

  const update = (patch: Partial<NewProfile>) =>
    onChange({ ...draft, ...patch } as NewProfile);
  const hasErrors = Object.keys(errors).length > 0;
  const canTest = !isNew && !isDirty && connectionTest.state !== "testing";

  return (
    <dialog
      ref={dialogRef}
      className="vocab-editor-dialog profile-editor-dialog"
      aria-labelledby="profile-editor-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!isSaving) onCancel();
      }}
    >
      <div className="vocab-editor-header">
        <div>
          <h3 id="profile-editor-title">
            {isNew ? "Add profile" : "Edit profile"}
          </h3>
          <p>
            {PROFILE_KIND_LABELS[draft.kind]}
            {draft.kind === "gladia"
              ? " · live streaming"
              : " · transcribes after you stop"}
          </p>
        </div>
        <button
          type="button"
          className="vocab-editor-close"
          onClick={onCancel}
          disabled={isSaving}
          aria-label="Close profile editor"
        >
          ×
        </button>
      </div>

      <div className="profile-editor-body">
        {onKindChange && (
          <div
            className="profile-kind-picker"
            role="radiogroup"
            aria-label="Profile type"
          >
            {(["openai_compat", "gladia"] as const).map((kind) => (
              <button
                key={kind}
                type="button"
                role="radio"
                aria-checked={draft.kind === kind}
                className={`btn btn-sm ${
                  draft.kind === kind ? "btn-primary" : "btn-ghost"
                }`}
                onClick={() => onKindChange(kind)}
              >
                {PROFILE_KIND_LABELS[kind]}
              </button>
            ))}
          </div>
        )}
        <div className="form-group">
          <label className="form-label" htmlFor="profile-name">
            Name
          </label>
          <input
            id="profile-name"
            ref={nameInputRef}
            type="text"
            className={`form-input${errors.name ? " form-input--error" : ""}`}
            value={draft.name}
            placeholder={
              draft.kind === "gladia" ? "e.g. Gladia work" : "e.g. Groq"
            }
            onChange={(e) => update({ name: e.target.value })}
            {...inputProps}
          />
          {errors.name && (
            <p className="form-footnote form-footnote--danger">{errors.name}</p>
          )}
        </div>

        {draft.kind === "gladia" ? (
          <>
            <div className="settings-row">
              <div className="form-group">
                <label className="form-label" htmlFor="profile-gladia-key">
                  API key
                </label>
                <input
                  id="profile-gladia-key"
                  type="password"
                  className="api-key-input"
                  value={draft.api_key}
                  placeholder="Gladia API key"
                  onChange={(e) => update({ api_key: e.target.value })}
                  {...inputProps}
                />
              </div>
              <div className="form-group">
                <label className="form-label" htmlFor="profile-region">
                  Region
                </label>
                <select
                  id="profile-region"
                  className="form-input"
                  value={draft.region}
                  onChange={(e) =>
                    update({ region: e.target.value as GladiaRegion })
                  }
                >
                  {REGION_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </div>
            </div>
            <div className="settings-row">
              <div className="form-group">
                <label className="form-label">
                  Endpointing
                  <InfoTooltip label="About endpointing">
                    <strong>Endpointing</strong>
                    How long (in seconds) Gladia waits for silence before
                    treating an utterance as finished. Lower values feel
                    snappier but may cut sentences short; higher values wait
                    longer so pauses don&apos;t split your speech.
                  </InfoTooltip>
                </label>
                <div className="endpointing-control">
                  <input
                    type="range"
                    className="endpointing-slider"
                    aria-label="Endpointing"
                    min={0.05}
                    max={1}
                    step={0.05}
                    value={draft.endpointing}
                    onChange={(e) =>
                      update({ endpointing: parseFloat(e.target.value) })
                    }
                  />
                  <span className="endpointing-value">
                    {draft.endpointing.toFixed(2)}s
                  </span>
                </div>
              </div>
              <div className="form-group">
                <label className="form-label">Code switching</label>
                <label className="toggle-switch" title="Code switching">
                  <input
                    type="checkbox"
                    aria-label="Code switching"
                    checked={draft.code_switching}
                    onChange={(e) =>
                      update({ code_switching: e.target.checked })
                    }
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
            </div>
          </>
        ) : (
          <>
            <div className="form-group">
              <label className="form-label" htmlFor="profile-base-url">
                Base URL
              </label>
              <input
                id="profile-base-url"
                type="text"
                className={`form-input${errors.base_url ? " form-input--error" : ""}`}
                value={draft.base_url}
                placeholder="http://localhost:11434/v1"
                onChange={(e) => update({ base_url: e.target.value })}
                {...inputProps}
              />
              {errors.base_url && (
                <p className="form-footnote form-footnote--danger">
                  {errors.base_url}
                </p>
              )}
            </div>
            <div className="settings-row">
              <div className="form-group">
                <label className="form-label" htmlFor="profile-model">
                  Model
                </label>
                <input
                  id="profile-model"
                  type="text"
                  className={`form-input${errors.model ? " form-input--error" : ""}`}
                  value={draft.model}
                  placeholder="gemma4:e4b"
                  onChange={(e) => update({ model: e.target.value })}
                  {...inputProps}
                />
                {errors.model && (
                  <p className="form-footnote form-footnote--danger">
                    {errors.model}
                  </p>
                )}
              </div>
              <div className="form-group">
                <label className="form-label" htmlFor="profile-openai-key">
                  API key
                </label>
                <input
                  id="profile-openai-key"
                  type="password"
                  className="api-key-input"
                  value={draft.api_key}
                  placeholder="Optional for Ollama"
                  onChange={(e) => update({ api_key: e.target.value })}
                  {...inputProps}
                />
              </div>
            </div>
          </>
        )}

        {saveError && (
          <p className="form-footnote form-footnote--danger" role="alert">
            {saveError}
          </p>
        )}
        {connectionTest.message && (
          <p
            className={`form-footnote ${
              connectionTest.state === "error"
                ? "form-footnote--danger"
                : "text-secondary"
            }`}
            role="status"
          >
            {connectionTest.message}
          </p>
        )}
      </div>

      <div className="profile-editor-actions">
        {!isNew && (
          <button
            type="button"
            className="btn btn-ghost"
            onClick={onTest}
            disabled={!canTest}
            title={isDirty ? "Save your changes to test them" : undefined}
          >
            {connectionTest.state === "testing" ? (
              <>
                <span className="btn-spinner" aria-hidden="true" />
                Testing...
              </>
            ) : (
              "Test connection"
            )}
          </button>
        )}
        <span className="profile-editor-spacer" />
        <button
          type="button"
          className="btn btn-ghost"
          onClick={onCancel}
          disabled={isSaving}
        >
          {isNew || isDirty ? "Cancel" : "Close"}
        </button>
        <button
          type="button"
          className="btn btn-primary"
          onClick={onSave}
          disabled={isSaving || hasErrors || (!isNew && !isDirty)}
        >
          {isSaving ? "Saving..." : isNew ? "Add profile" : "Save"}
        </button>
      </div>
    </dialog>
  );
}
