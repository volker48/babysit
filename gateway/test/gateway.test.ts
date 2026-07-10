import { env, exports } from "cloudflare:workers";
import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { createHmac } from "node:crypto";
import { describe, expect, it } from "vitest";
import { DEBOUNCE_WINDOW_MS } from "../src/replay";
import { fetch as workerFetch } from "../src/worker";
import statusFixture from "./fixtures/github-status.json";

const webhookSecret = "webhook-test-secret";

function missingBindingEnv(): Parameters<typeof workerFetch>[1] {
  return {
    REPOSITORY_GATEWAY: undefined,
    WATCHER_TOKEN: "watcher-test-token",
    WEBHOOK_SECRET: "webhook-test-secret",
  } as unknown as Parameters<typeof workerFetch>[1];
}

function signedStatus(repository: string, sha: string, deliveryId = crypto.randomUUID()): Request {
  const body = JSON.stringify({
    ...statusFixture,
    repository: { id: 1, full_name: repository },
    sha,
  });
  const signature = createHmac("sha256", webhookSecret).update(body).digest("hex");
  return new Request("https://gateway.test/webhooks/github", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-github-delivery": deliveryId,
      "x-github-event": "status",
      "x-hub-signature-256": `sha256=${signature}`,
    },
    body,
  });
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

