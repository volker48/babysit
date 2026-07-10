import { type WakeEvent, type WakeRegistration, matchesWake, selectWakeRoute } from "./wake";

export const WAKE_RETENTION_MS = 6 * 60 * 60 * 1000;
export const DEBOUNCE_WINDOW_MS = 2_000;
const RETRY_DELAY_MS = 1_000;

interface StoredWake {
  [key: string]: SqlStorageValue;
  cursor: number;
  received_at_ms: number;
  kind: string;
  delivery_id: string | null;
  repository_id: string | null;
  pr_number: number | null;
  head_oid: string | null;
}

interface BurstRow {
  [key: string]: SqlStorageValue;
  route_key: string;
  leading_cursor: number;
  pending_cursor: number;
  deadline_ms: number;
}

interface OutboxRow extends StoredWake {
  retry_at_ms: number;
}

export interface ResumeResult {
  cursor: number;
  replay: number[];
  resync: boolean;
}

export interface AcceptResult {
  cursor: number;
  duplicate: boolean;
}

export interface WakeIntent {
  cursor: number;
  wake: WakeEvent;
}

/** Persists replay, delivery deduplication, and durable wake delivery state. */
export class WakeHistory {
  constructor(private readonly storage: DurableObjectStorage) {
    this.createTables();
    this.migrateWakeSchema();
    this.dedupeExistingHistory();
    this.storage.sql.exec(
      "CREATE UNIQUE INDEX IF NOT EXISTS wake_events_delivery_id ON wake_events (delivery_id) " +
        "WHERE delivery_id IS NOT NULL",
    );
    this.storage.sql.exec(
      "CREATE INDEX IF NOT EXISTS wake_events_received_at ON wake_events (received_at_ms)",
    );
  }

  async migrateLegacyCursor(): Promise<void> {
    const legacy = await this.storage.get<unknown>("cursor");
    if (legacy === undefined) return;
    if (typeof legacy === "number" && Number.isSafeInteger(legacy) && legacy >= 0) {
      this.storage.transactionSync(() => {
        if (legacy > this.currentCursor()) this.setCursor(legacy);
      });
    }
    await this.storage.delete("cursor");
  }

  async accept(wake: WakeEvent, now: number): Promise<AcceptResult> {
    await this.prearm(now);
    const result = this.storage.transactionSync(() => this.acceptTransaction(wake, now));
    await this.schedule(now);
    return result;
  }

  async deliver(now: number, send: (intent: WakeIntent) => void): Promise<void> {
    await this.prearm(now);
    this.storage.transactionSync(() => {
      this.materializeOverdue(now);
      this.prune(now);
    });
    for (const intent of this.dueIntents(now)) {
      try {
        send(intent);
      } catch {
        this.defer(intent.cursor, now);
        break;
      }
      this.removeIntent(intent.cursor);
    }
    await this.schedule(now);
  }

  resume(after: number | null, registration: WakeRegistration, now: number): ResumeResult {
    return this.storage.transactionSync(() => {
      this.materializeOverdue(now);
      this.prune(now);
      const cursor = this.currentCursor();
      if (after === null || after === cursor) return { cursor, replay: [], resync: false };
      if (after > cursor || !this.isRetainedCursor(after, cursor)) {
        return { cursor, replay: [], resync: true };
      }
      const events = this.rowsAfter(after).map(wakeFromRow);
      if (events.some((event) => event === null)) return { cursor, replay: [], resync: true };
      const replay = events.flatMap((event) => this.replayCursor(event, registration));
      return { cursor, replay, resync: false };
    });
  }

  private acceptTransaction(wake: WakeEvent, now: number): AcceptResult {
    this.materializeOverdue(now);
    this.prune(now);
    const duplicate = this.findDelivery(wake.deliveryId);
    if (duplicate !== null) return { cursor: duplicate, duplicate: true };
    const cursor = this.append(wake);
    const route = routeKey(wake);
    const burst = this.burst(route);
    if (burst) this.updatePending(route, cursor);
    else this.startBurst(route, cursor, now, wake);
    return { cursor, duplicate: false };
  }

  private createTables(): void {
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS broker_state (id INTEGER PRIMARY KEY CHECK (id = 1), " +
        "current_cursor INTEGER NOT NULL)",
    );
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS wake_events (" +
        "cursor INTEGER PRIMARY KEY, received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, " +
        "head_oid TEXT, pr_number INTEGER, delivery_id TEXT, repository_id TEXT)",
    );
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS debounce_bursts (route_key TEXT PRIMARY KEY, " +
        "leading_cursor INTEGER NOT NULL, pending_cursor INTEGER NOT NULL, deadline_ms INTEGER NOT NULL)",
    );
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS wake_outbox (cursor INTEGER PRIMARY KEY, retry_at_ms INTEGER NOT NULL, " +
        "received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, delivery_id TEXT NOT NULL, " +
        "repository_id TEXT NOT NULL, pr_number INTEGER, head_oid TEXT)",
    );
    this.storage.sql.exec("INSERT OR IGNORE INTO broker_state (id, current_cursor) VALUES (1, 0)");
  }

