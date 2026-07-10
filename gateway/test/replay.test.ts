import { env } from "cloudflare:workers";
import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { DEBOUNCE_WINDOW_MS, WAKE_RETENTION_MS, WakeHistory } from "../src/replay";
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

describe("durable wake delivery", () => {
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
