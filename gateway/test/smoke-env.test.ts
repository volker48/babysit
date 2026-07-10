import { describe, expect, it } from "vitest";
import { smokeChildEnvironment } from "../scripts/smoke-env.js";

describe("smokeChildEnvironment", () => {
  it("removes smoke secrets while preserving unrelated values", () => {
    const environment = smokeChildEnvironment(
      {
        PATH: "/usr/bin",
        UNRELATED_VALUE: "survives",
        WATCHER_TOKEN: "watcher-sentinel",
        WEBHOOK_SECRET: "webhook-sentinel",
      },
      { GH_VIEW_COUNTER: "/tmp/count", REAL_GH: "/usr/bin/gh" },
    );

    expect(environment).toMatchObject({
      PATH: "/usr/bin",
      UNRELATED_VALUE: "survives",
      GH_VIEW_COUNTER: "/tmp/count",
      REAL_GH: "/usr/bin/gh",
    });
    expect(environment).not.toHaveProperty("WATCHER_TOKEN");
    expect(environment).not.toHaveProperty("WEBHOOK_SECRET");
  });
});
