import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// A focused release guard, not a substitute for reviewing new files and media.
// Report filenames and rule names only: never print a matched credential.
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const files = [...new Set(execFileSync("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard"], { cwd: root, encoding: "utf8" }).split("\0").filter(Boolean))];
const rules = [
  ["private key", /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/],
  ["API credential", /\b(?:sk-[A-Za-z0-9_-]{24,}|sb_(?:secret|publishable)_[A-Za-z0-9_-]{18,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AKIA[A-Z0-9]{16})\b/],
  ["embedded JWT", /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b/],
  ["personal build path", /\/(?:Users|home)\/[A-Za-z0-9_.-]+\//],
  ["configured team project", /https:\/\/[a-z0-9]{20}\.supabase\.co\b/],
];
const forbiddenFile = /(?:^|\/)(?:\.devdata|\.release|node_modules|target|exports|storyboards)(?:\/|$)|(?:^|\/)(?:\.DS_Store|\.env(?:\..*)?|\.dev\.vars(?:\..*)?)$|\.(?:sqlite(?:-.*)?|db(?:-.*)?|pem|p12|pfx|key|log)$/i;
const failures = [];
let checked = 0;
for (const file of files) {
  const path = resolve(root, file);
  if (!existsSync(path)) continue;
  checked++;
  if (forbiddenFile.test(file) && !file.endsWith(".env.example")) failures.push(`${file}: local data or secret file`);
  const text = readFileSync(path).toString("utf8");
  for (const [label, pattern] of rules) if (pattern.test(text)) failures.push(`${file}: ${label}`);
}
const catalog = JSON.parse(readFileSync(resolve(root, "config/seed-catalog.json"), "utf8"));
if (Object.keys(catalog.gateways ?? {}).length) failures.push("config/seed-catalog.json: review configured gateways before publishing");
for (const url of Object.values(catalog.regions)) {
  if (!/^https:\/\/ark\.(ap-southeast|eu-west)\.bytepluses\.com\/api\/v3$/.test(url)) failures.push("config/seed-catalog.json: unreviewed provider address");
}
if (failures.length) {
  console.error(failures.join("\n"));
  process.exit(1);
}
console.log(`Release source check passed (${checked} files). Review media and final packages separately.`);
