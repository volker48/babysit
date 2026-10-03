import type { E2EConfig } from "e2e";

export default {
  targets: [{ name: "local", platform: "api" }],
  tests: "tests/**/*.e2e.ts",
  workers: 1,
  retries: 0,
  timeout: 20_000,
  trace: "off",
} satisfies E2EConfig;
