<div align="center">
  <h1>TTM</h1>

  <p><strong>Fast, accurate voice dictation in any desktop app.</strong></p>

  <p>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-8b5cf6.svg" alt="MIT license" /></a>
    <img src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white" alt="Tauri 2" />
  </p>

  <p>
    <a href="#quick-start">Quick start</a> ·
    <a href="#development">Development</a> ·
    <a href="#architecture">Architecture</a> ·
    <a href="#privacy-and-data-handling">Privacy</a> ·
    <a href="#contributing">Contributing</a>
  </p>
</div>

TTM is an open-source desktop app that turns speech into text wherever you type. Hold a hotkey, speak, and the app sends your audio to the [Gladia Live Transcription API](https://docs.gladia.io/) or to a [local model through Ollama](#using-a-local-model-ollama), cleans the result, and pastes it into the focused application.

> [!NOTE]
>
> TTM currently supports macOS and Windows. A [Gladia API key](https://app.gladia.io/) is required unless you [use a local model](#using-a-local-model-ollama).

## Why TTM?

- **Dictate anywhere** — paste transcribed speech into editors, browsers, chat apps, and more.
- **See results live** — with Gladia, partial and final transcripts arrive over a low-latency WebSocket session.
- **Choose your workflow** — use push-to-talk or toggle mode with a configurable global hotkey.
- **Improve specialist terms** — add custom vocabulary, pronunciations, and language hints.
- **Speak naturally** — automatic endpointing and transcript cleanup produce readable text.
- **Keep control** — manage languages, code switching, history, and dictation statistics locally.
- **Stay out of the way** — use the compact overlay and reopen the app from the system tray.

## Quick start

This fork has no GitHub Releases yet, so build TTM from source. Install the [prerequisites](#prerequisites) first. On macOS:

```bash
npm ci
npm run tauri build
```

Copy `src-tauri/target/release/bundle/macos/TTM.app` to `/Applications`, then:

1. Launch TTM and either choose a local model ([Ollama](#using-a-local-model-ollama), see below) or enter a [Gladia API key](https://app.gladia.io/).
2. Grant microphone access. On macOS, also grant Accessibility access so TTM can paste into other apps.
3. Focus any text field and use the activation shortcut:
   - **macOS:** hold <kbd>Fn</kbd> / <kbd>Globe</kbd>
   - **Windows:** hold <kbd>Ctrl</kbd> + <kbd>Space</kbd>
4. Speak, then release the shortcut to finish the session.

The shortcut and activation mode can be changed in **App settings**.

## Using a local model (Ollama)

TTM can transcribe with a local model through Ollama's OpenAI-compatible API instead of Gladia. You don't need an API key, and audio never leaves your machine.

1. Install [Ollama](https://ollama.com) and pull the model:

   ```sh
   ollama pull gemma4:e4b
   ```

2. In TTM, open **Transcription settings** and set **Provider** to **OpenAI-compatible**. On first launch you can pick **Use a local model (Ollama) instead** rather than entering a Gladia key.
3. Keep the defaults (base URL `http://localhost:11434/v1`, model `gemma4:e4b`, no API key) and click **Test connection**.

There is no streaming with this provider. Nothing shows while you speak: TTM transcribes the whole recording after you release the shortcut, and sends recordings longer than 28 seconds as consecutive chunks. In testing, a 3-second clip came back in about 1 second and a 46-second clip in about 27 seconds.

Cloud services that expose an OpenAI-compatible `/audio/transcriptions` endpoint, such as Groq or OpenAI, work the same way: change the base URL, API key and model. In that case the audio goes to that service.

## Development

### Prerequisites

- [Node.js](https://nodejs.org/) 20.19 or newer
- [Rust](https://www.rust-lang.org/tools/install) stable
- The [Tauri 2 system dependencies](https://v2.tauri.app/start/prerequisites/) for your platform
- For end-to-end transcription testing, either a [Gladia API key](https://app.gladia.io/) or local [Ollama](https://ollama.com) with `gemma4:e4b` (no key needed)

Windows development also requires the Microsoft C++ Build Tools and WebView2. WebView2 is already included with current Windows 10 and Windows 11 installations.

### Run locally

```bash
git clone https://github.com/mvillarejo/ttm.git
cd ttm
npm ci
npm run tauri:dev
```

The last command starts Vite, opens the Tauri application, and enables frontend and Rust hot reload. On first launch, choose a local model (Ollama) or enter a Gladia API key; no `.env` file is needed.

> [!TIP]
>
> Run `npm run dev` when you only need the browser-based frontend. Native audio, global shortcuts, clipboard access, and Tauri commands require `npm run tauri:dev`.

### Useful commands

| Command               | Purpose                                          |
| --------------------- | ------------------------------------------------ |
| `npm run tauri:dev`   | Run the complete desktop app in development mode |
| `npm run dev`         | Run only the Vite frontend                       |
| `npm test`            | Run Rust tests, Vitest, and TypeScript checks    |
| `npm run typecheck`   | Type-check the frontend without emitting files   |
| `npm run format`      | Format the repository with Prettier              |
| `npm run build`       | Build the frontend bundle                        |
| `npm run tauri:build` | Create a production desktop bundle               |
| `npm run screenshots` | Capture the app screenshot gallery               |

Before opening a pull request, run:

```bash
npm run format
npm test
npm run build
```

## How it works

```text
Global hotkey → microphone capture ─┬→ Gladia WebSocket (live)
                                    └→ OpenAI-compatible /audio/transcriptions (batch, e.g. local Ollama)
                                        ↓
Focused app ← clipboard-safe paste ← transcript cleanup
```

1. The global hotkey starts microphone capture and opens a Gladia live session. The OpenAI-compatible provider instead buffers the audio and transcribes it after you release the hotkey.
2. With Gladia, PCM audio is sent in chunks while partial and final transcription events stream back.
3. Final utterances pass through `UtteranceCleaner`, which handles punctuation, capitalization, and duplicates.
4. Clean incremental text is pasted into the focused app without permanently replacing the clipboard.
5. Releasing the hotkey—or toggling it off—finalizes the session and returns the overlay to idle.

## Architecture

| Area | Technology | Location |
| --- | --- | --- |
| Desktop shell | Tauri 2 | `src-tauri/` |
| Native backend | Rust, Tokio, CPAL | `src-tauri/src/` |
| Interface | React 19, TypeScript, Vite | `src/` |
| Native bridge | Tauri commands and events | `src/App.tsx`, `src-tauri/src/main.rs` |
| CI and releases | GitHub Actions | `.github/workflows/` |

Important backend modules include:

- `gladia.rs` — live WebSocket transcription session
- `batch_stt.rs` — OpenAI-compatible batch transcription (local Ollama by default)
- `audio.rs` — microphone capture and PCM streaming
- `hotkey.rs` — global activation shortcut and accessibility checks
- `utterance_cleaner.rs` — incremental transcript post-processing
- `vocabulary.rs` — custom vocabulary expansion
- `config.rs` and `history.rs` — local settings and dictation history

The UI uses two windows: `main` for onboarding, history, and settings; and `overlay` for the floating dictation indicator.

## Platform support

| Capability | macOS | Windows |
| --- | --- | --- |
| Default shortcut | <kbd>Fn</kbd> / <kbd>Globe</kbd> | <kbd>Ctrl</kbd> + <kbd>Space</kbd> |
| Paste integration | CoreGraphics <kbd>Cmd</kbd> + <kbd>V</kbd> | enigo <kbd>Ctrl</kbd> + <kbd>V</kbd> |
| Clipboard | `pbcopy` / `pbpaste` | `arboard` |
| Required permissions | Microphone and Accessibility | Microphone |
| Release bundle | Universal DMG | NSIS installer |

Linux is not currently supported because the global hotkey, clipboard, and permission integrations are platform-specific.

## Building and releasing

Create a local production bundle with:

```bash
npm run tauri:build
```

Artifacts are written below `src-tauri/target/`. macOS universal release builds are placed in `universal-apple-darwin/release/bundle/dmg/`; Windows NSIS builds are placed in `release/bundle/nsis/`.

The release workflow runs for tags matching `v*`, stamps the tag version into the Node, Tauri, and Cargo manifests, builds macOS and Windows installers, and publishes them to GitHub Releases.

## Privacy and data handling

TTM is a local desktop client. It does not operate its own cloud backend for dictation. With the default Gladia provider, microphone audio is sent to Gladia’s Live Transcription API, which is the sole remote processor for that audio. With a [local model](#using-a-local-model-ollama), audio never leaves your machine.

| Data | What happens |
| --- | --- |
| Microphone audio | Gladia: during an active dictation session, PCM audio is streamed over WebSocket to the Gladia Live Transcription API. OpenAI-compatible provider: audio is held in memory until you stop, then sent as WAV to the configured base URL. With local Ollama (the default base URL), audio never leaves the machine. TTM does not save audio files locally. |
| Transcripts | Text returns from the selected provider. Cleaned transcript text and the session ID (Gladia's, or a locally generated one) are stored locally in SQLite (`history.db` in the app config directory). |
| Settings and API key | Stored locally in `config.json` in the user config directory (API keys, transcription provider and endpoint, hotkey, languages, custom vocabulary, and related preferences). |
| Clipboard | Used transiently to paste transcribed text into the focused app. An optional setting can leave the final transcript on the clipboard. |

Audio and transcripts processed by Gladia are subject to Gladia’s own policies:

- [Privacy notice](https://www.gladia.io/privacy-notice)
- [Terms & conditions](https://www.gladia.io/terms-conditions)

Those documents cover retention, subprocessors, and related platform practices. For how TTM stores data on your machine, see also [SECURITY.md](SECURITY.md).

## Contributing

Contributions are welcome—bug fixes, platform improvements, documentation, and focused feature proposals all help. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and pull-request expectations.

To report a security vulnerability privately, see [SECURITY.md](SECURITY.md).

## License

TTM is a personal fork of [GladiaFlow](https://github.com/gladiaio/gladiaflow) by Gladia, released under the [MIT License](LICENSE). The original copyright notice is kept in LICENSE.

TTM is not an official Gladia product and is not affiliated with or endorsed by Gladia. “Gladia” and “GladiaFlow” are trademarks of Gladia; see [TRADEMARKS.md](TRADEMARKS.md).
