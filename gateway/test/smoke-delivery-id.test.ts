import { describe, expect, it } from "vitest";
import { smokeDeliveryId } from "../scripts/smoke-delivery-id.js";

describe("smokeDeliveryId", () => {
  it("creates nonempty, GitHub-like delivery IDs that differ per invocation", () => {
    const first = smokeDeliveryId();
    const second = smokeDeliveryId();

    expect(first).toMatch(
      /^gateway-smoke-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
    );
    expect(second).toMatch(
      /^gateway-smoke-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
    );
    expect(first).not.toBe(second);
  });
});
