import { DurableObject } from "cloudflare:workers";
import { type StoredWake, type WakeInput, WakeHistory } from "./replay";

interface Env {
  REPOSITORY_GATEWAY: DurableObjectNamespace<RepositoryGateway>;
  WEBHOOK_SECRET: string;
  WATCHER_TOKEN: string;
}

interface Registration {
  after: number | null;
  headOid: string;
  repository: string;
}

const decoder = new TextDecoder();
const encoder = new TextEncoder();
interface SocketAttachment {
  cursor: number;
  headOid: string;
}

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
    const wake = parseWakeInput(await request.json<unknown>());
    if (!wake) return new Response("bad wake", { status: 400 });
    await this.publish(wake);
    return new Response(null, { status: 202 });
  }

  async alarm(): Promise<void> {
    const trailing = this.history.takeTrailingWake(Date.now());
    if (trailing.alarmAt !== null) {
      await this.ctx.storage.setAlarm(trailing.alarmAt);
      return;
    }
    if (trailing.wake) this.broadcast(trailing.wake, true);
  }

  webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): void {
    if (typeof message !== "string") return socket.close(1003, "text frames required");
    const registration = parseRegistration(message);
    if (!registration || !this.ctx.getTags(socket).includes(registration.repository)) {
      return socket.close(1008, "invalid registration");
    }
    const resume = this.history.resume(registration.after, registration.headOid, Date.now());
    const attachment: SocketAttachment = { cursor: resume.cursor, headOid: registration.headOid };
    socket.serializeAttachment(attachment);
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

  private async publish(input: WakeInput): Promise<void> {
    const accepted = this.history.accept(input, Date.now());
    if (accepted.alarmAt !== null) await this.ctx.storage.setAlarm(accepted.alarmAt);
    if (accepted.wake) this.broadcast(accepted.wake);
  }

  private broadcast(wake: StoredWake, repositoryWide = false): void {
    for (const socket of this.ctx.getWebSockets()) {
      if (
        repositoryWide ||
        wake.headOid === null ||
        attachmentHeadOid(socket.deserializeAttachment()) === wake.headOid
      ) {
        socket.send(frame("wake", wake.cursor));
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
  if (request.headers.get("X-GitHub-Event") !== "status") {
    return new Response(null, { status: 202 });
  }
  const deliveryId = request.headers.get("X-GitHub-Delivery");
  const status = normalizeStatus(decoder.decode(body), deliveryId);
  if (!status) return new Response("invalid status payload", { status: 400 });
  const id = env.REPOSITORY_GATEWAY.idFromName(status.repository);
  return env.REPOSITORY_GATEWAY.get(id).fetch("https://repository-gateway/status", {
    method: "POST",
    body: JSON.stringify(status.wake),
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

function attachmentHeadOid(attachment: unknown): string | null {
  if (typeof attachment === "string") return attachment;
  if (!attachment || typeof attachment !== "object") return null;
  const value = attachment as Partial<SocketAttachment>;
  const cursor = value.cursor;
  if (
    typeof value.headOid !== "string" ||
    typeof cursor !== "number" ||
    !Number.isSafeInteger(cursor) ||
    cursor < 0
  ) {
    return null;
  }
  return value.headOid;
}

function unavailable(): Response {
  return new Response("gateway unavailable", { status: 503 });
}

function isConfiguredSecret(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function normalizeStatus(
  body: string,
  deliveryId: string | null,
): { repository: string; wake: WakeInput } | null {
  try {
    const value = JSON.parse(body) as {
      repository?: { full_name?: unknown; id?: unknown };
      sha?: unknown;
    };
    if (
      typeof value.repository?.full_name !== "string" ||
      !isPositiveInteger(value.repository.id) ||
      typeof value.sha !== "string" ||
      value.sha.length === 0 ||
      !isNonEmptyString(deliveryId)
    ) {
      return null;
    }
    return {
      repository: value.repository.full_name,
      wake: {
        deliveryId,
        kind: "status",
        repositoryId: value.repository.id,
        prNumber: null,
        headOid: value.sha,
      },
    };
  } catch {
    return null;
  }
}

function parseWakeInput(value: unknown): WakeInput | null {
  if (!value || typeof value !== "object") return null;
  const wake = value as Partial<WakeInput>;
  if (
    !isNonEmptyString(wake.deliveryId) ||
    wake.kind !== "status" ||
    !isPositiveInteger(wake.repositoryId) ||
    !isNullablePositiveInteger(wake.prNumber) ||
    !isNullableString(wake.headOid)
  ) {
    return null;
  }
  return wake as WakeInput;
}

function isPositiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function isNullablePositiveInteger(value: unknown): value is number | null {
  return value === null || isPositiveInteger(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
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
    return { after, headOid: watch.headOid, repository: watch.repository };
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
): watch is Record<"headOid" | "repository", string> {
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