  private migrateWakeSchema(): void {
    const columns = this.tableColumns("wake_events");
    if (columns.includes("repository_full_name")) {
      this.storage.transactionSync(() => this.dropLegacyRepositoryName());
      return;
    }
    if (!columns.includes("repository_id")) {
      this.storage.sql.exec("ALTER TABLE wake_events ADD COLUMN repository_id TEXT");
    }
  }

  private dropLegacyRepositoryName(): void {
    this.storage.sql.exec("ALTER TABLE wake_events RENAME TO wake_events_legacy");
    this.createTables();
    this.storage.sql.exec(
      "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid, pr_number, delivery_id, repository_id) " +
        "SELECT cursor, received_at_ms, kind, head_oid, pr_number, delivery_id, repository_id " +
        "FROM wake_events_legacy",
    );
    this.storage.sql.exec("DROP TABLE wake_events_legacy");
  }

  private dedupeExistingHistory(): void {
    this.storage.sql.exec(
      "DELETE FROM wake_events WHERE delivery_id IS NOT NULL AND cursor NOT IN (" +
        "SELECT MIN(cursor) FROM wake_events WHERE delivery_id IS NOT NULL GROUP BY delivery_id)",
    );
  }

  private append(wake: WakeEvent): number {
    const cursor = this.currentCursor() + 1;
    this.setCursor(cursor);
    this.storage.sql.exec(
      "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid, pr_number, delivery_id, " +
        "repository_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
      cursor,
      wake.receivedAt,
      wake.kind,
      wake.headRevision ?? "",
      wake.changeNumber ?? null,
      wake.deliveryId,
      wake.repository.id,
    );
    return cursor;
  }

  private startBurst(route: string, cursor: number, now: number, wake: WakeEvent): void {
    this.storage.sql.exec(
      "INSERT INTO debounce_bursts (route_key, leading_cursor, pending_cursor, deadline_ms) " +
        "VALUES (?, ?, ?, ?)",
      route,
      cursor,
      cursor,
      now + DEBOUNCE_WINDOW_MS,
    );
    this.insertIntent(cursor, wake, now);
  }

  private materializeOverdue(now: number): void {
    for (const burst of this.overdueBursts(now)) {
      if (burst.pending_cursor !== burst.leading_cursor) {
        const wake = this.wakeAt(burst.pending_cursor);
        if (wake) this.insertIntent(burst.pending_cursor, wake, now);
      }
      this.storage.sql.exec("DELETE FROM debounce_bursts WHERE route_key = ?", burst.route_key);
    }
  }

  private insertIntent(cursor: number, wake: WakeEvent, retryAt: number): void {
    this.storage.sql.exec(
      "INSERT OR IGNORE INTO wake_outbox (cursor, retry_at_ms, received_at_ms, kind, delivery_id, " +
        "repository_id, pr_number, head_oid) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
      cursor,
      retryAt,
      wake.receivedAt,
      wake.kind,
      wake.deliveryId,
      wake.repository.id,
      wake.changeNumber ?? null,
      wake.headRevision ?? null,
    );
  }

  private dueIntents(now: number): WakeIntent[] {
    return this.storage.sql
      .exec<OutboxRow>(
        "SELECT cursor, retry_at_ms, received_at_ms, kind, delivery_id, repository_id, " +
          "pr_number, head_oid FROM wake_outbox WHERE retry_at_ms <= ? " +
          "ORDER BY cursor ASC",
        now,
      )
      .toArray()
      .flatMap(intentFromRow);
  }

  private schedule(now: number): Promise<void> {
    const next = this.nextObligation(now);
    return next === null ? this.storage.deleteAlarm() : this.storage.setAlarm(next);
  }

  private nextObligation(now: number): number | null {
    const values = [this.nextRetry(), this.nextDeadline(), this.nextExpiry()].filter(
      (value): value is number => value !== null,
    );
    return values.length === 0 ? null : Math.max(now, Math.min(...values));
  }

  private nextRetry(): number | null {
    return this.minimum("SELECT MIN(retry_at_ms) AS value FROM wake_outbox");
  }

  private nextDeadline(): number | null {
    return this.minimum("SELECT MIN(deadline_ms) AS value FROM debounce_bursts");
  }

  private nextExpiry(): number | null {
    const oldest = this.minimum("SELECT MIN(received_at_ms) AS value FROM wake_events");
    return oldest === null ? null : oldest + WAKE_RETENTION_MS;
  }

  private minimum(query: string): number | null {
    const value = this.storage.sql.exec<{ value: number | null }>(query).one().value;
    return typeof value === "number" ? value : null;
  }

  private prearm(now: number): Promise<void> {
    return this.storage.setAlarm(now);
  }

  private prune(now: number): void {
    this.storage.sql.exec(
      "DELETE FROM wake_events WHERE received_at_ms <= ?",
      now - WAKE_RETENTION_MS,
    );
  }

