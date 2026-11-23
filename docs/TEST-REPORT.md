# Verification guide

The automated checks are defined in `.github/workflows/ci.yml`. For an exact result, inspect the workflow run associated with the commit or release you are using.

```bash
pnpm install --frozen-lockfile
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml --locked --features devserver
cargo check --manifest-path src-tauri/Cargo.toml --locked --features devserver --bin devserver
pnpm package:mac --bundles app
```

The Rust tests cover project persistence and isolation, graph validation, undo/redo, request planning, API error handling and local mock-provider interactions. Model capability tests should exercise both chat and canvas paths and saved settings from earlier app versions.

## Manual release checks

- Start the packaged app on a Mac with a clean data directory and try local simulation.
- Confirm first launch has no projects, saved keys, signed-in team account or media.
- Create a project and confirm its canvas and gallery are empty. Exercise image and video nodes with your own test inputs, switching models and resolutions.
- Delete the last test project and restart; the empty welcome screen must return.
- Reopen a saved project and confirm media and graph state persist.
- Check light, dark and system appearances, app icon rendering and keyboard navigation.
- With a maintainer-owned key, verify any changed provider integration and report the model ID and region in release notes. Avoid publishing request content or credentials.
- If distributing a DMG, install that exact file on a test Mac and record its architecture and signing status.

A successful build or mock test does not verify live model entitlement, provider billing, output quality, another Mac architecture or a deployed team backend. Release notes should record those checks when performed.
