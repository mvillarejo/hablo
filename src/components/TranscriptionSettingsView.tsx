import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import {
  isBatchProvider,
  validateSttBaseUrl,
  type SttProvider,
  type SttSettings,
} from "../lib/sttProvider";
import type { AppSettings } from "../types";
import { InfoTooltip } from "./InfoTooltip";
import { SettingsPageLayout } from "./SettingsPageLayout";

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
  sttSettings,
  setSttSettings,
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
  sttSettings: SttSettings;
  setSttSettings: (settings: SttSettings) => void;
  onDone: () => void;
}) {
  const isBatch = isBatchProvider(sttSettings.provider);
  const baseUrlError = validateSttBaseUrl(sttSettings.baseUrl);
  const modelError = sttSettings.model.trim() ? null : "Model is required";
  const [connectionTest, setConnectionTest] = useState<{
    state: "idle" | "testing" | "ok" | "error";
    message: string;
  }>({ state: "idle", message: "" });
  const updateStt = (patch: Partial<SttSettings>) => {
    setConnectionTest({ state: "idle", message: "" });
    setSttSettings({ ...sttSettings, ...patch });
  };
  const handleTestConnection = async () => {
    setConnectionTest({ state: "testing", message: "" });
    try {
      const message = await invoke<string>("test_stt_connection", {
        settings: sttSettings,
      });
      setConnectionTest({ state: "ok", message });
    } catch (error) {
      setConnectionTest({ state: "error", message: String(error) });
    }
  };
  const endpointingValue = Number.isFinite(settings.endpointing)
    ? settings.endpointing
    : 0.1;
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
        <label className="form-label" htmlFor="stt-provider">
          Provider
          <InfoTooltip label="About providers">
            <strong>Provider</strong>
            Gladia streams live and shows text while you speak. An
            OpenAI-compatible endpoint (local Ollama by default) transcribes the
            whole recording after you stop; with a local model the audio never
            leaves this Mac.
          </InfoTooltip>
        </label>
        <select
          id="stt-provider"
          className="form-input"
          value={sttSettings.provider}
          onChange={(e) =>
            updateStt({ provider: e.target.value as SttProvider })
          }
        >
          <option value="gladia">Gladia (live streaming)</option>
          <option value="openai_compat">
            OpenAI-compatible (e.g. local Ollama)
          </option>
        </select>
      </div>
      <div className={isBatch ? undefined : "settings-row"}>
        <div className="form-group">
          <label className="form-label">Languages</label>
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
        {!isBatch && (
          <div className="form-group">
            <label className="form-label">Code switching</label>
            <label className="toggle-switch" title="Code switching">
              <input
                type="checkbox"
                checked={settings.codeSwitching}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    codeSwitching: e.target.checked,
                  })
                }
              />
              <span className="toggle-slider" />
            </label>
          </div>
        )}
      </div>

      {isBatch ? (
        <>
          <div className="form-group">
            <label className="form-label" htmlFor="stt-base-url">
              Base URL
            </label>
            <input
              id="stt-base-url"
              type="text"
              className={`form-input${baseUrlError ? " form-input--error" : ""}`}
              value={sttSettings.baseUrl}
              placeholder="http://localhost:11434/v1"
              onChange={(e) => updateStt({ baseUrl: e.target.value })}
              autoCorrect="off"
              autoCapitalize="none"
              autoComplete="off"
              spellCheck={false}
            />
            {baseUrlError && (
              <p className="form-footnote form-footnote--danger">
                {baseUrlError}
              </p>
            )}
          </div>
          <div className="settings-row">
            <div className="form-group">
              <label className="form-label" htmlFor="stt-model">
                Model
              </label>
              <input
                id="stt-model"
                type="text"
                className={`form-input${modelError ? " form-input--error" : ""}`}
                value={sttSettings.model}
                placeholder="gemma4:e4b"
                onChange={(e) => updateStt({ model: e.target.value })}
                autoCorrect="off"
                autoCapitalize="none"
                autoComplete="off"
                spellCheck={false}
              />
              {modelError && (
                <p className="form-footnote form-footnote--danger">
                  {modelError}
                </p>
              )}
            </div>
            <div className="form-group">
              <label className="form-label" htmlFor="stt-api-key">
                API key
              </label>
              <input
                id="stt-api-key"
                type="password"
                className="api-key-input"
                value={sttSettings.apiKey}
                placeholder="Optional, not needed for Ollama"
                onChange={(e) => updateStt({ apiKey: e.target.value })}
                autoCorrect="off"
                autoCapitalize="none"
                autoComplete="off"
                spellCheck={false}
              />
            </div>
          </div>
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
        </>
      ) : (
        <div className="form-group">
          <label className="form-label">
            Endpointing
            <InfoTooltip label="About endpointing">
              <strong>Endpointing</strong>
              How long (in seconds) Gladia waits for silence before treating an
              utterance as finished. Lower values feel snappier but may cut
              sentences short; higher values wait longer so pauses don&apos;t
              split your speech.
            </InfoTooltip>
          </label>
          <div className="endpointing-control">
            <input
              type="range"
              className="endpointing-slider"
              min={0.05}
              max={1}
              step={0.05}
              value={endpointingValue}
              onChange={(e) =>
                setSettings({
                  ...settings,
                  endpointing: parseFloat(e.target.value),
                })
              }
            />
            <span className="endpointing-value">
              {endpointingValue.toFixed(2)}s
            </span>
          </div>
        </div>
      )}

      <div className="setup-nav setup-nav-center">
        {isBatch && (
          <button
            type="button"
            className="btn btn-ghost"
            onClick={handleTestConnection}
            disabled={
              connectionTest.state === "testing" ||
              !!baseUrlError ||
              !!modelError
            }
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
        <button className="btn btn-primary" onClick={onDone}>
          Done
        </button>
      </div>
    </SettingsPageLayout>
  );
}