  private findDelivery(deliveryId: string): number | null {
    const row = this.storage.sql
      .exec<{ cursor: number }>("SELECT cursor FROM wake_events WHERE delivery_id = ?", deliveryId)
      .toArray()[0];
    return row?.cursor ?? null;
  }

  private burst(route: string): BurstRow | null {
    return (
      this.storage.sql
        .exec<BurstRow>(
          "SELECT route_key, leading_cursor, pending_cursor, deadline_ms FROM debounce_bursts " +
            "WHERE route_key = ?",
          route,
        )
        .toArray()[0] ?? null
    );
  }

  private overdueBursts(now: number): BurstRow[] {
    return this.storage.sql
      .exec<BurstRow>(
        "SELECT route_key, leading_cursor, pending_cursor, deadline_ms FROM debounce_bursts " +
          "WHERE deadline_ms <= ? ORDER BY deadline_ms ASC, route_key ASC",
        now,
      )
      .toArray();
  }

  private updatePending(route: string, cursor: number): void {
    this.storage.sql.exec(
      "UPDATE debounce_bursts SET pending_cursor = ? WHERE route_key = ?",
      cursor,
      route,
    );
  }

  private wakeAt(cursor: number): WakeEvent | null {
    const row = this.storage.sql
      .exec<StoredWake>(
        "SELECT cursor, received_at_ms, kind, delivery_id, repository_id, pr_number, head_oid " +
          "FROM wake_events WHERE cursor = ?",
        cursor,
      )
      .toArray()[0];
    return row ? (wakeFromRow(row)?.wake ?? null) : null;
  }

  private removeIntent(cursor: number): void {
    this.storage.sql.exec("DELETE FROM wake_outbox WHERE cursor = ?", cursor);
  }

  private defer(cursor: number, now: number): void {
    this.storage.sql.exec(
      "UPDATE wake_outbox SET retry_at_ms = ? WHERE cursor = ?",
      now + RETRY_DELAY_MS,
      cursor,
    );
  }

  private rowsAfter(after: number): StoredWake[] {
    return this.storage.sql
      .exec<StoredWake>(
        "SELECT cursor, received_at_ms, kind, delivery_id, repository_id, pr_number, head_oid " +
          "FROM wake_events WHERE cursor > ? ORDER BY cursor ASC",
        after,
      )
      .toArray();
  }

  private replayCursor(
    event: { cursor: number; wake: WakeEvent } | null,
    registration: WakeRegistration,
  ): number[] {
    if (!event) return [];
    const route = selectWakeRoute(event.wake, [registration]);
    return matchesWake(event.wake, registration, route) ? [event.cursor] : [];
  }

  private currentCursor(): number {
    return this.storage.sql
      .exec<{ current_cursor: number }>("SELECT current_cursor FROM broker_state WHERE id = 1")
      .one().current_cursor;
  }

  private setCursor(cursor: number): void {
    this.storage.sql.exec("UPDATE broker_state SET current_cursor = ? WHERE id = 1", cursor);
  }

  private isRetainedCursor(after: number, cursor: number): boolean {
    if (cursor === 0) return after === 0;
    const oldest = this.storage.sql
      .exec<{ cursor: number }>("SELECT cursor FROM wake_events ORDER BY cursor ASC LIMIT 1")
      .toArray()[0];
    return Boolean(oldest && (after >= oldest.cursor || (after === 0 && oldest.cursor === 1)));
  }

  private tableColumns(table: string): string[] {
    return this.storage.sql
      .exec<{ [key: string]: SqlStorageValue; name: string }>(`PRAGMA table_info(${table})`)
      .toArray()
      .map((column) => column.name);
  }
}

function routeKey(wake: WakeEvent): string {
  if (wake.changeNumber !== undefined) return `pr:${wake.changeNumber}`;
  if (wake.headRevision !== undefined) return `head:${wake.headRevision}`;
  return "repository";
}

function intentFromRow(row: OutboxRow): WakeIntent[] {
  const wake = wakeFromRow(row)?.wake;
  return wake ? [{ cursor: row.cursor, wake }] : [];
}

function wakeFromRow(row: StoredWake): { cursor: number; wake: WakeEvent } | null {
  if (
    !Number.isSafeInteger(row.cursor) ||
    !Number.isSafeInteger(row.received_at_ms) ||
    typeof row.kind !== "string" ||
    typeof row.delivery_id !== "string" ||
    typeof row.repository_id !== "string" ||
    !optionalNumber(row.pr_number) ||
    !optionalString(row.head_oid)
  ) {
    return null;
  }
  return {
    cursor: row.cursor,
    wake: {
      deliveryId: row.delivery_id,
      kind: row.kind,
      repository: { id: row.repository_id, fullName: "" },
      changeNumber: row.pr_number ?? undefined,
      headRevision: row.head_oid || undefined,
      receivedAt: row.received_at_ms,
    },
  };
}

function optionalNumber(value: number | null): boolean {
  return value === null || Number.isSafeInteger(value);
}

function optionalString(value: string | null): boolean {
  return value === null || typeof value === "string";
}
