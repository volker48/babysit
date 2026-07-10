import { DurableObject } from "cloudflare:workers";
import { isSupportedGitHubEvent, normalizeGitHubWebhook } from "./github";
import { WakeHistory } from "./replay";
import { matchesWake, selectWakeRoute, type WakeEvent, type WakeRegistration } from "./wake";

interface Env {
  REPOSITORY_GATEWAY: DurableObjectNamespace<RepositoryGateway>;
  WEBHOOK_SECRET: string;
  WATCHER_TOKEN: string;
}

interface Registration extends WakeRegistration {
  after: number | null;
  repository: string;
}

interface SocketAttachment extends WakeRegistration {
  cursor: number;
}

const decoder = new TextDecoder();
const encoder = new TextEncoder();

export async function fetch(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname === "/webhooks/github") return receiveWebhook(request, env);
  const repository = repositoryFromWatchPath(url.pathname);
  if (repository) return connectWatcher(request, env, repository);
  return new Response("not found", { status: 404 });
}

export default { fetch } satisfies ExportedHandler<Env>;

export class RepositoryGateway extends DurableObject<Env> {
  private readonly history: WakeHistory;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.history = new WakeHistory(ctx.storage);
    ctx.blockConcurrencyWhile(async () => {
      await this.history.migrateLegacyCursor();
    });
  }

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade") === "websocket") return this.acceptWatcher(request);
    if (request.method !== "POST") return new Response("not found", { status: 404 });
    const wake = await request.json<WakeEvent>();
    this.publish(wake);
    return new Response(null, { status: 202 });
  }

  webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): void {
    if (typeof message !== "string") return socket.close(1003, "text frames required");
    const registration = parseRegistration(message);
    if (!registration || !this.ctx.getTags(socket).includes(registration.repository)) {
      return socket.close(1008, "invalid registration");
    }
    const resume = this.history.resume(registration.after, registration, Date.now());
    socket.serializeAttachment({
      cursor: resume.cursor,
      changeNumber: registration.changeNumber,
      headRevision: registration.headRevision,
    } satisfies SocketAttachment);
    socket.send(frame("ready", resume.cursor));
    if (resume.resync) {
      socket.send(frame("resync", resume.cursor));
      return;
    }
    for (const cursor of resume.replay) socket.send(frame("replay", cursor));
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

  private publish(wake: WakeEvent): void {
    const cursor = this.history.append(wake);
    const watchers = activeWatchers(this.ctx.getWebSockets());
    const route = selectWakeRoute(
      wake,
      watchers.map(({ registration }) => registration),
    );
    for (const { socket, registration } of watchers) {
      if (matchesWake(wake, registration, route)) socket.send(frame("wake", cursor));
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
    const registration = attachmentRegistration(socket.deserializeAttachment());
    return registration ? [{ socket, registration }] : [];
  });
}

function attachmentRegistration(attachment: unknown): WakeRegistration | null {
  if (typeof attachment === "string") return { headRevision: attachment };
  if (!attachment || typeof attachment !== "object") return null;
  const value = attachment as Partial<SocketAttachment> & { headOid?: unknown };
  if (typeof value.headRevision === "string") {
    if (value.cursor !== undefined && !isValidCursor(value.cursor)) return null;
    return {
      changeNumber: typeof value.changeNumber === "number" ? value.changeNumber : undefined,
      headRevision: value.headRevision,
    };
  }
  if (!isValidCursor(value.cursor)) return null;
  return typeof value.headOid === "string" ? { headRevision: value.headOid } : null;
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

function isValidCursor(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
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

function frame(type: "ready" | "wake" | "replay" | "resync", cursor: number): string {
  return JSON.stringify({ type, version: 1, cursor });
}
