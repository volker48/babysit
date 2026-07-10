import { matchesWake, selectWakeRoute, type WakeEvent, type WakeRegistration } from "./wake";

export const WAKE_RETENTION_MS = 6 * 60 * 60 * 1000;

interface StoredWake {
  [key: string]: SqlStorageValue;
  cursor: number;
  received_at_ms: number;
  kind: string;
  delivery_id: string | null;
  repository_id: string | null;
  repository_full_name: string | null;
  pr_number: number | null;
  head_oid: string | null;
}

export interface ResumeResult {
  cursor: number;
  replay: number[];
  resync: boolean;
}

/** Persists compact wake metadata and the repository broker replay window. */
export class WakeHistory {
  constructor(private readonly storage: DurableObjectStorage) {
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS broker_state (id INTEGER PRIMARY KEY CHECK (id = 1), " +
        "current_cursor INTEGER NOT NULL)",
    );
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS wake_events (" +
        "cursor INTEGER PRIMARY KEY, received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, " +
        "head_oid TEXT, pr_number INTEGER, delivery_id TEXT, repository_id TEXT, " +
        "repository_full_name TEXT)",
    );
    this.ensureWakeColumns();
    this.storage.sql.exec(
      "CREATE INDEX IF NOT EXISTS wake_events_received_at ON wake_events (received_at_ms)",
    );
    this.storage.sql.exec("INSERT OR IGNORE INTO broker_state (id, current_cursor) VALUES (1, 0)");
  }

  async migrateLegacyCursor(): Promise<void> {
    const legacy = await this.storage.get<unknown>("cursor");
    if (legacy === undefined) return;
    if (typeof legacy === "number" && Number.isSafeInteger(legacy) && legacy >= 0) {
      this.storage.transactionSync(() => {
        if (legacy > this.currentCursor()) {
          this.storage.sql.exec("UPDATE broker_state SET current_cursor = ? WHERE id = 1", legacy);
        }
      });
    }
    await this.storage.delete("cursor");
  }

  append(wake: WakeEvent): number {
    return this.storage.transactionSync(() => {
      const cursor = this.currentCursor() + 1;
      this.storage.sql.exec("UPDATE broker_state SET current_cursor = ? WHERE id = 1", cursor);
      this.storage.sql.exec(
        "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid, pr_number, delivery_id, " +
          "repository_id, repository_full_name) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        cursor,
        wake.receivedAt,
        wake.kind,
        wake.headRevision ?? "",
        wake.changeNumber ?? null,
        wake.deliveryId,
        wake.repository.id,
        wake.repository.fullName,
      );
      this.prune(wake.receivedAt);
      return cursor;
    });
  }

  resume(after: number | null, registration: WakeRegistration, now: number): ResumeResult {
    return this.storage.transactionSync(() => {
      this.prune(now);
      const cursor = this.currentCursor();
      if (after === null || after === cursor) return { cursor, replay: [], resync: false };
      if (after > cursor || !this.isRetainedCursor(after, cursor)) {
        return { cursor, replay: [], resync: true };
      }
      const events = this.storage.sql
        .exec<StoredWake>(
          "SELECT cursor, received_at_ms, kind, delivery_id, repository_id, repository_full_name, " +
            "pr_number, head_oid FROM wake_events WHERE cursor > ? ORDER BY cursor ASC",
          after,
        )
        .toArray()
        .map(wakeFromRow);
      if (events.some((event) => event === null)) return { cursor, replay: [], resync: true };
      const replay = events.flatMap((event) => {
        if (!event) return [];
        const route = selectWakeRoute(event.wake, [registration]);
        return matchesWake(event.wake, registration, route) ? [event.cursor] : [];
      });
      return { cursor, replay, resync: false };
    });
  }

  private ensureWakeColumns(): void {
    const columns = this.storage.sql
      .exec<{ [key: string]: SqlStorageValue; name: string }>("PRAGMA table_info(wake_events)")
      .toArray()
      .map((column) => column.name);
    for (const column of ["repository_id", "repository_full_name"]) {
      if (!columns.includes(column))
        this.storage.sql.exec(`ALTER TABLE wake_events ADD COLUMN ${column} TEXT`);
    }
  }

  private currentCursor(): number {
    return this.storage.sql
      .exec<{ current_cursor: number }>("SELECT current_cursor FROM broker_state WHERE id = 1")
      .one().current_cursor;
  }

  private isRetainedCursor(after: number, cursor: number): boolean {
    if (cursor === 0) return after === 0;
    const oldest = this.storage.sql
      .exec<{ cursor: number }>("SELECT cursor FROM wake_events ORDER BY cursor ASC LIMIT 1")
      .toArray()[0];
    if (!oldest) return false;
    return after >= oldest.cursor || (after === 0 && oldest.cursor === 1);
  }

  private prune(now: number): void {
    this.storage.sql.exec(
      "DELETE FROM wake_events WHERE received_at_ms < ?",
      now - WAKE_RETENTION_MS,
    );
  }
}

function wakeFromRow(row: StoredWake): { cursor: number; wake: WakeEvent } | null {
  if (
    !Number.isSafeInteger(row.cursor) ||
    !Number.isSafeInteger(row.received_at_ms) ||
    typeof row.kind !== "string" ||
    typeof row.delivery_id !== "string" ||
    typeof row.repository_id !== "string" ||
    typeof row.repository_full_name !== "string" ||
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
      repository: { id: row.repository_id, fullName: row.repository_full_name },
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
