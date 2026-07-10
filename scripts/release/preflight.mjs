import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const CANONICAL_REPOSITORY = "NeurealRoblox/Neuman";
const OFFICIAL_WORKFLOW_PATH = ".github/workflows/official-release.yml";
const EXPECTED_ACTIONS = new Map([
  ["actions/checkout", "9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"],
  ["actions/setup-node", "48b55a011bda9f5d6aeb4c2d9c7362e8dae4041e"],
  ["actions/attest", "a1948c3f048ba23858d222213b7c278aabede763"],
  ["actions/upload-artifact", "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"],
  ["actions/download-artifact", "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"],
]);

function fail(message) {
  throw new Error(`release preflight failed: ${message}`);
}

function read(path) {
  return readFileSync(resolve(path), "utf8");
}

function parseCanonicalTag(value) {
  const match = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/.exec(
    value ?? "",
  );
  if (!match) fail("the tag must be a canonical v-prefixed SemVer value without build metadata");
  for (const identifier of match[4]?.split(".") ?? []) {
    if (/^\d+$/.test(identifier) && identifier.length > 1 && identifier.startsWith("0")) {
      fail("numeric prerelease identifiers must not contain leading zeroes");
    }
  }
  return value.slice(1);
}

function assertPinnedActions(workflow, label) {
  const references = [...workflow.matchAll(/^\s*uses:\s*([^@\s]+)@([^\s#]+).*$/gm)];
  if (references.length === 0) fail(`${label} contains no action references`);
  for (const [, action, reference] of references) {
    if (!/^[0-9a-f]{40}$/.test(reference)) fail(`${label} action ${action} is not pinned to a full commit`);
    const expected = EXPECTED_ACTIONS.get(action);
    if (!expected) fail(`${label} uses unreviewed action ${action}`);
    if (reference !== expected) fail(`${label} action ${action} is not pinned to the reviewed commit`);
  }
}

function eventNames(workflow) {
  const lines = workflow.split(/\r?\n/);
  const start = lines.findIndex((line) => /^on:\s*$/.test(line));
  if (start < 0) fail("release workflow does not have a structured top-level on block");
  const events = [];
  for (const line of lines.slice(start + 1)) {
    if (line.trim() === "") continue;
    if (!/^[ \t]/.test(line)) break;
    const event = /^ {2}([a-z_]+):\s*$/.exec(line)?.[1];
    if (event) events.push(event);
  }
  return events;
}

const tag = process.argv[2];
const version = parseCanonicalTag(tag);

const cargo = read("Cargo.toml");
const cargoPackage = /^\[package\]\s*$([\s\S]*?)(?=^\[)/m.exec(cargo)?.[1];
const cargoVersion = /^version\s*=\s*"([^"]+)"\s*$/m.exec(cargoPackage ?? "")?.[1];
const cargoRepository = /^repository\s*=\s*"([^"]+)"\s*$/m.exec(cargoPackage ?? "")?.[1];
const packageJson = JSON.parse(read("package.json"));
const packageLock = JSON.parse(read("package-lock.json"));
const tauri = JSON.parse(read("tauri.conf.json"));
const lockPackages = read("Cargo.lock")
  .split(/^\[\[package\]\]\s*$/m)
  .filter((block) => /^name\s*=\s*"neuman-desktop"\s*$/m.test(block));
if (lockPackages.length !== 1) fail(`Cargo.lock contains ${lockPackages.length} NeuMan package records`);
const lockVersion = /^version\s*=\s*"([^"]+)"\s*$/m.exec(lockPackages[0])?.[1];

const versions = {
  tag: version,
  cargo: cargoVersion,
  cargoLock: lockVersion,
  package: packageJson.version,
  packageLock: packageLock.packages?.[""]?.version,
  packageLockTopLevel: packageLock.version,
  tauri: tauri.version,
};
for (const [source, value] of Object.entries(versions)) {
  if (value !== version) fail(`${source} version ${String(value)} does not equal ${version}`);
}

