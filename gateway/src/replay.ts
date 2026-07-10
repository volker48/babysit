export const WAKE_RETENTION_MS = 6 * 60 * 60 * 1000;

interface StoredWake {
  [key: string]: SqlStorageValue;
  cursor: number;
  head_oid: string;
}

export interface ResumeResult {
  cursor: number;
  replay: number[];
  resync: boolean;
}

/** Persists the repository broker cursor and its compact wake replay window. */
export class WakeHistory {
  constructor(private readonly storage: DurableObjectStorage) {
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS broker_state (id INTEGER PRIMARY KEY CHECK (id = 1), " +
        "current_cursor INTEGER NOT NULL)",
    );
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS wake_events (" +
        "cursor INTEGER PRIMARY KEY, received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, " +
        "head_oid TEXT NOT NULL, pr_number INTEGER, delivery_id TEXT)",
    );
    this.storage.sql.exec(
      "CREATE INDEX IF NOT EXISTS wake_events_received_at ON wake_events (received_at_ms)",
    );
    this.storage.sql.exec("INSERT OR IGNORE INTO broker_state (id, current_cursor) VALUES (1, 0)");
  }

  append(headOid: string, now: number): number {
    return this.storage.transactionSync(() => {
      const cursor = this.currentCursor() + 1;
      this.storage.sql.exec("UPDATE broker_state SET current_cursor = ? WHERE id = 1", cursor);
      this.storage.sql.exec(
        "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid) VALUES (?, ?, ?, ?)",
        cursor,
        now,
        "status",
        headOid,
      );
      this.prune(now);
      return cursor;
    });
  }

  resume(after: number | null, headOid: string, now: number): ResumeResult {
    return this.storage.transactionSync(() => {
      this.prune(now);
      const cursor = this.currentCursor();
      if (after === null || after === cursor) return { cursor, replay: [], resync: false };
      if (after > cursor || !this.isRetainedCursor(after, cursor)) {
        return { cursor, replay: [], resync: true };
      }
      const replay = this.storage.sql
        .exec<StoredWake>(
          "SELECT cursor, head_oid FROM wake_events " +
            "WHERE cursor > ? AND head_oid = ? ORDER BY cursor ASC",
          after,
          headOid,
        )
        .toArray()
        .map((event) => event.cursor);
      return { cursor, replay, resync: false };
    });
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