function register(
  socket: WebSocket,
  repository: string,
  headOid: string,
  after: number | null,
): void {
  socket.send(
    JSON.stringify({
      type: "register",
      version: 1,
      watch: { forge: "github", host: "github.com", repository, number: 7, headOid },
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

  it("acknowledges a duplicate delivery without allocating another cursor or wake", async () => {
    const repository = "duplicate/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "duplicate-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const wake = nextMessage(socket);
    expect(
      (await exports.default.fetch(signedStatus(repository, "duplicate-head", "same-id"))).status,
    ).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", cursor: 1 });
    expect(
      (await exports.default.fetch(signedStatus(repository, "duplicate-head", "same-id"))).status,
    ).toBe(202);

    const resumed = await watcher(repository);
    const resumedReady = nextMessage(resumed);
    register(resumed, repository, "duplicate-head", 1);
    expect(await resumedReady).toMatchObject({ type: "ready", cursor: 1 });
    socket.close();
    resumed.close();
  });

  it("emits the leading and final wake of a related burst within two seconds", async () => {
    const repository = "debounce/repo";
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "debounce-head", null);
    expect(await ready).toMatchObject({ type: "ready", cursor: 0 });

    const wakes: number[] = [];
    socket.addEventListener("message", ({ data }) => {
      const frame = JSON.parse(String(data)) as { type?: string; cursor?: number };
      if (frame.type === "wake" && typeof frame.cursor === "number") wakes.push(frame.cursor);
    });
    for (const [headOid, deliveryId] of [
      ["debounce-head", "burst-first"],
      ["intermediate-head", "burst-middle"],
      ["final-head", "burst-final"],
    ]) {
      expect(
        (await exports.default.fetch(signedStatus(repository, headOid, deliveryId))).status,
      ).toBe(202);
    }

    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(wakes).toEqual([1]);
    await expect.poll(() => wakes, { timeout: DEBOUNCE_WINDOW_MS + 1_000 }).toEqual([1, 3]);
    socket.close();
  });

  it("stores only compact wake metadata and serializes concurrent duplicate delivery", async () => {
    const repository = "privacy/repo";
    const first = signedStatus(repository, "privacy-head", "concurrent-id");
    const second = signedStatus(repository, "privacy-head", "concurrent-id");
    const responses = await Promise.all([
      exports.default.fetch(first),
      exports.default.fetch(second),
    ]);
    expect(responses.map((response) => response.status)).toEqual([202, 202]);

    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    await runInDurableObject(stub, (_, state) => {
      const columns = state.storage.sql
        .exec<{ name: string }>("PRAGMA table_info(wake_events)")
        .toArray()
        .map((column) => column.name);
      const wake = state.storage.sql
        .exec<Record<string, unknown>>("SELECT * FROM wake_events")
        .one();
      expect(columns).toEqual([
        "cursor",
        "delivery_id",
        "kind",
        "repository_id",
        "pr_number",
        "head_oid",
        "received_at_ms",
      ]);
      expect(wake).toMatchObject({
        cursor: 1,
        delivery_id: "concurrent-id",
        kind: "status",
        repository_id: 1,
        pr_number: null,
        head_oid: "privacy-head",
      });
      expect(Object.keys(wake).sort()).toEqual(columns.slice().sort());
      expect(state.storage.sql.exec("SELECT delivery_id FROM delivery_dedupe").toArray()).toEqual([
        { delivery_id: "concurrent-id" },
      ]);
    });
  });

  it("prunes expired delivery IDs with expired wake history before accepting a retry", async () => {
    const repository = "pruning/repo";
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    const expiredAt = Date.now() - 6 * 60 * 60 * 1000 - 1;
    await runInDurableObject(stub, (_, state) => {
      state.storage.sql.exec("UPDATE broker_state SET current_cursor = 8 WHERE id = 1");
      state.storage.sql.exec(
        "INSERT INTO wake_events " +
          "(cursor, delivery_id, kind, repository_id, pr_number, head_oid, received_at_ms) " +
          "VALUES (?, ?, ?, ?, ?, ?, ?)",
        8,
        "expired-id",
        "status",
        1,
        null,
        "expired-head",
        expiredAt,
      );
      state.storage.sql.exec(
        "INSERT INTO delivery_dedupe (delivery_id, received_at_ms) VALUES (?, ?)",
        "expired-id",
        expiredAt,
      );
    });

    expect(
      (await exports.default.fetch(signedStatus(repository, "new-head", "expired-id"))).status,
    ).toBe(202);
    await runInDurableObject(stub, (_, state) => {
      expect(
        state.storage.sql.exec<{ cursor: number }>("SELECT cursor FROM wake_events").toArray(),
      ).toEqual([{ cursor: 9 }]);
      expect(
        state.storage.sql
          .exec<{ delivery_id: string }>("SELECT delivery_id FROM delivery_dedupe")
          .toArray(),
      ).toEqual([{ delivery_id: "expired-id" }]);
    });
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

  it("migrates #10 wake history into the compact schema without losing replay", async () => {
    const repository = "legacy-history/repo";
    const stub = env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
    const receivedAt = Date.now();
    await runInDurableObject(stub, (_, state) => {
      state.storage.sql.exec("DROP TABLE delivery_dedupe");
      state.storage.sql.exec("DROP TABLE wake_events");
      state.storage.sql.exec("DROP TABLE broker_state");
      state.storage.sql.exec(
        "CREATE TABLE broker_state (id INTEGER PRIMARY KEY CHECK (id = 1), " +
          "current_cursor INTEGER NOT NULL)",
      );
      state.storage.sql.exec("INSERT INTO broker_state (id, current_cursor) VALUES (1, 1)");
      state.storage.sql.exec(
        "CREATE TABLE wake_events (" +
          "cursor INTEGER PRIMARY KEY, received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, " +
          "head_oid TEXT NOT NULL, pr_number INTEGER, delivery_id TEXT)",
      );
      state.storage.sql.exec(
        "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid) VALUES (?, ?, ?, ?)",
        1,
        receivedAt,
        "status",
        "legacy-head",
      );
    });
    await evictDurableObject(stub);

    expect(
      (await exports.default.fetch(signedStatus(repository, "new-head", "new-id"))).status,
    ).toBe(202);
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "legacy-head", 0);
    expect(await ready).toMatchObject({ type: "ready", cursor: 2 });
    expect(await nextMessage(socket)).toMatchObject({ type: "replay", cursor: 1 });
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
