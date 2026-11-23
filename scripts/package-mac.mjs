import { spawnSync } from "node:child_process";
import { homedir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Keep the builder's username and local checkout paths out of compiled diagnostics.
// Cargo's encoded form also supports paths containing spaces.
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const inherited = process.env.CARGO_ENCODED_RUSTFLAGS?.split("\x1f")
  ?? process.env.RUSTFLAGS?.trim().split(/\s+/).filter(Boolean)
  ?? [];
const flags = [
  ...inherited,
  `--remap-path-prefix=${homedir()}=~`,
  `--remap-path-prefix=${root}=seed-studio`,
];
const args = process.argv.slice(2);
const result = spawnSync("pnpm", ["tauri", "build", ...(args.length ? args : ["--bundles", "app,dmg"])], {
  cwd: root,
  stdio: "inherit",
  env: { ...process.env, CARGO_ENCODED_RUSTFLAGS: flags.join("\x1f") },
});
if (result.error) console.error(result.error.message);
process.exit(result.status ?? 1);
