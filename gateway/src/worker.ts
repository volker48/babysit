import { DurableObject } from "cloudflare:workers";
import { isSupportedGitHubEvent, normalizeGitHubWebhook } from "./github";
import { matchesWake, selectWakeRoute, type WakeEvent, type WakeRegistration } from "./wake";

interface Env {
  REPOSITORY_GATEWAY: DurableObjectNamespace<RepositoryGateway>;
  WEBHOOK_SECRET: string;
  WATCHER_TOKEN: string;
}

interface RecordedWake extends WakeEvent {
  cursor: number;
}

interface Registration extends WakeRegistration {
  after: number | null;
  repository: string;
}

const decoder = new TextDecoder();
const encoder = new TextEncoder();
const historyLimit = 100;

export async function fetch(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname === "/webhooks/github") return receiveWebhook(request, env);
  const repository = repositoryFromWatchPath(url.pathname);
  if (repository) return connectWatcher(request, env, repository);
  return new Response("not found", { status: 404 });
}

export default { fetch } satisfies ExportedHandler<Env>;

export class RepositoryGateway extends DurableObject<Env> {
  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade") === "websocket") return this.acceptWatcher(request);
    if (request.method !== "POST") return new Response("not found", { status: 404 });
    const event = await request.json<WakeEvent>();
    await this.publish(event);
    return new Response(null, { status: 202 });
  }

  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (typeof message !== "string") return socket.close(1003, "text frames required");
    const registration = parseRegistration(message);
    if (!registration || !this.ctx.getTags(socket).includes(registration.repository)) {
      return socket.close(1008, "invalid registration");
    }
    socket.serializeAttachment(registration);
    const current = (await this.ctx.storage.get<number>("cursor")) ?? 0;
    socket.send(frame("ready", current));
    await this.replay(socket, registration.after, current);
  }

  webSocketClose(socket: WebSocket): void {
    socket.close();
  }

  private acceptWatcher(request: Request): Response {
    if (request.headers.get("X-Gateway-Authenticated") !== "1") {
      return new Response("unauthorized", { status: 401 });
    }
    const repository = request.headers.get("X-Gateway-Repository");
    if (!repository) return new Response("bad watcher", { status: 400 });
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server, [repository]);
    return new Response(null, { status: 101, webSocket: client });
  }

  private async publish(wake: WakeEvent): Promise<void> {
    const cursor = ((await this.ctx.storage.get<number>("cursor")) ?? 0) + 1;
    const event = { ...wake, cursor } satisfies RecordedWake;
    await this.ctx.storage.put({ cursor, [eventKey(cursor)]: event });
    if (cursor > historyLimit) await this.ctx.storage.delete(eventKey(cursor - historyLimit));
    const watchers = activeWatchers(this.ctx.getWebSockets());
    const route = selectWakeRoute(
      event,
      watchers.map(({ registration }) => registration),
    );
    for (const { socket, registration } of watchers) {
      if (matchesWake(event, registration, route)) socket.send(frame("wake", cursor));
    }
  }

  private async replay(socket: WebSocket, after: number | null, current: number): Promise<void> {
    if (after === null || after === current) return;
    if (after > current || after < Math.max(0, current - historyLimit)) {
      socket.send(frame("resync", current));
      return;
    }
    const registration = attachedRegistration(socket);
    if (!registration) return;
    const watchers = activeWatchers(this.ctx.getWebSockets());
    const events = await this.ctx.storage.list<RecordedWake>({ prefix: "event:" });
    for (const event of events.values()) {
      const route = selectWakeRoute(
        event,
        watchers.map(({ registration: active }) => active),
      );
      if (event.cursor > after && matchesWake(event, registration, route)) {
        socket.send(frame("replay", event.cursor));
      }
    }
  }
}

