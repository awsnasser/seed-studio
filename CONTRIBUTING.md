# Contributing to Seed Studio

Help make creative AI workflows more useful on Mac. Bug fixes, model integrations, documentation, accessibility improvements and reproducible examples are welcome.

## Start a contribution

Search existing issues, then describe the workflow you want to improve. For a larger feature, open a feature request so maintainers and contributors can agree on scope. Small fixes can arrive directly as a pull request.

1. Fork the repository and create a branch for your change.
2. Follow the setup instructions in the README.
3. Make a focused change and preserve existing project data.
4. Run the checks below and explain what you verified in the pull request.

```bash
pnpm install --frozen-lockfile
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml --locked --features devserver
```

Use local simulation or a local mock server for automated tests. For manual provider checks, use your own key in the application's Settings and account for the provider's charges. Keep credentials, private media, user databases and generated project exports out of commits and issue attachments.

## Adding a model

Verify the exact model ID, API host and capabilities against the provider's documentation. Update `config/seed-catalog.json` and both frontend controls and backend validation where necessary. Exercise chat and canvas entry points, model switching, saved older projects and failed requests. Include the source documentation and a regression test for capability differences.

A model appearing in the provider's catalogue does not establish account access. Describe separately what was checked against a mock server and what was tested against a live account.

## Releases

The target cadence is monthly improvements, with fixes between feature releases when needed. Keep unreleased changes in `CHANGELOG.md` and discuss upcoming work through issues and `ROADMAP.md`.

For a release, align versions in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `src-tauri/tauri.conf.json`. Run the checks, review the changelog, and run **Draft macOS release** from GitHub Actions on `main`. The workflow builds an Apple silicon DMG and creates a draft GitHub release with checksums. Review its notes, install the build on a test Mac, then publish the draft. The workflow uses ad-hoc signing. Developer ID signing and notarization require a separate maintainer setup.

A contribution is provided under this repository's MIT license. Include attribution and compatible licenses when adding third-party assets or code.
