# Changelog

## Unreleased

- Use the current stable Apple silicon runner for GitHub checks and future Mac releases; the retired macOS 14 runner no longer starts jobs.

## 0.2.0 — 2026-11-23

Prepared for the first public-source release of Seed Studio.

### Added

- Seedance 2.5 Flash integration with its own video capabilities.
- MIT license, contributor guide, public roadmap and issue forms.
- GitHub Actions checks and a manual workflow for draft Mac releases.

### Changed

- Refreshed the app icon and public project presentation.
- Documented free application use, user-supplied API credentials and provider billing.
- New installations open an empty workspace; users name their own projects.
- Removed the bundled example canvas, reference image and private gateway configuration.

### Fixed

- Restricted the development bridge to expected local hosts and UI origins, with POST and JSON validation for RPC requests.
- Kept video task polling tied to the connection selected when the task was created.

### Verification

- Passed 42 Rust tests (41 integration tests and one development-bridge unit test).
- Release verification includes the empty first launch, user-created projects and returning to an empty workspace after deleting the last project.
- Live-provider generation and GitHub-hosted workflows require separate verification.

### Included from the desktop prototype

- Project chat, Seedream image workflows and a visual node canvas.
- Seedance video tasks, storyboards, reference elements and voice clips.
- Local project/media storage and macOS Keychain credentials.
- Optional Supabase accounts and session administration.

Live model access varies by account and region. Release checks use automated tests and local simulation unless the release notes explicitly identify a live-provider check.
