import { SELF } from "cloudflare:test";
import { createHmac } from "node:crypto";
import { describe, expect, it } from "vitest";
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

function signedStatus(repository: string, sha: string): Request {
  const body = JSON.stringify({
    ...statusFixture,
    repository: { full_name: repository },
    sha,
  });
  const signature = createHmac("sha256", webhookSecret).update(body).digest("hex");
  return new Request("https://gateway.test/webhooks/github", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-github-event": "status",
      "x-hub-signature-256": `sha256=${signature}`,
    },
    body,
  });
}

async function watcher(repository: string, token = "watcher-test-token"): Promise<WebSocket> {
  const response = await SELF.fetch(`https://gateway.test/watch/${repository}`, {
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

function expectNoMessage(socket: WebSocket): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(resolve, 25);
    socket.addEventListener(
      "message",
      () => {
        clearTimeout(timer);
        reject(new Error("unexpected watcher message"));
      },
      { once: true },
    );
  });
}

describe("GitHub status gateway", () => {
  it("rejects unsigned and invalidly signed payloads before JSON parsing", async () => {
    const missing = await SELF.fetch("https://gateway.test/webhooks/github", {
      method: "POST",
      headers: { "content-type": "application/json", "x-github-event": "status" },
      body: "not json",
    });
    const invalid = await SELF.fetch("https://gateway.test/webhooks/github", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-github-event": "status",
        "x-hub-signature-256": `sha256=${"0".repeat(64)}`,
      },
      body: "not json",
    });
    const malformed = await SELF.fetch("https://gateway.test/webhooks/github", {
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
    const response = await SELF.fetch("https://gateway.test/watch/auth/repo", {
      headers: { Upgrade: "websocket", Authorization: "Bearer wrong-token" },
    });

    expect(response.status).toBe(401);
  });

  it("sends ready before replaying a retained matching status", async () => {
    const repository = "replay/repo";
    expect((await SELF.fetch(signedStatus(repository, "replay-head"))).status).toBe(202);
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

  it("resyncs when the requested cursor is outside retained history", async () => {
    const repository = "resync/repo";
    for (let index = 0; index <= 100; index += 1) {
      expect((await SELF.fetch(signedStatus(repository, `head-${index}`))).status).toBe(202);
    }
    const socket = await watcher(repository);
    const ready = nextMessage(socket);
    register(socket, repository, "head-100", 0);

    expect(await ready).toMatchObject({ type: "ready", cursor: 101 });
    expect(await nextMessage(socket)).toMatchObject({ type: "resync", cursor: 101 });
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
    const noWake = expectNoMessage(nonmatching);
    expect((await SELF.fetch(signedStatus(repository, "matching-head"))).status).toBe(202);
    expect(await wake).toMatchObject({ type: "wake", version: 1, cursor: 1 });
    await noWake;
    matching.close();
    nonmatching.close();
  });
});
