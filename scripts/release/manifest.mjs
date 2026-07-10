import { createHash } from "node:crypto";
import { closeSync, fstatSync, lstatSync, openSync, readFileSync, readSync, readdirSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

const CANONICAL_REPOSITORY = "NeurealRoblox/Neuman";
const SIGNER_WORKFLOW = `${CANONICAL_REPOSITORY}/.github/workflows/official-release.yml`;
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,199}$/;
const TAG_PATTERN = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
const MAX_ASSET_BYTES = 2 * 1024 * 1024 * 1024;

function fail(message) {
  throw new Error(`release manifest failed: ${message}`);
}

function parseTag(tag) {
  const match = TAG_PATTERN.exec(tag ?? "");
  if (!match) fail("TAG must be canonical v-prefixed SemVer without build metadata");
  for (const identifier of match[4]?.split(".") ?? []) {
    if (/^\d+$/.test(identifier) && identifier.length > 1 && identifier.startsWith("0")) {
      fail("numeric prerelease identifiers must not contain leading zeroes");
    }
  }
  return tag.slice(1);
}

function validateCommit(value, label) {
  if (!/^[0-9a-f]{40}$/.test(value ?? "")) fail(`${label} must be a full lowercase commit SHA`);
}

function validateRepository(repository) {
  if (repository !== CANONICAL_REPOSITORY) fail(`repository must be ${CANONICAL_REPOSITORY}`);
}

function validateWorkflowUrl(value, repository) {
  let url;
  try {
    url = new URL(value);
  } catch {
    fail("workflow URL is invalid");
  }
  const expectedPath = new RegExp(`^/${repository.replace("/", "\\/")}/actions/runs/[1-9]\\d*/attempts/[1-9]\\d*$`);
  if (
    url.protocol !== "https:" ||
    url.hostname !== "github.com" ||
    url.port !== "" ||
    url.username !== "" ||
    url.password !== "" ||
    url.search !== "" ||
    url.hash !== "" ||
    !expectedPath.test(url.pathname)
  ) {
    fail("workflow URL is not an exact canonical GitHub Actions attempt URL");
  }
}

function directChild(root, path, label) {
  const resolved = resolve(path);
  if (dirname(resolved) !== root) fail(`${label} must be a direct child of ROOT`);
  return resolved;
}

function listFlatFiles(root) {
  const rootStat = lstatSync(root);
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) fail("ROOT must be a real directory");
  const entries = readdirSync(root, { withFileTypes: true });
  if (entries.length > 64) fail(`release ROOT contains too many entries: ${entries.length}`);
  const names = [];
  const caseFolded = new Set();
  for (const entry of entries) {
    if (!entry.isFile() || entry.isSymbolicLink()) fail(`release ROOT contains a non-regular entry: ${entry.name}`);
    if (!SAFE_NAME.test(entry.name)) fail(`unsafe release asset name: ${entry.name}`);
    const folded = entry.name.toLowerCase();
    if (caseFolded.has(folded)) fail(`case-insensitive duplicate release asset: ${entry.name}`);
    caseFolded.add(folded);
    names.push(entry.name);
  }
  return names.sort((left, right) => (left < right ? -1 : left > right ? 1 : 0));
}

function hashAndSize(path) {
  const descriptor = openSync(path, "r");
  try {
    const size = fstatSync(descriptor).size;
    if (!Number.isSafeInteger(size) || size < 0 || size > MAX_ASSET_BYTES) {
      fail(`release asset exceeds the 2 GiB policy: ${basename(path)}`);
    }
    const hash = createHash("sha256");
    const buffer = Buffer.allocUnsafe(1024 * 1024);
    let bytesRead;
    do {
      bytesRead = readSync(descriptor, buffer, 0, buffer.length, null);
      if (bytesRead > 0) hash.update(buffer.subarray(0, bytesRead));
    } while (bytesRead > 0);
    return { size, sha256: hash.digest("hex") };
  } finally {
    closeSync(descriptor);
  }
}

function readSmallText(path, label, maximum = 1024 * 1024) {
  const size = lstatSync(path).size;
  if (size < 1 || size > maximum) fail(`${label} has invalid byte length ${size}`);
  return readFileSync(path, "utf8");
}

