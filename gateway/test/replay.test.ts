import { env } from "cloudflare:workers";
import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { DEBOUNCE_WINDOW_MS, WAKE_RETENTION_MS, WakeHistory, type WakeIntent } from "../src/replay";
import type { WakeEvent } from "../src/wake";
import { RepositoryGateway } from "../src/worker";

const base = Date.now() + 60_000;

function wake(
  cursor: number,
  options: Partial<Pick<WakeEvent, "changeNumber" | "headRevision">> = {},
): WakeEvent {
  return {
    deliveryId: `delivery-${cursor}`,
    kind: "status",
    repository: { id: "100", fullName: "owner/repository" },
    receivedAt: base + cursor,
    ...options,
  };
}

function stub(repository: string): DurableObjectStub<RepositoryGateway> {
  return env.REPOSITORY_GATEWAY.get(env.REPOSITORY_GATEWAY.idFromName(repository));
}

async function withHistory<T>(
  repository: string,
  action: (history: WakeHistory, state: DurableObjectState) => Promise<T> | T,
): Promise<T> {
  return runInDurableObject(stub(repository), (_, state) =>
    action(new WakeHistory(state.storage), state),
  );
}

function tableColumns(state: DurableObjectState, table: string): string[] {
  return state.storage.sql
    .exec<{ name: string }>(`PRAGMA table_info(${table})`)
    .toArray()
    .map((column) => column.name);
}

