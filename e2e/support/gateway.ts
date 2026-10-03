import { createHmac, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { once } from "node:events";
import { join } from "node:path";
import { WebSocket } from "ws";
import { expect } from "e2e";
import { root } from "./cli.js";

// Deliberately synthetic credentials, used only by the isolated local worker.
export const webhookSecret = "synthetic-e2e-webhook-secret";
export const watcherToken = "synthetic-e2e-watcher-token";
export interface Frame {
  type: string;
  version: number;
  cursor: number;
}
export class Watcher {
  readonly frames: Frame[] = [];
  readonly closed: Promise<unknown[]>;
  constructor(
    readonly socket: WebSocket,
    readonly repository: string,
  ) {
    this.closed = once(socket, "close");
    socket.on("message", (data) => {
      this.frames.push(JSON.parse(data.toString()) as Frame);
    });
  }
  register(
    after: number | null = null,
    number = 7,
    headOid = "head",
    repository = this.repository,
  ) {
    this.socket.send(
      JSON.stringify({
        type: "register",
        version: 1,
        after,
        watch: { forge: "github", host: "github.com", repository, number, headOid },
      }),
    );
  }
  async next(index: number): Promise<Frame> {
    await expect.poll(() => this.frames.length, { timeout: 6_000 }).toBeGreaterThan(index);
    return this.frames[index];
  }
  close() {
    this.socket.terminate();
  }
}
export async function connect(baseUrl: string, repository: string): Promise<Watcher> {
  const socket = new WebSocket(`${baseUrl.replace("http:", "ws:")}/watch/${repository}`, {
    headers: { Authorization: `Bearer ${watcherToken}` },
    handshakeTimeout: 5_000,
  });
  const watcher = new Watcher(socket, repository);
  // Handle rejected upgrades on both event promises.
  await Promise.race([
    once(socket, "open"),
    watcher.closed.then(() => {
      throw new Error("watcher closed before open");
    }),
  ]);
  return watcher;
}
export async function deliver(
  baseUrl: string,
  repository: string,
  event = "status",
  delivery: string = randomUUID(),
  overrides: Record<string, unknown> = {},
) {
  const payload = JSON.parse(
    await readFile(
      join(
        root,
        "gateway/test/fixtures",
        `github-${event.replace("issue_comment", "issue-comment-pr").replaceAll("_", "-")}.json`,
      ),
      "utf8",
    ),
  ) as Record<string, unknown>;
  const body = JSON.stringify({
    ...payload,
    repository: { id: 100, full_name: repository },
    ...overrides,
  });
  return send(baseUrl, event, body, delivery);
}
export function send(
  baseUrl: string,
  event: string,
  body: string,
  delivery: string = randomUUID(),
  signature = createHmac("sha256", webhookSecret).update(body).digest("hex"),
  method: "POST" | "PUT" = "POST",
) {
  return fetch(`${baseUrl}/webhooks/github`, {
    method,
    headers: {
      "content-type": "application/json",
      "x-github-event": event,
      "x-github-delivery": delivery,
      "x-hub-signature-256": `sha256=${signature}`,
    },
    body,
  });
}
