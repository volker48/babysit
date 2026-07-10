import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest({
      miniflare: {
        bindings: {
          WATCHER_TOKEN: "watcher-test-token",
          WEBHOOK_SECRET: "webhook-test-secret",
        },
      },
      wrangler: { configPath: "./wrangler.toml" },
    }),
  ],
});
