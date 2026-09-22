# AI Allowance

A Windows-first system-tray and detachable desktop widget for viewing
authoritative AI usage, billed cost, quota, and reset data across GitHub
Copilot, Anthropic, and OpenAI.

Current version: **0.2.1**

## Principles

- Authoritative allowances come from official provider APIs only; no dashboard
  scraping or traffic interception. Local CLI telemetry is labeled separately
  and is never treated as an allowance without an authoritative limit.
- Credentials stay in Windows Credential Manager.
- Provider metrics remain separate when their units are incompatible.
- Missing limits are shown as unavailable rather than estimated.
- Provider quota value is never labeled as actual spend.

## Development

```powershell
npm install
npm run dev
npm test
npm run build
npm run tauri dev
```

The browser build includes representative demo data so the interface can be
reviewed without credentials. Native account connections persist configuration
in SQLite and secrets in Windows Credential Manager.

The native app never displays demo usage as real data. On every startup it
discovers reusable local authentication paths and suggests them in Settings.
Nothing found on the machine is connected automatically:

- `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` are suggested when inherited by the
  desktop process; their values are never copied into SQLite.
- Logged-in GitHub CLI accounts are suggested without copying the OAuth token.
- Installed Codex CLI accounts are reported even though their consumer login
  cannot currently be imported.

Claude Code and Claude Desktop are not shown because their personal sign-ins
cannot provide allowance data. Anthropic organization usage and cost remain
available through an Admin API key.

Choosing **Connect** explicitly registers a usable suggestion.
Every discovered and connected account includes an expandable **How to
connect** guide. The manual provider dialog also updates its instructions for
the selected provider and personal or organization account type. Connected
accounts can be disconnected from Settings; once connected, the same account
is hidden from **Found on this machine** until it is disconnected.
Anthropic and OpenAI manual connections are organization-only, so selecting
either provider immediately shows its Admin API key instructions instead of an
unsupported personal-account form. Account-setting links in the guides open in
the system browser.

GitHub's personal AI-credit report requires the local GitHub CLI token to have
the `user` OAuth scope. If the account card reports that this scope is missing,
run:

```powershell
gh auth refresh -h github.com -s user
```

For an authenticated Copilot account, the app first reads Copilot's own quota
snapshot through the GitHub account used by `gh api`. When GitHub returns an
allowance, the card shows the AIC entitlement, remaining AIC, derived AIC used,
used quota value in USD, GitHub's separately reported `credits_used` counter,
and reset date. Derived usage uses `entitlement - remaining` because those
fields define the current allowance balance. GitHub defines one AI credit
(AIC) as `$0.01` of quota value; that conversion is not actual billed spend.

## Quota value versus actual spend

Copilot quota and billing reports describe different amounts:

- Copilot entitlement, remaining, and AIC usage are allowance counters. Their
  USD equivalent is quota value, not a charge.
- GitHub billing usage reports use `grossQuantity` for total AIC consumed and
  `netAmount` for actual billed spend.
- Anthropic and OpenAI organization cost APIs report costs independently from
  personal-plan allowances.

Power BI **My AI Usage** is a separate cross-ecosystem report for linked
Microsoft 365 and GitHub accounts. It can report actual spend, tokens,
requests, and active days, but does not provide the Copilot entitlement,
remaining quota, reset date, or AIC allowance fields used here. AI Allowance
does not currently import Power BI data, so Copilot quota value must not be
compared to Power BI spend as though they were the same metric.

## Combined selected usage

The overall value is **Combined selected usage**. Only account snapshots with
an authoritative positive limit are eligible. Users can choose which eligible
accounts are included from **Settings**, and the selection persists locally.
All connected accounts appear in the selector; accounts without a
provider-reported limit are disabled with an explanation because they cannot
produce a meaningful usage percentage.
Provider-account creation and local-account discovery also live in Settings so
the dashboard remains focused on usage and status. **Use all
automatically** resets the preference so all currently and subsequently
eligible accounts are selected.

Settings is presented as a separate full-window workspace with a persistent
sidebar:

- **General** controls provider and local refresh schedules.
- **Accounts** controls summary inclusion, local discovery, and provider
  connections.
- **Appearance** selects the light or dark color mode.

Aggregation preserves provider units:

1. For each unit, the app sums used amounts and limits across selected
   accounts.
2. It calculates one used percentage for each unit group.
3. It averages the unit-group percentages equally to produce the overall used
   percentage.

When a metric supplies an explicit remaining value, used is derived as
`limit - remaining`; that takes precedence over the source-reported consumed
value. Each used amount is clamped to its own `[0, limit]` range before
aggregation. Unlike units are never added together.

The former **Most constrained allowance** summary selected the single lowest
remaining percentage. It was removed because one account's minimum did not
represent usage combined across the included accounts.

The `Widget` control opens a 240-by-290-pixel always-on-top window backed by the
same selection and account data. It shows only the hourglass percentage, the
**USED** caption, selected-account count, and nearest reset among the selected
eligible accounts.

The Copilot CLI card is local telemetry, not an authoritative allowance. The
former **Copilot CLI current session** label was misleading: the card now
aggregates `total_nano_aiu`, input tokens, output tokens, and cached-input tokens
across all local Copilot CLI sessions in the current UTC month from
`%USERPROFILE%\.copilot\session-store.db`. Nano-AIU is converted using
`1 AIC = 1,000,000,000 nano-AIU`. Because this telemetry supplies no
authoritative limit, it is distinct from and excluded from Combined selected
usage.

Refresh schedules are configurable in **Settings** and persist locally. Provider
allowance and cost refresh can run every 1, 5, 15, 30, or 60 minutes; local
Copilot telemetry can refresh every 5, 15, 30, or 60 seconds. The defaults are
15 minutes and 5 seconds respectively. Manual and tray-triggered refreshes
remain available.

Closing the primary window hides it to the system tray. Use the tray menu to
show the dashboard, toggle the always-on-top widget, refresh accounts, or quit.

The locally built development executable is written to
`src-tauri\target\debug\ai-allowance.exe`.

## Releases and versioning

AI Allowance follows [Semantic Versioning](https://semver.org/):

- **Patch** (`0.1.1`) for backward-compatible fixes.
- **Minor** (`0.2.0`) for backward-compatible features.
- **Major** (`1.0.0`) for breaking behavior or data-format changes.

Published versions are retained as private GitHub Releases. Each release
contains:

- `ai-allowance.exe` — portable Windows executable.
- `AI Allowance_<version>_x64_en-US.msi` — MSI installer.
- `AI Allowance_<version>_x64-setup.exe` — NSIS installer.

Pushing a tag such as `v0.2.0` runs the Windows release workflow and publishes
the three artifacts automatically. See
[`docs/RELEASING.md`](docs/RELEASING.md) for the complete release procedure and
[`CHANGELOG.md`](CHANGELOG.md) for version history.

## Supported account paths

| Provider | Official account path |
|---|---|
| GitHub | Personal or organization AI-credit billing usage |
| Anthropic | Organization Admin Usage and Cost APIs |
| OpenAI | Organization Usage and Costs APIs |

Personal Claude Pro/Max and ChatGPT/Codex allowance cards intentionally report
that the metric is unavailable because no documented third-party account API is
used.

Anthropic's Usage and Cost Admin API is unavailable to individual accounts.
Before connecting Anthropic, convert the Claude Console account to an
organization/team if necessary, then create an Admin API key under **Settings →
Admin keys**. The key must begin with `sk-ant-admin01-`; a regular
organization-scoped key beginning with `sk-ant-api` is not sufficient.

Anthropic organization connections report current-month API cost in USD,
uncached input tokens, cache-creation tokens, cache-read tokens, output tokens,
total tokens, and web-search requests. Anthropic reports cost amounts in cents,
which AI Allowance converts to dollars. Empty reports are shown as a successful
connection with no organization API activity rather than as missing data.
Each Anthropic card links directly to Claude Console billing so the current
prepaid balance can be checked or updated at its authoritative source.