function one(names, predicate, label) {
  const matches = names.filter(predicate);
  if (matches.length !== 1) fail(`expected exactly one ${label}, observed ${matches.length}`);
  return matches[0];
}

function validateInventory(root, names, version, evidenceName) {
  const allowed = (name) =>
    name === "SHA256SUMS" ||
    name === "SHA256SUMS.windows" ||
    name === "SHA256SUMS.macos" ||
    name === "latest.json" ||
    name === evidenceName ||
    name === `neuman-studio-plugin-${version}.luau` ||
    /\.(?:exe|msi|dmg|sig)$/.test(name) ||
    /\.app\.tar\.gz$/.test(name);
  for (const name of names) {
    if (!allowed(name)) fail(`unexpected release asset type: ${name}`);
  }
  for (const required of [
    "SHA256SUMS.windows",
    "SHA256SUMS.macos",
    `neuman-studio-plugin-${version}.luau`,
  ]) {
    if (!names.includes(required)) fail(`required release asset is missing: ${required}`);
  }

  const windowsInstallers = names.filter((name) => /\.(?:exe|msi)$/.test(name));
  if (windowsInstallers.length < 2) fail("both NSIS and MSI Windows installers are required");
  for (const installer of windowsInstallers) {
    if (!names.includes(`${installer}.sig`)) fail(`missing Tauri updater signature for ${installer}`);
  }
  const nsis = one(names, (name) => /_x64-setup\.exe$/.test(name), "Windows x86_64 NSIS updater");
  const macUpdater = one(names, (name) => /\.app\.tar\.gz$/.test(name), "macOS arm64 updater archive");
  if (!names.includes(`${macUpdater}.sig`)) fail(`missing Tauri updater signature for ${macUpdater}`);
  one(names, (name) => /\.dmg$/.test(name), "macOS arm64 DMG");

  for (const signature of names.filter((name) => name.endsWith(".sig"))) {
    if (!names.includes(signature.slice(0, -4))) fail(`orphan updater signature: ${signature}`);
    const value = readSmallText(join(root, signature), `updater signature ${signature}`, 4096).trim();
    if (value.length < 64 || value.length > 4096 || !/^[\x20-\x7e\r\n]+$/.test(value)) {
      fail(`malformed updater signature: ${signature}`);
    }
  }
  return { nsis, macUpdater };
}

function updaterManifest(root, tag, repository, inventory) {
  const entry = (asset) => ({
    signature: readSmallText(join(root, `${asset}.sig`), `updater signature for ${asset}`, 4096).trim(),
    url: `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(asset)}`,
  });
  return {
    version: tag.slice(1),
    notes: `See the signed GitHub release notes for ${tag}.`,
    platforms: {
      "darwin-aarch64": entry(inventory.macUpdater),
      "windows-x86_64": entry(inventory.nsis),
    },
  };
}

function signatureKind(name) {
  if (/\.(msi|exe)$/.test(name)) return "windows-authenticode-timestamped";
  if (/\.dmg$/.test(name)) return "apple-developer-id-notarized-stapled";
  if (/\.app\.tar\.gz$/.test(name)) return "contains-apple-signed-notarized-app";
  if (/\.sig$/.test(name)) return "tauri-updater-signature";
  return "github-artifact-attestation";
}

function artifacts(root, names, evidenceName) {
  return names
    .filter((name) => name !== evidenceName && name !== "SHA256SUMS")
    .map((name) => {
      const measured = hashAndSize(join(root, name));
      return {
        path: name,
        size: measured.size,
        sha256: measured.sha256,
        signatureKind: signatureKind(name),
      };
    });
}

