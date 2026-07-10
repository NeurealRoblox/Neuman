import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";

const root = process.cwd();
const ignoredDirectories = new Set([".git", "node_modules", "target", "dist"]);
const markdownFiles = [];

function walk(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) continue;
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) walk(path);
    else if (entry.isFile() && entry.name.endsWith(".md")) markdownFiles.push(path);
  }
}

walk(root);

const failures = [];
const markdownLink = /\[[^\]]*\]\(([^)]+)\)/g;
for (const file of markdownFiles) {
  const source = readFileSync(file, "utf8");
  for (const match of source.matchAll(markdownLink)) {
    let target = match[1].trim().replace(/^<|>$/g, "").split(/\s+"/u, 1)[0];
    if (!target || target.startsWith("#") || /^[a-z][a-z0-9+.-]*:/iu.test(target)) continue;
    target = decodeURIComponent(target.split("#", 1)[0].split("?", 1)[0]);
    const resolved = target.startsWith("/") ? resolve(root, target.slice(1)) : resolve(dirname(file), target);
    if (relative(root, resolved).startsWith("..") || !existsSync(resolved)) {
      failures.push(`${relative(root, file)} -> ${match[1]}`);
    }
  }
}

const rootClutter = readdirSync(root).filter((name) => {
  if (!statSync(resolve(root, name)).isFile()) return false;
  return /^(?:SPEC_|ADR_)/u.test(name) || /_(?:README|IMPLEMENTATION)\.md$/u.test(name);
});
for (const name of rootClutter) failures.push(`root documentation belongs under docs/: ${name}`);

if (failures.length > 0) {
  console.error(`Documentation check failed:\n${failures.map((failure) => `- ${failure}`).join("\n")}`);
  process.exit(1);
}

console.log(`Documentation check passed (${markdownFiles.length} Markdown files).`);
