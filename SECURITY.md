# Security policy

## Supported versions

Security fixes are applied to the latest released version of TTM on the `main` branch and published through [GitHub Releases](https://github.com/mvillarejo/ttm/releases). Older installers are not patched in place; upgrade to the newest release when a fix is available.

## Reporting a vulnerability

If you find a security issue in TTM — for example unsafe handling of the Gladia API key, clipboard contents, local config/history data, or the paste / Accessibility integration — please report it privately.

**Preferred:** use [GitHub private vulnerability reporting](https://github.com/mvillarejo/ttm/security/advisories/new) for this repository.

Please do not open a public GitHub issue or pull request that discloses an unfixed vulnerability.

We aim to acknowledge reports within a few business days and to keep you informed while we investigate and ship a fix.

## Scope notes

- TTM stores the API key and settings locally in the user config directory; treat that machine as trusted.
- TTM builds use ad-hoc signing and are not signed by Gladia.
- Transcription audio and results are processed by the Gladia Live Transcription API under Gladia’s [privacy notice](https://www.gladia.io/privacy-notice) and [terms & conditions](https://www.gladia.io/terms-conditions). See [Privacy and data handling](README.md#privacy-and-data-handling) in the README for what leaves the device vs. what stays local. Report Gladia platform issues through Gladia’s support channels, not this repository, unless they are specific to this client’s usage.