async function receiveWebhook(request: Request, env: Env): Promise<Response> {
  if (!isConfiguredSecret(env.WEBHOOK_SECRET)) return unavailable();
  const body = await request.arrayBuffer();
  const signature = request.headers.get("X-Hub-Signature-256");
  if (!(await hasValidSignature(body, signature, env.WEBHOOK_SECRET))) {
    return new Response("invalid signature", { status: 401 });
  }
  const event = request.headers.get("X-GitHub-Event");
  if (!isSupportedGitHubEvent(event)) return new Response(null, { status: 202 });
  const wake = normalizeGitHubWebhook(
    event,
    decoder.decode(body),
    request.headers.get("X-GitHub-Delivery"),
    Date.now(),
  );
  if (!wake) return new Response("invalid webhook payload", { status: 400 });
  const id = env.REPOSITORY_GATEWAY.idFromName(wake.repository.fullName);
  return env.REPOSITORY_GATEWAY.get(id).fetch("https://repository-gateway/wake", {
    method: "POST",
    body: JSON.stringify(wake),
  });
}

function connectWatcher(
  request: Request,
  env: Env,
  repository: string,
): Promise<Response> | Response {
  if (!isConfiguredSecret(env.WATCHER_TOKEN)) return unavailable();
  if (request.headers.get("Authorization") !== `Bearer ${env.WATCHER_TOKEN}`) {
    return new Response("unauthorized", { status: 401 });
  }
  if (request.headers.get("Upgrade") !== "websocket") {
    return new Response("upgrade required", { status: 426 });
  }
  const headers = new Headers(request.headers);
  headers.set("X-Gateway-Authenticated", "1");
  headers.set("X-Gateway-Repository", repository);
  const forwarded = new Request(request, { headers });
  return env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository)).fetch(forwarded);
}

function unavailable(): Response {
  return new Response("gateway unavailable", { status: 503 });
}

function isConfiguredSecret(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function activeWatchers(
  sockets: WebSocket[],
): { socket: WebSocket; registration: WakeRegistration }[] {
  return sockets.flatMap((socket) => {
    const registration = attachedRegistration(socket);
    return registration ? [{ socket, registration }] : [];
  });
}

function attachedRegistration(socket: WebSocket): WakeRegistration | null {
  const attachment = socket.deserializeAttachment() as Partial<Registration> | null;
  if (typeof attachment?.changeNumber !== "number" || typeof attachment.headRevision !== "string") {
    return null;
  }
  return { changeNumber: attachment.changeNumber, headRevision: attachment.headRevision };
}

function parseRegistration(message: string): Registration | null {
  try {
    const value = JSON.parse(message) as {
      after?: unknown;
      type?: unknown;
      version?: unknown;
      watch?: Record<string, unknown>;
    };
    const after = value.after;
    const watch = value.watch;
    if (!isValidAfter(after) || !isValidRegistration(value.type, value.version, watch)) {
      return null;
    }
    return {
      after,
      changeNumber: watch.number,
      headRevision: watch.headOid,
      repository: watch.repository,
    };
  } catch {
    return null;
  }
}

function isValidAfter(value: unknown): value is number | null {
  return value === null || (typeof value === "number" && Number.isSafeInteger(value) && value >= 0);
}

function isValidRegistration(
  type: unknown,
  version: unknown,
  watch: Record<string, unknown> | undefined,
): watch is Record<"headOid" | "repository", string> & Record<"number", number> {
  if (type !== "register" || version !== 1 || !watch) return false;
  return (
    watch.forge === "github" &&
    watch.host === "github.com" &&
    typeof watch.repository === "string" &&
    typeof watch.number === "number" &&
    typeof watch.headOid === "string"
  );
}

function repositoryFromWatchPath(pathname: string): string | null {
  const match = /^\/watch\/([^/]+)\/([^/]+)$/.exec(pathname);
  return match ? `${decodeURIComponent(match[1])}/${decodeURIComponent(match[2])}` : null;
}

async function hasValidSignature(
  body: ArrayBuffer,
  signature: string | null,
  secret: string,
): Promise<boolean> {
  const signatureBytes = parseSignature(signature);
  if (!signatureBytes) return false;
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"],
  );
  return crypto.subtle.verify("HMAC", key, signatureBytes, body);
}

function parseSignature(signature: string | null): Uint8Array | null {
  const match = /^sha256=([0-9a-f]{64})$/i.exec(signature ?? "");
  if (!match) return null;
  const bytes = new Uint8Array(32);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(match[1].slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

function eventKey(cursor: number): string {
  return `event:${cursor.toString().padStart(20, "0")}`;
}

function frame(type: "ready" | "wake" | "replay" | "resync", cursor: number): string {
  return JSON.stringify({ type, version: 1, cursor });
}
