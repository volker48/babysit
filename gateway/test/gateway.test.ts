import { env, exports } from "cloudflare:workers";
import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { createHmac } from "node:crypto";
import { describe, expect, it } from "vitest";
import { fetch as workerFetch } from "../src/worker";
import checkRunFixture from "./fixtures/github-check-run.json";
import checkRunMultiplePrsFixture from "./fixtures/github-check-run-multiple-prs.json";
import checkSuiteFixture from "./fixtures/github-check-suite.json";
import checkSuiteMultiplePrsFixture from "./fixtures/github-check-suite-multiple-prs.json";
import pullRequestFixture from "./fixtures/github-pull-request.json";
import pullRequestReviewFixture from "./fixtures/github-pull-request-review.json";
import pullRequestReviewCommentFixture from "./fixtures/github-pull-request-review-comment.json";
import pullRequestReviewThreadFixture from "./fixtures/github-pull-request-review-thread.json";
import issueCommentIssueFixture from "./fixtures/github-issue-comment-issue.json";
import issueCommentPrFixture from "./fixtures/github-issue-comment-pr.json";
import statusFixture from "./fixtures/github-status.json";

const webhookSecret = "webhook-test-secret";

function missingBindingEnv(): Parameters<typeof workerFetch>[1] {
  return {
    REPOSITORY_GATEWAY: undefined,
    WATCHER_TOKEN: "watcher-test-token",
    WEBHOOK_SECRET: "webhook-test-secret",
  } as unknown as Parameters<typeof workerFetch>[1];
}

function signedWebhook(event: string, payload: unknown, deliveryId = "delivery-1"): Request {
  const body = JSON.stringify(payload);
  const signature = createHmac("sha256", webhookSecret).update(body).digest("hex");
  return new Request("https://gateway.test/webhooks/github", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-github-delivery": deliveryId,
      "x-github-event": event,
      "x-hub-signature-256": `sha256=${signature}`,
    },
    body,
  });
}

function signedStatus(repository: string, sha: string, deliveryId = crypto.randomUUID()): Request {
  return signedWebhook(
    "status",
    {
      ...statusFixture,
      repository: { id: 1000, full_name: repository },
      sha,
    },
    deliveryId,
  );
}

async function watcher(repository: string, token = "watcher-test-token"): Promise<WebSocket> {
  const response = await exports.default.fetch(`https://gateway.test/watch/${repository}`, {
    headers: { Upgrade: "websocket", Authorization: `Bearer ${token}` },
  });
  expect(response.status).toBe(101);
  const socket = response.webSocket;
  if (!socket) throw new Error("expected websocket");
  socket.accept();
  return socket;
}

function nextMessage(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve) => {
    socket.addEventListener("message", ({ data }) => resolve(JSON.parse(String(data))), {
      once: true,
    });
  });
}

async function expectNoMessage(socket: WebSocket): Promise<void> {
  const result = await Promise.race([
    nextMessage(socket).then(() => "message"),
    new Promise((resolve) => setTimeout(resolve, 25, "quiet")),
  ]);
  expect(result).toBe("quiet");
}

function register(
  socket: WebSocket,
  repository: string,
  headOid: string,
  after: number | null,
  number = 7,
): void {
  socket.send(
    JSON.stringify({
      type: "register",
      version: 1,
      watch: { forge: "github", host: "github.com", repository, number, headOid },
      after,
    }),
  );
}

