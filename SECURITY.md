# Security

## Report a vulnerability

Use GitHub's **Security → Report a vulnerability** option if it is enabled for this repository. If that option is unavailable, open an issue asking the maintainer for a private reporting channel and omit exploit details, credentials and personal data. Include affected versions and a minimal reproduction once a private channel is available.

Security fixes target the latest published release. During early development, review release notes before upgrading and back up your project data.

## Credentials and local data

The native Mac app stores provider keys and optional team sessions in macOS Keychain. Keys are entered through Settings. Personal projects and media are stored locally and can contain sensitive content. Shared screenshots, logs, sample projects and support attachments should be reviewed before posting.

Generation and assistant requests send selected content to your configured provider. Provider access, data handling and charges follow that provider's terms. A configured gateway also receives the requests it forwards.

The browser development bridge is a developer tool with access to its local project data. Run it only during development and stop it afterward. The packaged desktop app uses Tauri IPC.

For an optional Supabase team backend, use the public project URL and publishable key in the app. Keep the database password and privileged service credentials in appropriate secret storage. Review and test the supplied backend configuration for your deployment before inviting others.
