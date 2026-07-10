import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";

const script = resolve("release_manifest.mjs");
const root = mkdtempSync(join(tmpdir(), "neuman-release-contract-"));
const tag = "v0.1.0";
const sourceCommit = "a".repeat(40);
const workflowCommit = "b".repeat(40);
const repository = "neuman-build/neuman";
const workflowUrl = `https://github.com/${repository}/actions/runs/123/attempts/1`;

function write(name, value) {
  writeFileSync(join(root, name), value);
}

function run(args, expectFailure = false) {
  try {
    execFileSync(process.execPath, [script, ...args], { encoding: "utf8", stdio: "pipe" });
    if (expectFailure) throw new Error(`command unexpectedly succeeded: ${args.join(" ")}`);
  } catch (error) {
    if (!expectFailure) throw error;
  }
}

function aggregateChecksum() {
  const names = [
    "NeuMan_0.1.0_aarch64.app.tar.gz",
    "NeuMan_0.1.0_aarch64.app.tar.gz.sig",
    "NeuMan_0.1.0_aarch64.dmg",
    "NeuMan_0.1.0_x64-setup.exe",
    "NeuMan_0.1.0_x64-setup.exe.sig",
    "NeuMan_0.1.0_x64.msi",
    "NeuMan_0.1.0_x64.msi.sig",
    "SHA256SUMS.macos",
    "SHA256SUMS.windows",
    "latest.json",
    "neuman-studio-plugin-0.1.0.luau",
    "release-evidence.json",
  ];
  return `${names
    .map((name) => {
      const digest = createHash("sha256").update(readFileSync(join(root, name))).digest("hex");
      return `${digest}  ${name}`;
    })
    .join("\n")}\n`;
}

try {
  const signature = `untrusted-fixture-${"A".repeat(96)}`;
  write("NeuMan_0.1.0_x64-setup.exe", "fixture-nsis");
  write("NeuMan_0.1.0_x64-setup.exe.sig", signature);
  write("NeuMan_0.1.0_x64.msi", "fixture-msi");
  write("NeuMan_0.1.0_x64.msi.sig", signature);
  write("NeuMan_0.1.0_aarch64.app.tar.gz", "fixture-app-tar");
  write("NeuMan_0.1.0_aarch64.app.tar.gz.sig", signature);
  write("NeuMan_0.1.0_aarch64.dmg", "fixture-dmg");
  write("SHA256SUMS.windows", "fixture platform checksum\n");
  write("SHA256SUMS.macos", "fixture platform checksum\n");
  write("neuman-studio-plugin-0.1.0.luau", "return true\n");

  run([
    root,
    tag,
    sourceCommit,
    repository,
    workflowUrl,
    workflowCommit,
    join(root, "release-evidence.json"),
    join(root, "latest.json"),
  ]);
  write("SHA256SUMS", aggregateChecksum());
  run([
    "--verify",
    root,
    join(root, "release-evidence.json"),
    tag,
    sourceCommit,
    repository,
    workflowCommit,
  ]);

  write("NeuMan_0.1.0_x64-setup.exe", "tampered-after-evidence");
  run(
    [
      "--verify",
      root,
      join(root, "release-evidence.json"),
      tag,
      sourceCommit,
      repository,
      workflowCommit,
    ],
    true,
  );
  run(
    [
      root,
      "v0.1.0-01",
      sourceCommit,
      repository,
      workflowUrl,
      workflowCommit,
      join(root, "second-evidence.json"),
      join(root, "second-latest.json"),
    ],
    true,
  );
  console.log(JSON.stringify({ ok: true, tests: 3 }));
} finally {
  rmSync(root, { recursive: true, force: true });
}
