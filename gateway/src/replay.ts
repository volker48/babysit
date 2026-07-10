export const WAKE_RETENTION_MS = 6 * 60 * 60 * 1000;
export const DEBOUNCE_WINDOW_MS = 2_000;

export type WakeKind = string;

export interface WakeInput {
  deliveryId: string;
  kind: WakeKind;
  repositoryId: number;
  prNumber: number | null;
  headOid: string | null;
}

export interface StoredWake extends WakeInput {
  cursor: number;
  receivedAtMs: number;
}

export interface ResumeResult {
  cursor: number;
  replay: number[];
  resync: boolean;
}

export interface AcceptedWake {
  wake: StoredWake | null;
  alarmAt: number | null;
}

interface WakeRow {
  [key: string]: SqlStorageValue;
  cursor: number;
  delivery_id: string | null;
  kind: WakeKind;
  repository_id: number | null;
  pr_number: number | null;
  head_oid: string | null;
  received_at_ms: number;
}

interface BrokerState {
  [key: string]: SqlStorageValue;
  current_cursor: number;
  debounce_leading_cursor: number | null;
  debounce_pending_cursor: number | null;
  debounce_until_ms: number | null;
}

/** Persists compact wake replay history and delivery deduplication for one repository. */
export class WakeHistory {
  constructor(private readonly storage: DurableObjectStorage) {
    this.createBrokerState();
    this.migrateWakeEvents();
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS delivery_dedupe (delivery_id TEXT PRIMARY KEY, " +
        "received_at_ms INTEGER NOT NULL)",
    );
    this.storage.sql.exec(
      "CREATE INDEX IF NOT EXISTS delivery_dedupe_received_at ON delivery_dedupe (received_at_ms)",
    );
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

  accept(input: WakeInput, now: number): AcceptedWake {
    return this.storage.transactionSync(() => {
      this.prune(now);
      if (this.hasDelivery(input.deliveryId)) {
        return { wake: null, alarmAt: this.brokerState().debounce_until_ms };
      }

      this.storage.sql.exec(
        "INSERT INTO delivery_dedupe (delivery_id, received_at_ms) VALUES (?, ?)",
        input.deliveryId,
        now,
      );
      const wake = this.append(input, now);
      const state = this.brokerState();
      if (state.debounce_until_ms === null || state.debounce_until_ms <= now) {
        const alarmAt = now + DEBOUNCE_WINDOW_MS;
        this.storage.sql.exec(
          "UPDATE broker_state SET debounce_leading_cursor = ?, debounce_pending_cursor = ?, " +
            "debounce_until_ms = ? WHERE id = 1",
          wake.cursor,
          wake.cursor,
          alarmAt,
        );
        return { wake, alarmAt };
      }

      this.storage.sql.exec(
        "UPDATE broker_state SET debounce_pending_cursor = ? WHERE id = 1",
        wake.cursor,
      );
      return { wake: null, alarmAt: null };
    });
  }

  takeTrailingWake(now: number): { wake: StoredWake | null; alarmAt: number | null } {
    return this.storage.transactionSync(() => {
      const state = this.brokerState();
      if (state.debounce_until_ms === null || state.debounce_leading_cursor === null) {
        return { wake: null, alarmAt: null };
      }
      if (state.debounce_until_ms > now) return { wake: null, alarmAt: state.debounce_until_ms };

      this.storage.sql.exec(
        "UPDATE broker_state SET debounce_leading_cursor = NULL, debounce_pending_cursor = NULL, " +
          "debounce_until_ms = NULL WHERE id = 1",
      );
      if (state.debounce_pending_cursor === state.debounce_leading_cursor) {
        return { wake: null, alarmAt: null };
      }
      return { wake: this.wakeAt(state.debounce_pending_cursor), alarmAt: null };
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
        .exec<WakeRow>(
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

  private createBrokerState(): void {
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS broker_state (id INTEGER PRIMARY KEY CHECK (id = 1), " +
        "current_cursor INTEGER NOT NULL)",
    );
    this.addBrokerColumn("debounce_leading_cursor");
    this.addBrokerColumn("debounce_pending_cursor");
    this.addBrokerColumn("debounce_until_ms");
    this.storage.sql.exec("INSERT OR IGNORE INTO broker_state (id, current_cursor) VALUES (1, 0)");
  }

  private addBrokerColumn(column: string): void {
    if (this.tableColumns("broker_state").includes(column)) return;
    this.storage.sql.exec(`ALTER TABLE broker_state ADD COLUMN ${column} INTEGER`);
  }

  private migrateWakeEvents(): void {
    const columns = this.tableColumns("wake_events");
    if (columns.length > 0 && !columns.includes("repository_id")) {
      this.storage.transactionSync(() => {
        this.storage.sql.exec("ALTER TABLE wake_events RENAME TO wake_events_legacy");
        this.createWakeEvents();
        this.storage.sql.exec(
          "INSERT INTO wake_events " +
            "(cursor, delivery_id, kind, repository_id, pr_number, head_oid, received_at_ms) " +
            "SELECT cursor, delivery_id, kind, NULL, pr_number, head_oid, received_at_ms " +
            "FROM wake_events_legacy",
        );
        this.storage.sql.exec("DROP TABLE wake_events_legacy");
      });
    } else {
      this.createWakeEvents();
    }
    this.storage.sql.exec(
      "CREATE INDEX IF NOT EXISTS wake_events_received_at ON wake_events (received_at_ms)",
    );
  }

  private createWakeEvents(): void {
    this.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS wake_events (" +
        "cursor INTEGER PRIMARY KEY, delivery_id TEXT, kind TEXT NOT NULL, repository_id INTEGER, " +
        "pr_number INTEGER, head_oid TEXT, received_at_ms INTEGER NOT NULL)",
    );
  }

  private append(input: WakeInput, now: number): StoredWake {
    const cursor = this.currentCursor() + 1;
    this.storage.sql.exec("UPDATE broker_state SET current_cursor = ? WHERE id = 1", cursor);
    this.storage.sql.exec(
      "INSERT INTO wake_events " +
        "(cursor, delivery_id, kind, repository_id, pr_number, head_oid, received_at_ms) " +
        "VALUES (?, ?, ?, ?, ?, ?, ?)",
      cursor,
      input.deliveryId,
      input.kind,
      input.repositoryId,
      input.prNumber,
      input.headOid,
      now,
    );
    return { ...input, cursor, receivedAtMs: now };
  }

  private wakeAt(cursor: number | null): StoredWake | null {
    if (cursor === null) return null;
    const row = this.storage.sql
      .exec<WakeRow>(
        "SELECT cursor, delivery_id, kind, repository_id, pr_number, head_oid, received_at_ms " +
          "FROM wake_events WHERE cursor = ?",
        cursor,
      )
      .toArray()[0];
    if (!row || row.delivery_id === null || row.repository_id === null) return null;
    return {
      cursor: row.cursor,
      deliveryId: row.delivery_id,
      kind: row.kind,
      repositoryId: row.repository_id,
      prNumber: row.pr_number,
      headOid: row.head_oid,
      receivedAtMs: row.received_at_ms,
    };
  }

  private brokerState(): BrokerState {
    return this.storage.sql
      .exec<BrokerState>(
        "SELECT current_cursor, debounce_leading_cursor, debounce_pending_cursor, debounce_until_ms " +
          "FROM broker_state WHERE id = 1",
      )
      .one();
  }

  private currentCursor(): number {
    return this.brokerState().current_cursor;
  }

  private hasDelivery(deliveryId: string): boolean {
    return (
      this.storage.sql
        .exec<{ delivery_id: string }>(
          "SELECT delivery_id FROM delivery_dedupe WHERE delivery_id = ?",
          deliveryId,
        )
        .toArray().length > 0
    );
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
    const cutoff = now - WAKE_RETENTION_MS;
    this.storage.sql.exec("DELETE FROM wake_events WHERE received_at_ms < ?", cutoff);
    this.storage.sql.exec("DELETE FROM delivery_dedupe WHERE received_at_ms < ?", cutoff);
  }

  private tableColumns(table: string): string[] {
    return this.storage.sql
      .exec<{ name: string }>(`PRAGMA table_info(${table})`)
      .toArray()
      .map((column) => column.name);
  }
}