if (cargoRepository !== `https://github.com/${CANONICAL_REPOSITORY}`) {
  fail("Cargo package repository is not the canonical GitHub repository");
}
if (packageJson.name !== "neuman-desktop" || packageJson.private !== true) {
  fail("desktop package identity/private-publish guard changed");
}
if (packageJson.engines?.node !== "24.12.0") fail("the official Node.js runtime is not pinned to 24.12.0");
if (!tauri.bundle?.createUpdaterArtifacts) fail("Tauri updater artifacts are not enabled");
if (tauri.identifier !== "dev.neuman.manager") fail("the signed application identifier changed");
if (!/^name\s*=\s*"neuman-desktop"\s*$/m.test(cargoPackage ?? "")) {
  fail("Cargo package name must match the Tauri desktop binary");
}
if (!/^name\s*=\s*"neuman"\s*$/m.test(/^\[lib\]\s*$([\s\S]*?)(?=^\[)/m.exec(cargo)?.[1] ?? "")) {
  fail("the shared Rust library must retain the neuman crate name");
}
if (
  packageJson.scripts?.["dev:desktop"] !== "tauri dev --features desktop -- --bin neuman-desktop" ||
  packageJson.scripts?.["build:desktop"] !==
    "tauri build --features desktop -- --bin neuman-desktop" ||
  packageJson.scripts?.["build:desktop:debug"] !==
    "tauri build --debug --no-bundle --features desktop -- --bin neuman-desktop"
) {
  fail("desktop build scripts must use Tauri's asset pipeline and explicitly enable the desktop feature");
}

for (const feature of ["windows-native", "apple-native"]) {
  if (!cargo.includes(`"${feature}"`)) fail(`OS credential feature ${feature} is missing`);
}

const desktop = read("neuman_desktop.rs");
for (const required of [
  'option_env!("NEUMAN_ROBLOX_OAUTH_CLIENT_ID")',
  'code_challenge_method", "S256"',
  'http://localhost:43891/oauth/callback',
  "KEYRING_SERVICE",
  ".https_only(true)",
]) {
  if (!desktop.includes(required)) fail(`OAuth security marker ${required} is missing`);
}
if (/["']client_secret["']/i.test(desktop)) {
  fail("desktop source contains a client-secret request or token-exchange field");
}
const compiledSelection = desktop.indexOf("let client_id = COMPILED_OAUTH_CLIENT_ID");
const developmentFallback = desktop.indexOf(".or_else(|| request.client_id.as_deref()", compiledSelection);
if (compiledSelection < 0 || developmentFallback < compiledSelection) {
  fail("compiled public OAuth client ID no longer takes precedence over the development fallback");
}

const updater = tauri.plugins?.updater;
if (!updater || typeof updater.pubkey !== "string" || updater.pubkey.trim().length < 64) {
  fail("official updater public key is not embedded; official release remains blocked");
}
if (/placeholder|replace|your public key/i.test(updater.pubkey)) fail("updater public key is a placeholder");
if (updater.dangerousInsecureTransportProtocol === true) fail("updater permits insecure transport");
const expectedUpdaterEndpoint = `https://github.com/${CANONICAL_REPOSITORY}/releases/latest/download/latest.json`;
if (
  !Array.isArray(updater.endpoints) ||
  updater.endpoints.length !== 1 ||
  updater.endpoints[0] !== expectedUpdaterEndpoint
) {
  fail("updater endpoint must be the single canonical GitHub Releases latest.json URL");
}
if (!cargo.includes("tauri-plugin-updater") || !desktop.includes("tauri_plugin_updater")) {
  fail("the signed desktop does not register the Tauri updater plugin");
}

const workflow = read(OFFICIAL_WORKFLOW_PATH);
const tauriBuilds = [...workflow.matchAll(/npm run tauri -- build[^\r\n]*/g)].map((match) => match[0]);
if (
  tauriBuilds.length !== 2 ||
  tauriBuilds.some(
    (command) =>
      !command.includes("--features desktop") || !command.includes("-- --bin neuman-desktop"),
  )
) {
  fail("every official Tauri build must enable and select the desktop binary");
}
const events = eventNames(workflow);
if (events.length !== 1 || events[0] !== "workflow_dispatch") {
  fail(`signing workflow events must be exactly workflow_dispatch, observed: ${events.join(", ")}`);
}
if (/pull_request_target\s*:/.test(workflow)) fail("the signing workflow must never use pull_request_target");
if (/runs-on:\s*(?:\[?\s*)?self-hosted\b/i.test(workflow)) fail("official signing cannot use a self-hosted runner");
if (!/^permissions:\s*\r?\n {2}contents:\s*read\s*$/m.test(workflow)) {
  fail("release workflow default permissions are not contents: read");
}
for (const permission of ["id-token: write", "attestations: write", "artifact-metadata: write"]) {
  if (!workflow.includes(permission)) fail(`release workflow is missing ${permission}`);
}
for (const marker of [
  "RUSTUP_TOOLCHAIN: 1.93.0",
  `CANONICAL_REPOSITORY: ${CANONICAL_REPOSITORY}`,
  "group: official-release",
  '[[ "$GITHUB_SHA" == "$default_head" ]]',
  '[[ "$commit" == "$default_head" ]]',
  "github.run_attempt",
  "--signer-workflow",
  "--signer-digest",
  "--deny-self-hosted-runners",
  "gh release verify",
  "vars.ROBLOX_OAUTH_CLIENT_ID",
]) {
  if (!workflow.includes(marker)) fail(`release workflow security marker ${marker} is missing`);
}
if (/ROBLOX_OAUTH_CLIENT_SECRET|secrets\.ROBLOX_OAUTH_CLIENT_ID/i.test(workflow)) {
  fail("Roblox public-client identity is incorrectly modeled as a secret");
}
for (const forbidden of ["aws-actions/", "azure/", "google-github-actions/", "vercel/", "sentry-cli", "terraform ", "kubectl "]) {
  if (workflow.toLowerCase().includes(forbidden)) fail(`release workflow contains deployment/control-plane marker ${forbidden}`);
}
assertPinnedActions(workflow, "official release workflow");

const ciWorkflow = read(".github/workflows/ci.yml");
assertPinnedActions(ciWorkflow, "CI workflow");
if (!ciWorkflow.includes("persist-credentials: false")) fail("CI checkout persists GitHub credentials");
if (!ciWorkflow.includes("npm run check:docs")) fail("CI does not validate documentation links and layout");
if (!ciWorkflow.includes("node scripts/release/contract-test.mjs")) fail("CI does not exercise the release contract test");

const dependencyNames = Object.keys({ ...packageJson.dependencies, ...packageJson.devDependencies });
if (dependencyNames.some((name) => /sentry|datadog|newrelic|posthog|segment|amplitude/i.test(name))) {
  fail("an unapproved hosted telemetry dependency is present");
}
const officialContract = read("docs/guides/OFFICIAL_RELEASES.md");
for (const invariant of [
  "does not operate an account service",
  "No `client_secret` exists",
  "distribution, not a central project-data service",
  "Workflows are active only from `.github/workflows`.",
]) {
  if (!officialContract.includes(invariant)) fail(`official distribution invariant is missing: ${invariant}`);
}

if (process.env.GITHUB_ACTIONS === "true") {
  if (process.env.GITHUB_REPOSITORY !== CANONICAL_REPOSITORY) fail("workflow is not running in the canonical repository");
  if (!/^[0-9a-f]{40}$/.test(process.env.GITHUB_SHA ?? "")) fail("GitHub workflow source commit is unavailable");
}

console.log(
  JSON.stringify(
    {
      ok: true,
      tag,
      version,
      versions,
      canonicalRepository: CANONICAL_REPOSITORY,
      publicOAuthClient: true,
      runtimeDataService: "none",
      updaterEndpoint: expectedUpdaterEndpoint,
      workflowSource: OFFICIAL_WORKFLOW_PATH,
    },
    null,
    2,
  ),
);