describe("durable wake delivery", () => {
  it("migrates #9 repository names out of retained replay metadata", async () => {
    const repository = "schema-migration";
    await withHistory(repository, (_, state) => {
      state.storage.sql.exec("DROP TABLE wake_events");
      state.storage.sql.exec(
        "CREATE TABLE wake_events (cursor INTEGER PRIMARY KEY, received_at_ms INTEGER NOT NULL, " +
          "kind TEXT NOT NULL, head_oid TEXT, pr_number INTEGER, delivery_id TEXT, " +
          "repository_id TEXT, repository_full_name TEXT)",
      );
      state.storage.sql.exec(
        "INSERT INTO wake_events VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        1,
        base,
        "status",
        "head",
        null,
        "legacy-delivery",
        "100",
        "owner/repository",
      );
      state.storage.sql.exec(
        "UPDATE broker_state SET current_cursor = 1, intents_initialized = 0 WHERE id = 1",
      );
    });
    await evictDurableObject(stub(repository));
    await withHistory(repository, (history, state) => {
      expect(history.resume(0, { headRevision: "head" }, base)).toEqual({
        cursor: 1,
        replay: [1],
        resync: false,
      });
      expect(tableColumns(state, "wake_events")).not.toContain("repository_full_name");
    });
  });

  it("resyncs #9 wake rows that predate repository IDs", async () => {
    const repository = "schema-migration-without-repository-id";
    await withHistory(repository, (_, state) => {
      state.storage.sql.exec("DROP TABLE wake_events");
      state.storage.sql.exec(
        "CREATE TABLE wake_events (cursor INTEGER PRIMARY KEY, received_at_ms INTEGER NOT NULL, " +
          "kind TEXT NOT NULL, head_oid TEXT, pr_number INTEGER, delivery_id TEXT, " +
          "repository_full_name TEXT)",
      );
      state.storage.sql.exec(
        "INSERT INTO wake_events VALUES (?, ?, ?, ?, ?, ?, ?)",
        1,
        base,
        "status",
        "head",
        null,
        "legacy-delivery-without-id",
        "owner/repository",
      );
      state.storage.sql.exec(
        "UPDATE broker_state SET current_cursor = 1, intents_initialized = 0 WHERE id = 1",
      );
    });
    await evictDurableObject(stub(repository));
    await withHistory(repository, (history, state) => {
      expect(history.resume(0, { headRevision: "head" }, base)).toEqual({
        cursor: 1,
        replay: [],
        resync: true,
      });
      expect(tableColumns(state, "wake_events")).not.toContain("repository_full_name");
    });
  });

  it("migrates intermediate outbox rows without repository names", async () => {
    const repository = "outbox-schema-migration";
    await withHistory(repository, (_, state) => {
      state.storage.sql.exec("DROP TABLE wake_outbox");
      state.storage.sql.exec(
        "CREATE TABLE wake_outbox (cursor INTEGER PRIMARY KEY, retry_at_ms INTEGER NOT NULL, " +
          "received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, delivery_id TEXT NOT NULL, " +
          "repository_id TEXT NOT NULL, repository_full_name TEXT NOT NULL, pr_number INTEGER, " +
          "head_oid TEXT)",
      );
      state.storage.sql.exec(
        "INSERT INTO wake_outbox VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        7,
        base,
        base,
        "status",
        "outbox-delivery",
        "100",
        "owner/private",
        7,
        "head",
      );
    });
    await evictDurableObject(stub(repository));
    await withHistory(repository, async (history, state) => {
      expect(tableColumns(state, "wake_outbox")).toEqual([
        "cursor",
        "retry_at_ms",
        "received_at_ms",
        "kind",
        "delivery_id",
        "repository_id",
        "pr_number",
        "head_oid",
      ]);
      const sent: WakeIntent[] = [];
      await history.deliver(base, (intent) => sent.push(intent));
      expect(sent).toMatchObject([{ cursor: 7, wake: { deliveryId: "outbox-delivery" } }]);
    });
  });

  it("drops legacy outbox rows missing current columns during rebuild", async () => {
    // Characterizes current behavior: rows from unrecognized legacy shapes are dropped, not migrated.
    const repository = "outbox-schema-drop";
    await withHistory(repository, (_, state) => {
      state.storage.sql.exec("DROP TABLE wake_outbox");
      state.storage.sql.exec(
        "CREATE TABLE wake_outbox (cursor INTEGER PRIMARY KEY, retry_at_ms INTEGER NOT NULL, " +
          "received_at_ms INTEGER NOT NULL, kind TEXT NOT NULL, delivery_id TEXT NOT NULL, " +
          "pr_number INTEGER, head_oid TEXT)",
      );
      state.storage.sql.exec(
        "INSERT INTO wake_outbox VALUES (?, ?, ?, ?, ?, ?, ?)",
        8,
        base,
        base,
        "status",
        "dropped-delivery",
        8,
        "head",
      );
    });
    await evictDurableObject(stub(repository));
    await withHistory(repository, async (history, state) => {
      expect(tableColumns(state, "wake_outbox")).toEqual([
        "cursor",
        "retry_at_ms",
        "received_at_ms",
        "kind",
        "delivery_id",
        "repository_id",
        "pr_number",
        "head_oid",
      ]);
      expect(state.storage.sql.exec("SELECT * FROM wake_outbox").toArray()).toEqual([]);
      const sent: WakeIntent[] = [];
      await history.deliver(base, (intent) => sent.push(intent));
      expect(sent).toEqual([]);
    });
  });

  it("replays only leading and trailing logical intents from a burst", async () => {
    await withHistory("logical-replay", async (history) => {
      await history.accept(wake(1, { changeNumber: 7 }), base);
      await history.accept(wake(2, { changeNumber: 7 }), base + 1);
      await history.accept(wake(3, { changeNumber: 7 }), base + 2);
      expect(history.resume(0, { changeNumber: 7 }, base + DEBOUNCE_WINDOW_MS)).toEqual({
        cursor: 3,
        replay: [1, 3],
        resync: false,
      });
    });
  });

  it("cleans failed outbox, burst, and logical intent state at exact retention", async () => {
    await withHistory("expiry-cleanup", async (history, state) => {
      await history.accept(wake(1, { changeNumber: 7 }), base);
      await history.deliver(base, () => {
        throw new Error("persistent failure");
      });
      await history.deliver(base + WAKE_RETENTION_MS + 1, () => undefined);
      for (const table of ["wake_events", "wake_outbox", "wake_intents", "debounce_bursts"]) {
        expect(state.storage.sql.exec(`SELECT * FROM ${table}`).toArray()).toEqual([]);
      }
    });
  });

  it("schedules retention cleanup when an upgraded object activates with SQL history", async () => {
    const repository = "idle-upgrade";
    const receivedAt = base + 9;
    await withHistory(repository, (_, state) => {
      state.storage.sql.exec(
        "INSERT INTO wake_events (cursor, received_at_ms, kind, head_oid, delivery_id, repository_id) " +
          "VALUES (?, ?, ?, ?, ?, ?)",
        1,
        receivedAt,
        "status",
        "head",
        "upgrade-delivery",
        "100",
      );
      state.storage.sql.exec("UPDATE broker_state SET current_cursor = 1 WHERE id = 1");
    });
    await evictDurableObject(stub(repository));
    await withHistory(repository, async (_, state) => {
      expect(await state.storage.getAlarm()).toBe(receivedAt + WAKE_RETENTION_MS);
    });
  });

  it("retains a duplicate without allocating a cursor or outbox intent", async () => {
    await withHistory("duplicate", async (history, state) => {
      expect(await history.accept(wake(1, { changeNumber: 7 }), base)).toEqual({
        cursor: 1,
        duplicate: false,
      });
      expect(await history.accept(wake(1, { changeNumber: 7 }), base + 1)).toEqual({
        cursor: 1,
        duplicate: true,
      });
      expect(state.storage.sql.exec("SELECT cursor FROM wake_events").toArray()).toEqual([
        { cursor: 1 },
      ]);
      expect(state.storage.sql.exec("SELECT cursor FROM wake_outbox").toArray()).toEqual([
        { cursor: 1 },
      ]);
      expect(tableColumns(state, "wake_events")).toEqual([
        "cursor",
        "received_at_ms",
        "kind",
        "head_oid",
        "pr_number",
        "delivery_id",
        "repository_id",
      ]);
      expect(tableColumns(state, "wake_outbox")).toEqual([
        "cursor",
        "retry_at_ms",
        "received_at_ms",
        "kind",
        "delivery_id",
        "repository_id",
        "pr_number",
        "head_oid",
      ]);
    });
  });

  it("rolls back cursor, history, and intent when history insertion fails", async () => {
    await withHistory("rollback", async (history, state) => {
      state.storage.sql.exec(
        "CREATE TRIGGER reject_wake BEFORE INSERT ON wake_events BEGIN " +
          "SELECT RAISE(ABORT, 'reject wake'); END",
      );
      await expect(history.accept(wake(1), base)).rejects.toThrow("reject wake");
      expect(state.storage.sql.exec("SELECT current_cursor FROM broker_state").toArray()).toEqual([
        { current_cursor: 0 },
      ]);
      expect(state.storage.sql.exec("SELECT * FROM wake_outbox").toArray()).toEqual([]);
    });
  });

  it("recovers a persisted leading intent after eviction", async () => {
    const repository = "recovery";
    await withHistory(repository, (history) => history.accept(wake(1), base));
    await evictDurableObject(stub(repository));
    const sent: number[] = [];
    await withHistory(repository, (history) =>
      history.deliver(base, (intent) => sent.push(intent.cursor)),
    );
    expect(sent).toEqual([1]);
  });

  it("uses a fixed boundary and emits no trailing intent for a single event", async () => {
    await withHistory("single", async (history) => {
      await history.accept(wake(1, { changeNumber: 7 }), base);
      const sent: number[] = [];
      await history.deliver(base, (intent) => sent.push(intent.cursor));
      await history.deliver(base + DEBOUNCE_WINDOW_MS, (intent) => sent.push(intent.cursor));
      expect(sent).toEqual([1]);
    });
  });

  it("emits leading then final pending cursor at the fixed boundary", async () => {
    await withHistory("trailing", async (history) => {
      await history.accept(wake(1, { changeNumber: 7 }), base);
      await history.accept(wake(2, { changeNumber: 7, headRevision: "new-head" }), base + 1);
      const sent: WakeEvent[] = [];
      await history.deliver(base, (intent) => sent.push(intent.wake));
      await history.deliver(base + DEBOUNCE_WINDOW_MS, (intent) => sent.push(intent.wake));
      expect(sent.map((event) => event.deliveryId)).toEqual(["delivery-1", "delivery-2"]);
      expect(sent[1]).toMatchObject({ changeNumber: 7, headRevision: "new-head" });
    });
  });

  it("materializes a delayed trailing intent before beginning the next burst", async () => {
    await withHistory("delayed", async (history) => {
      await history.accept(wake(1, { changeNumber: 7 }), base);
      await history.deliver(base, () => undefined);
      await history.accept(wake(2, { changeNumber: 7 }), base + 1);
      await history.accept(wake(3, { changeNumber: 7 }), base + DEBOUNCE_WINDOW_MS);
      const sent: number[] = [];
      await history.deliver(base + DEBOUNCE_WINDOW_MS, (intent) => sent.push(intent.cursor));
      expect(sent).toEqual([2, 3]);
    });
  });

  it("keeps independent PR, head, and repository bursts separate", async () => {
    await withHistory("routes", async (history, state) => {
      await history.accept(wake(1, { changeNumber: 1 }), base);
      await history.accept(wake(2, { changeNumber: 2 }), base);
      await history.accept(wake(3, { headRevision: "head" }), base);
      await history.accept(wake(4), base);
      expect(
        state.storage.sql
          .exec<{ route_key: string }>("SELECT route_key FROM debounce_bursts ORDER BY route_key")
          .toArray(),
      ).toEqual([
        { route_key: "head:head" },
        { route_key: "pr:1" },
        { route_key: "pr:2" },
        { route_key: "repository" },
      ]);
    });
  });

  it("retains ordered outbox intents after a broadcast failure", async () => {
    await withHistory("failure", async (history) => {
      await history.accept(wake(1, { changeNumber: 1 }), base);
      await history.accept(wake(2, { changeNumber: 2 }), base);
      const attempts: number[] = [];
      await history.deliver(base, (intent) => {
        attempts.push(intent.cursor);
        throw new Error("send failed");
      });
      expect(attempts).toEqual([1]);
      await history.deliver(base + 1_000, (intent) => attempts.push(intent.cursor));
      expect(attempts).toEqual([1, 1, 2]);
    });
  });

  it("KNOWN BUG (inverted by plan 004): tail intents deliver before a deferred cursor retries", async () => {
    // Plan 004 will invert this characterization: a deferred failure must also defer the tail
    // so retries preserve cursor order.
    await withHistory("out-of-order-retry", async (history, state) => {
      await history.accept(wake(1, { changeNumber: 1 }), base);
      await history.accept(wake(2, { changeNumber: 2 }), base);
      const attempts: number[] = [];
      await history.deliver(base, (intent) => {
        attempts.push(intent.cursor);
        if (intent.cursor === 1) throw new Error("send failed");
      });
      expect(attempts).toEqual([1]);
      expect(await state.storage.getAlarm()).toBe(base);
      await history.deliver(base + 1, (intent) => attempts.push(intent.cursor));
      expect(attempts).toEqual([1, 2]);
    });
  });

  it("prunes history exactly at six hours and schedules the earliest obligation", async () => {
    await withHistory("retention", async (history, state) => {
      await history.accept(wake(1), base);
      await history.deliver(base, () => undefined);
      expect(await state.storage.getAlarm()).toBe(base + DEBOUNCE_WINDOW_MS);
      await history.deliver(base + WAKE_RETENTION_MS + 1, () => undefined);
      expect(state.storage.sql.exec("SELECT * FROM wake_events").toArray()).toEqual([]);
      expect(await state.storage.getAlarm()).toBeNull();
    });
  });
});
