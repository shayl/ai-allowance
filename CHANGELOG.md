# Changelog

All notable changes to AI Allowance are documented in this file.

The project follows Semantic Versioning. Dates use the ISO `YYYY-MM-DD` format.

## [Unreleased]

## [0.2.0] - 2026-09-19

### Changed

- Show every discovered local account or credential as a manual connection
  suggestion instead of connecting any provider automatically.
- Add contextual connection guides to discovered accounts, connected accounts,
  and each provider/account-type combination in the manual connection dialog.
- Rewrite connection guides as step-by-step account-portal instructions, add
  account disconnection, and hide connected accounts from local suggestions.
- Omit Claude Code and Claude Desktop because their personal sign-ins cannot
  provide allowance data; Anthropic Admin API connections remain available.
- Make Anthropic and OpenAI manual setup organization-only so their Admin API
  key instructions appear as soon as the provider is selected.
- Open account-setting links from connection guides in the system browser.
- Expand Anthropic organization reporting with documented token categories and
  web-search usage, convert cost cents to dollars, request the full monthly
  window, and distinguish a valid zero-activity report from missing data.
- Add a direct Claude Console billing link to connected Anthropic cards.
- Show all connected accounts in the combined-usage selector and clearly
  disable accounts that have no provider-reported allowance limit.

## [0.1.1] - 2026-09-17

### Fixed

- Distinguished Copilot AIC allowance consumption and its USD quota-value
  equivalent from actual billed or cross-ecosystem spend; Power BI spend is
  explicitly documented as not currently imported.
- Derived allowance usage consistently from remaining quota when available,
  clamped invalid values to each metric's limit, and preserved meaningful
  fractional percentages near 0% and 100%.
- Omitted incomplete Copilot allowance metrics instead of converting missing
  numeric fields to zero, while retaining finite GitHub-reported counters.
- Kept valid zero-used GitHub allowance snapshots and corrected billing
  fallback mapping to gross AIC consumption and net billed spend.

## [0.1.0] - 2026-09-17

### Added

- Windows system-tray application and detachable always-on-top hourglass
  widget.
- Authoritative GitHub Copilot AI-credit allowance, quota-value equivalent,
  remaining balance, and UTC reset reporting through the authenticated GitHub
  CLI account.
- Current-month local Copilot CLI AIC and token telemetry aggregated across
  local sessions.
- Anthropic and OpenAI organization API account support.
- Combined selected usage across compatible authoritative account limits.
- Persistent account selection and configurable provider/local refresh
  schedules.
- Separate Settings workspace with General, Accounts, and Appearance pages.
- Light and dark themes with a calm sage visual palette.
- SQLite history foundations and Windows Credential Manager secret storage.
- MSI, NSIS, and portable executable packaging.

[Unreleased]: https://github.com/shayl/ai-allowance/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/shayl/ai-allowance/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/shayl/ai-allowance/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/shayl/ai-allowance/releases/tag/v0.1.0
