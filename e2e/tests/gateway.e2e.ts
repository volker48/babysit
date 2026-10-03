import { randomUUID } from "node:crypto";
import { beforeAll, afterAll, afterEach, test, expect } from "e2e";
import { unstable_dev, type Unstable_DevWorker } from "wrangler";
import {
  connect,
  deliver,
  send,
  webhookSecret,
  watcherToken,
  type Watcher,
} from "../support/gateway.js";

let worker: Unstable_DevWorker;
let baseUrl: string;
const watchers: Watcher[] = [];
beforeAll(async () => {
  worker = await unstable_dev("../gateway/src/worker.ts", {
    config: "wrangler.json",
    local: true,
    ip: "127.0.0.1",
    port: 0,
    persist: false,
    logLevel: "error",
    envFiles: [],
    vars: { WEBHOOK_SECRET: webhookSecret, WATCHER_TOKEN: watcherToken },
    experimental: { disableExperimentalWarning: true, disableDevRegistry: true, watch: false },
  });
  baseUrl = `http://${worker.address}:${worker.port}`;
});
afterEach(() => {
  for (const watcher of watchers.splice(0)) watcher.close();
});
afterAll(async () => {
  await worker?.stop();
});
function repository() {
  return `e2e/${randomUUID()}`;
}
async function watch(repo: string, after: number | null = null, number = 7, head = "head") {
  const watcher = await connect(baseUrl, repo);
  watchers.push(watcher);
  watcher.register(after, number, head);
  expect(await watcher.next(0)).toMatchObject({ type: "ready", version: 1 });
  return watcher;
}
test("HTTP routing and watcher authentication", async () => {
  expect((await fetch(`${baseUrl}/missing`)).status).toBe(404);
  expect((await fetch(`${baseUrl}/watch/%ZZ/repo`)).status).toBe(404);
  expect((await fetch(`${baseUrl}/watch/owner/repo`)).status).toBe(401);
  expect(
    (
      await fetch(`${baseUrl}/watch/owner/repo`, {
        headers: { Authorization: "Bearer wrong", "X-Gateway-Authenticated": "1" },
      })
    ).status,
  ).toBe(401);
  expect(
    (
      await fetch(`${baseUrl}/watch/owner/repo`, {
        headers: { Authorization: `Bearer ${watcherToken}` },
      })
    ).status,
  ).toBe(426);
});
test("webhook rejects invalid signatures and malformed signed payloads", async () => {
  expect((await send(baseUrl, "status", "not json", undefined, "0".repeat(64))).status).toBe(401);
  expect((await send(baseUrl, "status", "not json")).status).toBe(400);
  expect((await send(baseUrl, "status", "{}", "")).status).toBe(400);
  expect((await send(baseUrl, "ping", "{}")).status).toBe(202);
});
for (const [event, number, head] of [
  ["check_run", 7, "check-run-head"],
  ["check_suite", 7, "check-suite-head"],
  ["status", 7, "HEAD_OID"],
  ["pull_request", 17, "pull-request-head"],
  ["pull_request_review", 23, "review-head"],
  ["pull_request_review_comment", 29, "review-comment-head"],
  ["pull_request_review_thread", 31, "review-thread-head"],
  ["issue_comment", 37, "head"],
] as const) {
  test(`signed ${event} webhook wakes only the matching watcher`, async () => {
    const repo = repository();
    const watcher = await watch(repo, null, number, head);
    const other = await watch(repo, null, 999, "other-head");
    const response = await deliver(baseUrl, repo, event);
    expect(response.status).toBe(202);
    expect(await watcher.next(1)).toEqual({ type: "wake", version: 1, cursor: 1 });
    // Registration is a round-trip barrier after publish, without an arbitrary sleep.
    other.register(null, 999, "other-head");
    expect(await other.next(1)).toEqual({ type: "ready", version: 1, cursor: 1 });
  });
}
test("duplicate deliveries do not advance the durable cursor", async () => {
  const repo = repository();
  const watcher = await watch(repo);
  const id = randomUUID();
  expect((await deliver(baseUrl, repo, "status", id, { sha: "head" })).status).toBe(202);
  expect((await watcher.next(1)).cursor).toBe(1);
  expect((await deliver(baseUrl, repo, "status", id, { sha: "head" })).status).toBe(202);
  watcher.register();
  expect(await watcher.next(2)).toEqual({ type: "ready", version: 1, cursor: 1 });
});
test("a burst produces a leading and a trailing wake", async () => {
  const repo = repository();
  const watcher = await watch(repo);
  expect((await deliver(baseUrl, repo, "status", undefined, { sha: "head" })).status).toBe(202);
  expect((await watcher.next(1)).cursor).toBe(1);
  for (let i = 0; i < 2; i++)
    expect((await deliver(baseUrl, repo, "status", undefined, { sha: "head" })).status).toBe(202);
  expect(await watcher.next(2)).toEqual({ type: "wake", version: 1, cursor: 3 });
  watcher.register();
  expect(await watcher.next(3)).toEqual({ type: "ready", version: 1, cursor: 3 });
});
test("reconnect sends ready before retained replay", async () => {
  const repo = repository();
  const original = await watch(repo);
  await deliver(baseUrl, repo, "status", undefined, { sha: "head" });
  await original.next(1);
  original.close();
  const resumed = await watch(repo, 0);
  expect(await resumed.next(1)).toEqual({ type: "replay", version: 1, cursor: 1 });
  expect(resumed.frames[0].cursor).toBe(1);
});
test("a cursor ahead of history requests resync", async () => {
  const watcher = await watch(repository(), 99);
  expect(await watcher.next(1)).toEqual({ type: "resync", version: 1, cursor: 0 });
});
test("PR routing excludes a different PR even when heads match", async () => {
  const repo = repository();
  const selected = await watch(repo, null, 7);
  const other = await watch(repo, null, 8);
  await deliver(baseUrl, repo, "pull_request", undefined, {
    number: 7,
    pull_request: { number: 7, head: { sha: "head" } },
  });
  expect((await selected.next(1)).type).toBe("wake");
  other.register(null, 8);
  expect((await other.next(1)).type).toBe("ready");
});
test("watchers in a different repository receive no wake", async () => {
  const firstRepo = repository();
  const first = await watch(firstRepo);
  const second = await watch(repository());
  await deliver(baseUrl, firstRepo, "status");
  await first.next(1);
  second.register();
  expect(await second.next(1)).toEqual({ type: "ready", version: 1, cursor: 0 });
});
for (const invalid of ["malformed JSON", "wrong repository", "invalid number", "binary"]) {
  test(`invalid registration closes the socket: ${invalid}`, async () => {
    const repo = repository();
    const watcher = await connect(baseUrl, repo);
    watchers.push(watcher);
    if (invalid === "malformed JSON") watcher.socket.send("{");
    else if (invalid === "binary") watcher.socket.send(Buffer.from("{}"));
    else
      watcher.register(
        null,
        invalid === "invalid number" ? 0 : 7,
        "head",
        invalid === "wrong repository" ? "wrong/repo" : repo,
      );
    const [code] = await watcher.closed;
    expect(code).toBe(invalid === "binary" ? 1003 : 1008);
  });
}
test("webhook ingress accepts only POST", async () => {
  const response = await send(baseUrl, "ping", "{}", undefined, undefined, "PUT");
  expect(response.status).toBe(405);
  expect(response.headers.get("allow")).toBe("POST");
});
test("a watcher can re-register after its PR head changes", async () => {
  const repo = repository();
  const watcher = await watch(repo);
  const oldHead = await watch(repo, null, 8, "head");
  watcher.register(null, 7, "new-head");
  expect((await watcher.next(1)).type).toBe("ready");
  expect((await deliver(baseUrl, repo, "status", undefined, { sha: "new-head" })).status).toBe(202);
  expect((await watcher.next(2)).type).toBe("wake");
  oldHead.register(null, 8, "head");
  expect((await oldHead.next(1)).type).toBe("ready");
});
test("events without a matching PR or head wake the repository", async () => {
  const repo = repository();
  const first = await watch(repo, null, 7, "first");
  const second = await watch(repo, null, 8, "second");
  expect(
    (await deliver(baseUrl, repo, "issue_comment", undefined, { issue: { number: 9 } })).status,
  ).toBe(202);
  expect((await first.next(1)).type).toBe("wake");
  expect((await second.next(1)).type).toBe("wake");
});
