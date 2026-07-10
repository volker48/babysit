import type { WakeEvent } from "./wake";

type JsonObject = Record<string, unknown>;

export function normalizeGitHubWebhook(
  event: string | null,
  body: string,
  deliveryId: string | null,
  receivedAt: number,
): WakeEvent | null {
  if (!isSupportedGitHubEvent(event) || !deliveryId) {
    return null;
  }
  const payload = parseObject(body);
  const repository = payload && repositoryFrom(payload);
  if (!repository || !payload) return null;
  if (event === "status") return statusWake(payload, repository, deliveryId, receivedAt);
  if (event === "pull_request") return pullRequestWake(payload, repository, deliveryId, receivedAt);
  if (event === "pull_request_review") {
    return pullRequestEventWake(payload, repository, deliveryId, receivedAt, "review");
  }
  if (event === "pull_request_review_comment") {
    return pullRequestEventWake(payload, repository, deliveryId, receivedAt, "comment");
  }
  if (event === "pull_request_review_thread") {
    return pullRequestEventWake(payload, repository, deliveryId, receivedAt, "thread");
  }
  if (event === "issue_comment")
    return issueCommentWake(payload, repository, deliveryId, receivedAt);
  const check = objectAt(payload, event === "check_run" ? "check_run" : "check_suite");
  return {
    deliveryId,
    kind: "check",
    repository,
    changeNumber: check ? firstPullRequestNumber(check) : undefined,
    headRevision: check ? stringAt(check, "head_sha") : undefined,
    receivedAt,
  };
}

function statusWake(
  payload: JsonObject,
  repository: WakeEvent["repository"],
  deliveryId: string,
  receivedAt: number,
): WakeEvent | null {
  const headRevision = stringAt(payload, "sha");
  if (!headRevision) return null;
  return { deliveryId, kind: "status", repository, headRevision, receivedAt };
}

export function isSupportedGitHubEvent(event: string | null): event is string {
  return [
    "check_run",
    "check_suite",
    "status",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_review_thread",
    "issue_comment",
  ].includes(event ?? "");
}

function pullRequestWake(
  payload: JsonObject,
  repository: WakeEvent["repository"],
  deliveryId: string,
  receivedAt: number,
): WakeEvent {
  const pullRequest = objectAt(payload, "pull_request");
  return {
    deliveryId,
    kind: "change",
    repository,
    changeNumber:
      numberAt(payload, "number") ?? (pullRequest ? numberAt(pullRequest, "number") : undefined),
    headRevision: pullRequest ? headRevision(pullRequest) : undefined,
    receivedAt,
  };
}

function pullRequestEventWake(
  payload: JsonObject,
  repository: WakeEvent["repository"],
  deliveryId: string,
  receivedAt: number,
  kind: WakeEvent["kind"],
): WakeEvent {
  const pullRequest = objectAt(payload, "pull_request");
  return {
    deliveryId,
    kind,
    repository,
    changeNumber: pullRequest ? numberAt(pullRequest, "number") : undefined,
    headRevision: pullRequest ? headRevision(pullRequest) : undefined,
    receivedAt,
  };
}

function issueCommentWake(
  payload: JsonObject,
  repository: WakeEvent["repository"],
  deliveryId: string,
  receivedAt: number,
): WakeEvent {
  const issue = objectAt(payload, "issue");
  const changeNumber =
    issue && objectAt(issue, "pull_request") ? numberAt(issue, "number") : undefined;
  return { deliveryId, kind: "comment", repository, changeNumber, receivedAt };
}

function parseObject(body: string): JsonObject | null {
  try {
    const value: unknown = JSON.parse(body);
    return isObject(value) ? value : null;
  } catch {
    return null;
  }
}

function repositoryFrom(payload: JsonObject): WakeEvent["repository"] | null {
  const repository = objectAt(payload, "repository");
  const id = repository && repositoryId(repository.id);
  const fullName = repository && stringAt(repository, "full_name");
  return id && fullName ? { id, fullName } : null;
}

function headRevision(value: JsonObject): string | undefined {
  const head = objectAt(value, "head");
  return head ? stringAt(head, "sha") : undefined;
}

function firstPullRequestNumber(value: JsonObject): number | undefined {
  const pullRequests = value.pull_requests;
  if (!Array.isArray(pullRequests) || !isObject(pullRequests[0])) return undefined;
  return numberAt(pullRequests[0], "number");
}

function objectAt(value: JsonObject, key: string): JsonObject | null {
  const nested = value[key];
  return isObject(nested) ? nested : null;
}

function stringAt(value: JsonObject, key: string): string | undefined {
  const nested = value[key];
  return typeof nested === "string" ? nested : undefined;
}

function numberAt(value: JsonObject, key: string): number | undefined {
  const nested = value[key];
  return typeof nested === "number" && Number.isSafeInteger(nested) ? nested : undefined;
}

function repositoryId(value: unknown): string | undefined {
  if (typeof value === "number" && Number.isSafeInteger(value)) return String(value);
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