describe("GitHub status gateway", () => {
  it("rejects unsigned and invalidly signed payloads before JSON parsing", async () => {
    const missing = await exports.default.fetch("https://gateway.test/webhooks/github", {
      method: "POST",
      headers: { "content-type": "application/json", "x-github-event": "status" },
      body: "not json",
    });
    const invalid = await exports.default.fetch("https://gateway.test/webhooks/github", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-github-event": "status",
        "x-hub-signature-256": `sha256=${"0".repeat(64)}`,
      },
      body: "not json",
    });
    const malformed = await exports.default.fetch("https://gateway.test/webhooks/github", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-github-event": "status",
        "x-hub-signature-256": "sha256=not-hex",
      },
      body: "not json",
    });

    expect(missing.status).toBe(401);
    expect(invalid.status).toBe(401);
    expect(malformed.status).toBe(401);
  });

  it.each([
    ["check_run", {}],
    ["check_suite", {}],
    ["status", {}],
    ["pull_request", { number: 17 }],
    ["pull_request_review", { pull_request: {} }],
    ["pull_request_review_comment", { pull_request: {} }],
    ["pull_request_review_thread", { pull_request: {} }],
    ["issue_comment", { issue: {} }],
  ])("rejects a signed %s payload missing required event objects", async (event, eventPayload) => {
    const response = await exports.default.fetch(
      signedWebhook(event, {
        repository: { id: 1012, full_name: "invalid-envelope/repo" },
        ...eventPayload,
      }),
    );

    expect(response.status).toBe(400);
  });

  it("wakes all repository watchers for a valid check run without routing fields", async () => {
    const repository = "check-run-fallback/repo";
    const first = await watcher(repository);
    const second = await watcher(repository);
    const firstReady = nextMessage(first);
    const secondReady = nextMessage(second);
    register(first, repository, "first-head", null, 51);
    register(second, repository, "second-head", null, 52);
    await firstReady;
    await secondReady;

    const firstWake = nextMessage(first);
    const secondWake = nextMessage(second);
    expect(
      (
        await exports.default.fetch(
          signedWebhook("check_run", {
            repository: { id: 1013, full_name: repository },
            check_run: {},
          }),
        )
      ).status,
    ).toBe(202);
    expect(await firstWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    expect(await secondWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    first.close();
    second.close();
  });

  it("fails closed when watcher or webhook secret bindings are absent or empty", async () => {
    for (const token of [undefined, ""]) {
      const env = missingBindingEnv();
      env.WATCHER_TOKEN = token as never;
      const response = await workerFetch(new Request("https://gateway.test/watch/auth/repo"), env);
      expect(response.status).toBe(503);
    }
    for (const secret of [undefined, ""]) {
      const env = missingBindingEnv();
      env.WEBHOOK_SECRET = secret as never;
      const response = await workerFetch(
        new Request("https://gateway.test/webhooks/github", { method: "POST", body: "not json" }),
        env,
      );
      expect(response.status).toBe(503);
    }
  });

  it("rejects an unauthenticated watcher", async () => {
    const response = await exports.default.fetch("https://gateway.test/watch/auth/repo", {
      headers: { Upgrade: "websocket", Authorization: "Bearer wrong-token" },
    });

    expect(response.status).toBe(401);
  });

  it("acknowledges duplicate signed deliveries without another cursor or wake", async () => {
    const repository = "duplicate/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "duplicate-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const wake = nextMessage(socket);
    expect(
      (await exports.default.fetch(signedStatus(repository, "duplicate-head", "same-delivery")))
        .status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", cursor: 1 });
    expect(
      (await exports.default.fetch(signedStatus(repository, "duplicate-head", "same-delivery")))
        .status,
    ).toBe(202);
    await expectNoMessage(socket);

    const resumed = await watcher(repository);
    const resumedReady = nextMessage(resumed);
    register(resumed, repository, "duplicate-head", 1);
    expect(await resumedReady).toMatchObject({ type: "ready", cursor: 1 });
    socket.close();
    resumed.close();
  });

  it("sends ready before replaying a retained matching status", async () => {
    const repository = "replay/repo";
    expect((await exports.default.fetch(signedStatus(repository, "replay-head"))).status).toBe(202);
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "replay-head", 0);

    expect(await ready).toMatchObject({ type: "ready", version: 1, cursor: 1 });
    expect(await nextMessage(socket)).toMatchObject({ type: "replay", version: 1, cursor: 1 });
    socket.close();
  });

  it("resyncs when a registration cursor is ahead of the current cursor", async () => {
    const repository = "ahead/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "ahead-head", 1);

    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });
    expect(await nextMessage(socket)).toMatchObject({ type: "resync", cursor: 0 });
    socket.close();
  });

  it("persists cursor history across Durable Object eviction", async () => {
    const repository = "eviction/repo";
    expect((await exports.default.fetch(signedStatus(repository, "same-head"))).status).toBe(202);
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await evictDurableObject(stub);
    expect((await exports.default.fetch(signedStatus(repository, "same-head"))).status).toBe(202);

    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "same-head", 0);

    expect(await ready).toMatchObject({ type: "ready", cursor: 2 });
    expect(await nextMessage(socket)).toMatchObject({ type: "replay", cursor: 1 });
    expect(await nextMessage(socket)).toMatchObject({ type: "replay", cursor: 2 });
    socket.close();
  });

  it("replays a retained pull request wake after ready using repository fallback", async () => {
    const repository = "replay-pull-request/repo";
    expect(
      (
        await exports.default.fetch(
          signedWebhook("pull_request", {
            ...pullRequestFixture,
            repository: { id: 1014, full_name: repository },
          }),
        )
      ).status,
    ).toBe(202);
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, (_, state) => {
      state.storage.sql.exec(
        "UPDATE wake_events SET received_at_ms = ? WHERE cursor = 1",
        Date.now() - 6 * 60 * 60 * 1000 + 60_000,
      );
    });
    await evictDurableObject(stub);

    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "other-head", 0, 18);

    expect(await ready).toMatchObject({ type: "ready", cursor: 1 });
    expect(await nextMessage(socket)).toMatchObject({ type: "replay", cursor: 1 });
    socket.close();
  });

  it("resyncs legacy KV event records instead of decoding them as compact wakes", async () => {
    const repository = "legacy-event/repo";
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, async (_, state) => {
      await state.storage.put({
        cursor: 1,
        "event:00000000000000000001": { cursor: 1, headOid: "legacy-head" },
      });
    });
    await evictDurableObject(stub);

    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", 0);

    expect(await ready).toMatchObject({ type: "ready", cursor: 1 });
    expect(await nextMessage(socket)).toMatchObject({ type: "resync", cursor: 1 });
    socket.close();
  });

  it("keeps a hibernated watcher's head registration", async () => {
    const repository = "hibernation/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "matching-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await evictDurableObject(stub);
    const wake = nextMessage(socket);
    expect((await exports.default.fetch(signedStatus(repository, "matching-head"))).status).toBe(
      202,
    );

    expect(await wake).toMatchObject({ type: "wake", cursor: 1 });
    socket.close();
  });

  it("wakes a hibernated watcher with a legacy head-OID attachment", async () => {
    const repository = "legacy-attachment/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, (_, state) => {
      state.getWebSockets()[0].serializeAttachment("legacy-head");
    });
    await evictDurableObject(stub);

    const wake = nextMessage(socket);
    expect((await exports.default.fetch(signedStatus(repository, "legacy-head"))).status).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", cursor: 1 });
    socket.close();
  });

  it("wakes a hibernated watcher with a legacy structured attachment", async () => {
    const repository = "legacy-structured-attachment/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, (_, state) => {
      state.getWebSockets()[0].serializeAttachment({ cursor: 0, headOid: "legacy-head" });
    });
    await evictDurableObject(stub);

    const wake = nextMessage(socket);
    expect((await exports.default.fetch(signedStatus(repository, "legacy-head"))).status).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", cursor: 1 });
    socket.close();
  });

  it("wakes a hibernated watcher with a pre-replay structured attachment", async () => {
    const repository = "pre-replay-attachment/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, (_, state) => {
      state.getWebSockets()[0].serializeAttachment({
        after: null,
        repository,
        changeNumber: 7,
        headRevision: "legacy-head",
      });
    });
    await evictDurableObject(stub);

    const wake = nextMessage(socket);
    expect((await exports.default.fetch(signedStatus(repository, "legacy-head"))).status).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", cursor: 1 });
    socket.close();
  });

  it("migrates a legacy KV cursor before allocating or replaying wakes", async () => {
    const repository = "legacy-cursor/repo";
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, async (_, state) => {
      await state.storage.put("cursor", 41);
    });
    await evictDurableObject(stub);

    expect((await exports.default.fetch(signedStatus(repository, "legacy-head"))).status).toBe(202);
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", 41);

    expect(await ready).toMatchObject({ type: "ready", cursor: 42 });
    expect(await nextMessage(socket)).toMatchObject({ type: "resync", cursor: 42 });

    const wake = nextMessage(socket);
    expect((await exports.default.fetch(signedStatus(repository, "legacy-head"))).status).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", cursor: 43 });
    socket.close();
  });

  it("keeps a newer SQL cursor during legacy KV migration", async () => {
    const repository = "legacy-cursor-newer-sql/repo";
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, async (_, state) => {
      await state.storage.put("cursor", 41);
      state.storage.sql.exec("UPDATE broker_state SET current_cursor = 50 WHERE id = 1");
    });
    await evictDurableObject(stub);

    expect((await exports.default.fetch(signedStatus(repository, "legacy-head"))).status).toBe(202);
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", 50);

    expect(await ready).toMatchObject({ type: "ready", cursor: 51 });
    expect(await nextMessage(socket)).toMatchObject({ type: "resync", cursor: 51 });
    socket.close();
  });

  it("resyncs an expired or unknown cursor after ready", async () => {
    const repository = "resync/repo";
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    const now = Date.now();
    await runInDurableObject(stub, (_, state) => {
      state.storage.sql.exec("UPDATE broker_state SET current_cursor = 8 WHERE id = 1");
      state.storage.sql.exec(
        "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid) VALUES (?, ?, ?, ?)",
        1,
        now - 6 * 60 * 60 * 1000 - 1,
        "status",
        "expired-head",
      );
      state.storage.sql.exec(
        "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid) VALUES (?, ?, ?, ?)",
        8,
        now,
        "status",
        "current-head",
      );
    });

    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "current-head", 6);

    expect(await ready).toMatchObject({ type: "ready", cursor: 8 });
    expect(await nextMessage(socket)).toMatchObject({ type: "resync", cursor: 8 });
    socket.close();
  });

  it("wakes a pull request watcher for a signed check run fixture", async () => {
    const repository = "checks/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "older-head", null);
    await ready;

    const wake = nextMessage(socket);
    expect((await exports.default.fetch(signedWebhook("check_run", checkRunFixture))).status).toBe(
      202,
    );
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it("wakes a matching head watcher for a signed check suite fixture", async () => {
    const repository = "check-suites/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "check-suite-head", null);
    await ready;

    const wake = nextMessage(socket);
    expect(
      (await exports.default.fetch(signedWebhook("check_suite", checkSuiteFixture))).status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it.each([
    ["check_run", checkRunMultiplePrsFixture],
    ["check_suite", checkSuiteMultiplePrsFixture],
  ])(
    "wakes every matching head watcher for a signed multi-PR %s fixture",
    async (event, fixture) => {
      const repository = fixture.repository.full_name;
      const first = await watcher(repository);
      const second = await watcher(repository);
      const firstReady = nextMessage(first);
      const secondReady = nextMessage(second);
      register(first, repository, "multi-check-head", null, 7);
      register(second, repository, "multi-check-head", null, 8);
      await firstReady;
      await secondReady;

      const firstWake = nextMessage(first);
      const secondWake = nextMessage(second);
      expect((await exports.default.fetch(signedWebhook(event, fixture))).status).toBe(202);
      expect(await firstWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
      expect(await secondWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
      first.close();
      second.close();
    },
  );

  it("keeps single-PR check run routing ahead of a matching head", async () => {
    const repository = "single-check-run-precedence/repo";
    const change = await watcher(repository);
    const revision = await watcher(repository);
    const changeReady = nextMessage(change);
    const revisionReady = nextMessage(revision);
    register(change, repository, "older-head", null, 7);
    register(revision, repository, "check-run-head", null, 99);
    await changeReady;
    await revisionReady;

    const changeWake = nextMessage(change);
    expect(
      (
        await exports.default.fetch(
          signedWebhook("check_run", {
            ...checkRunFixture,
            repository: { id: 1017, full_name: repository },
          }),
        )
      ).status,
    ).toBe(202);
    expect(await changeWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    await expectNoMessage(revision);
    change.close();
    revision.close();
  });

  it("prefers the pull request number from a signed pull request fixture", async () => {
    const repository = "pull-requests/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "older-head", null, 17);
    await ready;

    const wake = nextMessage(socket);
    expect(
      (await exports.default.fetch(signedWebhook("pull_request", pullRequestFixture))).status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it("prefers a matching pull request registration over a matching head", async () => {
    const repository = "routing-precedence/repo";
    const change = await watcher(repository);
    const revision = await watcher(repository);
    const changeReady = nextMessage(change);
    const revisionReady = nextMessage(revision);
    register(change, repository, "older-head", null, 17);
    register(revision, repository, "pull-request-head", null, 99);
    await changeReady;
    await revisionReady;

    const changeWake = nextMessage(change);
    expect(
      (
        await exports.default.fetch(
          signedWebhook("pull_request", {
            ...pullRequestFixture,
            repository: { id: 1010, full_name: repository },
          }),
        )
      ).status,
    ).toBe(202);
    expect(await changeWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    await expectNoMessage(revision);
    change.close();
    revision.close();
  });

  it("falls back to a matching head when no pull request registration matches", async () => {
    const repository = "routing-revision/repo";
    const revision = await watcher(repository);
    const other = await watcher(repository);
    const revisionReady = nextMessage(revision);
    const otherReady = nextMessage(other);
    register(revision, repository, "pull-request-head", null, 18);
    register(other, repository, "other-head", null, 19);
    await revisionReady;
    await otherReady;

    const revisionWake = nextMessage(revision);
    expect(
      (
        await exports.default.fetch(
          signedWebhook("pull_request", {
            ...pullRequestFixture,
            repository: { id: 1011, full_name: repository },
          }),
        )
      ).status,
    ).toBe(202);
    expect(await revisionWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    await expectNoMessage(other);
    revision.close();
    other.close();
  });

  it("wakes a pull request watcher for a signed review fixture", async () => {
    const repository = "reviews/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "older-head", null, 23);
    await ready;

    const wake = nextMessage(socket);
    expect(
      (await exports.default.fetch(signedWebhook("pull_request_review", pullRequestReviewFixture)))
        .status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it("wakes a pull request watcher for a signed review comment fixture", async () => {
    const repository = "review-comments/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "older-head", null, 29);
    await ready;

    const wake = nextMessage(socket);
    expect(
      (
        await exports.default.fetch(
          signedWebhook("pull_request_review_comment", pullRequestReviewCommentFixture),
        )
      ).status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it("wakes a pull request watcher for a signed review thread fixture", async () => {
    const repository = "review-threads/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "older-head", null, 31);
    await ready;

    const wake = nextMessage(socket);
    expect(
      (
        await exports.default.fetch(
          signedWebhook("pull_request_review_thread", pullRequestReviewThreadFixture),
        )
      ).status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it("wakes the pull request watcher for a signed pull request issue comment fixture", async () => {
    const repository = "issue-comments-pr/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "older-head", null, 37);
    await ready;

    const wake = nextMessage(socket);
    expect(
      (await exports.default.fetch(signedWebhook("issue_comment", issueCommentPrFixture))).status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    socket.close();
  });

  it("wakes all repository watchers for a signed non-pull-request issue comment fixture", async () => {
    const repository = "issue-comments-issue/repo";
    const first = await watcher(repository);
    const second = await watcher(repository);
    const firstReady = nextMessage(first);
    const secondReady = nextMessage(second);
    register(first, repository, "first-head", null, 41);
    register(second, repository, "second-head", null, 42);
    await firstReady;
    await secondReady;

    const firstWake = nextMessage(first);
    const secondWake = nextMessage(second);
    expect(
      (await exports.default.fetch(signedWebhook("issue_comment", issueCommentIssueFixture)))
        .status,
    ).toBe(202);
    expect(await firstWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    expect(await secondWake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    first.close();
    second.close();
  });

  it("acknowledges unsupported signed events without waking a watcher", async () => {
    const repository = "unsupported/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "head", null);
    await ready;

    expect(
      (
        await exports.default.fetch(
          signedWebhook("push", { repository: { id: 1009, full_name: repository } }),
        )
      ).status,
    ).toBe(202);
    await expectNoMessage(socket);
    socket.close();
  });

  it("wakes only watchers whose registered head matches the status", async () => {
    const repository = "wake/repo";
    const matching = await watcher(repository);
    const nonmatching = await watcher(repository);
    const matchingReady = nextMessage(matching);
    const nonmatchingReady = nextMessage(nonmatching);
    register(matching, repository, "matching-head", null);
    register(nonmatching, repository, "other-head", null);
    await matchingReady;
    await nonmatchingReady;

    const wake = nextMessage(matching);
    expect((await exports.default.fetch(signedStatus(repository, "matching-head"))).status).toBe(
      202,
    );
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });

    const nonmatchingFrame = nextMessage(nonmatching);
    register(nonmatching, repository, "other-head", null);
    expect(await nonmatchingFrame).toMatchObject({ type: "ready", version: 1, cursor: 1 });
    matching.close();
    nonmatching.close();
  });
});