function evidenceDocument({ tag, version, commit, repository, workflowUrl, workflowSourceCommit, artifacts: artifactList }) {
  return {
    schemaVersion: 2,
    product: "NeuMan",
    tag,
    version,
    sourceCommit: commit,
    repository,
    workflowUrl,
    workflowSourceCommit,
    signerWorkflow: SIGNER_WORKFLOW,
    distribution: "github-releases",
    runtimeDataService: "none",
    oauthClientType: "public-pkce-s256",
    supportedUpdaterTargets: ["darwin-aarch64", "windows-x86_64"],
    signingPolicy: {
      windows: "Authenticode plus RFC 3161 timestamp",
      macos: "Developer ID plus notarization and stapling",
      updater: "Tauri updater signature verified by embedded public key",
      provenance: "GitHub artifact attestation bound to official-release.yml",
    },
    artifacts: artifactList,
  };
}

function sameJson(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function generate(args) {
  const [rootArg, tag, commit, repository, workflowUrl, workflowSourceCommit, outputArg, updaterOutputArg] = args;
  if (!rootArg || !outputArg || !updaterOutputArg) {
    fail("usage: node scripts/release/manifest.mjs ROOT TAG COMMIT REPOSITORY WORKFLOW_URL WORKFLOW_SOURCE_COMMIT EVIDENCE_OUTPUT UPDATER_OUTPUT");
  }
  const version = parseTag(tag);
  validateCommit(commit, "COMMIT");
  validateCommit(workflowSourceCommit, "WORKFLOW_SOURCE_COMMIT");
  validateRepository(repository);
  validateWorkflowUrl(workflowUrl, repository);
  const root = resolve(rootArg);
  const output = directChild(root, outputArg, "EVIDENCE_OUTPUT");
  const updaterOutput = directChild(root, updaterOutputArg, "UPDATER_OUTPUT");
  if (basename(updaterOutput) !== "latest.json") fail("UPDATER_OUTPUT must be named latest.json");
  const initialNames = listFlatFiles(root);
  const inventory = validateInventory(root, initialNames, version, basename(output));
  const update = updaterManifest(root, tag, repository, inventory);
  writeFileSync(updaterOutput, `${JSON.stringify(update, null, 2)}\n`, { flag: "wx" });
  const names = listFlatFiles(root);
  const document = evidenceDocument({
    tag,
    version,
    commit,
    repository,
    workflowUrl,
    workflowSourceCommit,
    artifacts: artifacts(root, names, basename(output)),
  });
  writeFileSync(output, `${JSON.stringify(document, null, 2)}\n`, { flag: "wx" });
  console.log(JSON.stringify({ output, updaterOutput, artifacts: document.artifacts.length }));
}

function verify(args) {
  const [rootArg, evidenceArg, tag, commit, repository, workflowSourceCommit] = args;
  if (!rootArg || !evidenceArg) {
    fail("usage: node scripts/release/manifest.mjs --verify ROOT EVIDENCE TAG COMMIT REPOSITORY WORKFLOW_SOURCE_COMMIT");
  }
  const version = parseTag(tag);
  validateCommit(commit, "COMMIT");
  validateCommit(workflowSourceCommit, "WORKFLOW_SOURCE_COMMIT");
  validateRepository(repository);
  const root = resolve(rootArg);
  const evidencePath = directChild(root, evidenceArg, "EVIDENCE");
  const evidence = JSON.parse(readSmallText(evidencePath, "release evidence"));
  validateWorkflowUrl(evidence.workflowUrl, repository);
  const names = listFlatFiles(root);
  const inventory = validateInventory(root, names, version, basename(evidencePath));
  const expectedUpdate = updaterManifest(root, tag, repository, inventory);
  const observedUpdate = JSON.parse(readSmallText(join(root, "latest.json"), "updater manifest"));
  if (!sameJson(observedUpdate, expectedUpdate)) fail("latest.json does not match the signed release inventory");
  const expectedEvidence = evidenceDocument({
    tag,
    version,
    commit,
    repository,
    workflowUrl: evidence.workflowUrl,
    workflowSourceCommit,
    artifacts: artifacts(root, names, basename(evidencePath)),
  });
  if (!sameJson(evidence, expectedEvidence)) fail("release evidence does not match the downloaded release inventory");
  console.log(JSON.stringify({ ok: true, evidence: evidencePath, artifacts: evidence.artifacts.length }));
}

const args = process.argv.slice(2);
if (args[0] === "--verify") verify(args.slice(1));
else generate(args);
