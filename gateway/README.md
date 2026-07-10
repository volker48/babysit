# babysit gateway

The Worker accepts only GitHub `status` webhooks and wakes authenticated `babysit wait --events`
clients. It does not determine PR state: a wake asks the CLI to refetch GitHub authoritatively.

Deploy configuration is intentionally secret-free. Before deploying, set the two bindings:

```bash
cd gateway
pnpm exec wrangler secret put WEBHOOK_SECRET
pnpm exec wrangler secret put WATCHER_TOKEN
```

Point the GitHub webhook at `https://babysit.mindgoblin.pw/webhooks/github` and use a `status`
event. Configure CLI clients with `--gateway-url wss://babysit.mindgoblin.pw/watch/OWNER/REPO`.

Run `pnpm smoke -- --gateway-url ... --repository OWNER/REPO --watcher-token ... --webhook-secret ...`
after credentials and a deployed Worker exist. The smoke command signs a status payload and verifies
an authenticated watcher receives a wake. It prints, but does not execute, the CLI command for a
manual authoritative refetch; run that command with a real PR number and observe its `gh` fetch.
It neither deploys nor creates credentials.
