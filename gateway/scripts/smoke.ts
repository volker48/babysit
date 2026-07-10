import { spawn, spawnSync } from "node:child_process";
import { createHmac } from "node:crypto";
import { access, chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const quietWindowMs = 500;
const timeoutSecs = 20;
const args = parseArgs(process.argv.slice(2));
const repository = requiredArg(args, "--repository");
const pr = requiredArg(args, "--pr");
const gatewayUrl = requiredArg(args, "--gateway-url");
requiredEnv("WATCHER_TOKEN");
const webhookSecret = requiredEnv("WEBHOOK_SECRET");
const babysitBin = process.env.BABYSIT_BIN ?? defaultBabysitBin();
const realGh = findGh();
const headOid = fetchHeadOid(realGh, pr, repository);
const repositoryId = fetchRepositoryId(realGh, repository);
const webhookUrl = webhookUrlFor(gatewayUrl);
const tempDir = await mkdtemp(join(tmpdir(), "babysit-smoke-"));
const counter = join(tempDir, "gh-pr-view-count");
let child: ReturnType<typeof spawn> | undefined;
try {
  child = await startCli(babysitBin, pr, repository, gatewayUrl, realGh, counter, tempDir);
  await waitForExactFetches(counter, 2, child);
  await waitForQuietCount(counter, 2, child);
  await sendWebhook(webhookUrl, repository, repositoryId, headOid, webhookSecret);
  await waitForExactFetches(counter, 3, child);
  console.log("verified CLI initial, ready, and wake authoritative gh pr view fetches");
} finally {
  child?.kill("SIGTERM");
  await rm(tempDir, { force: true, recursive: true });
}

function parseArgs(values: string[]): Map<string, string> {
  const options = values[0] === "--" ? values.slice(1) : values;
  if (options.length % 2 !== 0) throw new Error("every smoke option requires a value");
  const args = new Map<string, string>();
  for (let index = 0; index < options.length; index += 2) {
    const [name, value] = [options[index], options[index + 1]];
    if (!name.startsWith("--") || !value) throw new Error("every smoke option requires a value");
    args.set(name, value);
  }
  return args;
}

function requiredArg(args: Map<string, string>, name: string): string {
  const value = args.get(name);
  if (!value)
    throw new Error(
      `usage: pnpm smoke -- --repository OWNER/REPO --pr NUMBER --gateway-url WSS_URL`,
    );
  return value;
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} must be set in the environment`);
  return value;
}

function defaultBabysitBin(): string {
  return resolve(dirname(fileURLToPath(import.meta.url)), "../../target/debug/babysit");
}

function findGh(): string {
  const result = spawnSync("which", ["gh"], { encoding: "utf8" });
  if (result.status !== 0) throw new Error("an authenticated gh executable is required");
  return result.stdout.trim();
}

function fetchHeadOid(gh: string, pr: string, repository: string): string {
  const result = spawnSync(gh, ["pr", "view", pr, "--repo", repository, "--json", "headRefOid"], {
    encoding: "utf8",
  });
  if (result.status !== 0) throw new Error("could not fetch the PR head with gh");
  const headOid = JSON.parse(result.stdout).headRefOid;
  if (typeof headOid !== "string") throw new Error("gh did not return a PR head OID");
  return headOid;
}

function fetchRepositoryId(gh: string, repository: string): number {
  const result = spawnSync(gh, ["repo", "view", repository, "--json", "databaseId"], {
    encoding: "utf8",
  });
  if (result.status !== 0) throw new Error("could not fetch the repository ID");
  const databaseId = JSON.parse(result.stdout).databaseId;
  if (typeof databaseId !== "number") throw new Error("could not fetch the repository ID");
  return databaseId;
}

function webhookUrlFor(gateway: string): string {
  const url = new URL(gateway);
  if (url.protocol !== "wss:" || url.pathname !== "/watch") {
    throw new Error("--gateway-url must be wss://host/watch");
  }
  url.protocol = "https:";
  url.pathname = "/webhooks/github";
  return url.toString();
}

async function startCli(
  binary: string,
  pr: string,
  repository: string,
  gateway: string,
  realGh: string,
  counter: string,
  tempDir: string,
) {
  await access(binary);
  const shim = join(tempDir, "gh");
  await writeFile(shim, ghShim());
  await chmod(shim, 0o700);
  const tokenStatus = spawnSync(binary, ["gateway-token", "status"], { encoding: "utf8" });
  if (tokenStatus.status !== 0 || !tokenStatus.stdout.includes("configured")) {
    throw new Error("enroll the matching watcher token with babysit gateway-token enroll first");
  }
  return spawn(
    binary,
    [
      "wait",
      pr,
      "--repo",
      repository,
      "--forge",
      "github",
      "--bots",
      "babysit-smoke-never-matches",
      "--events",
      "--gateway-url",
      gateway,
      "--timeout",
      String(timeoutSecs),
      "--interval",
      String(timeoutSecs),
    ],
    {
      env: {
        ...process.env,
        GH_VIEW_COUNTER: counter,
        PATH: `${tempDir}:${process.env.PATH}`,
        REAL_GH: realGh,
      },
      stdio: "inherit",
    },
  );
}

function ghShim(): string {
  return [
    "#!/bin/sh",
    'if [ "$1" = pr ] && [ "$2" = view ]; then printf . >> "$GH_VIEW_COUNTER"; fi',
    'exec "$REAL_GH" "$@"',
    "",
  ].join("\n");
}

async function sendWebhook(
  webhookUrl: string,
  repository: string,
  repositoryId: number,
  headOid: string,
  secret: string,
): Promise<void> {
  const body = JSON.stringify({
    repository: { id: repositoryId, full_name: repository },
    sha: headOid,
  });
  const signature = createHmac("sha256", secret).update(body).digest("hex");
  const response = await fetch(webhookUrl, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-github-delivery": "gateway-smoke-status",
      "x-github-event": "status",
      "x-hub-signature-256": `sha256=${signature}`,
    },
    body,
  });
  if (!response.ok) throw new Error(`webhook failed: ${response.status}`);
}

async function waitForExactFetches(
  counter: string,
  expected: number,
  child: ReturnType<typeof spawn>,
): Promise<void> {
  const deadline = Date.now() + timeoutSecs * 1000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null)
      throw new Error(`babysit exited before ${expected} gh pr view fetches`);
    const count = await fetchCount(counter);
    if (count === expected) return;
    if (count > expected) throw new Error(`expected ${expected} gh pr view fetches, got ${count}`);
    await sleep(100);
  }
  throw new Error(`timed out waiting for ${expected} gh pr view fetches`);
}

async function waitForQuietCount(
  counter: string,
  expected: number,
  child: ReturnType<typeof spawn>,
): Promise<void> {
  const deadline = Date.now() + quietWindowMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error("babysit exited during quiet window");
    const count = await fetchCount(counter);
    if (count !== expected)
      throw new Error(`expected ${expected} gh pr view fetches before wake, got ${count}`);
    await sleep(50);
  }
}

function sleep(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function fetchCount(counter: string): Promise<number> {
  try {
    return (await readFile(counter, "utf8")).length;
  } catch {
    return 0;
  }
}
