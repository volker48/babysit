import { expect } from "e2e";
import { test, type Step } from "../support/cli.js";

const args = ["gateway-webhook", "setup", "--repo", "example-org/example-repo"];
const secret = "synthetic-e2e-webhook-secret";
const hook = {
  id: 12,
  name: "web",
  active: true,
  events: [
    "check_run",
    "check_suite",
    "status",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_review_thread",
    "issue_comment",
  ],
  config: {
    url: "https://babysit.mindgoblin.pw/webhooks/github",
    content_type: "json",
    insecure_ssl: "0",
  },
};
function list(json: unknown): Step {
  return {
    command: "gh",
    args: ["api", "repos/example-org/example-repo/hooks?per_page=100&page=1"],
    json,
  };
}
for (const existing of [false, true]) {
  test(`webhook setup ${existing ? "updates an existing hook" : "creates a missing hook"} and verifies it`, async ({
    cli,
  }) => {
    const method = existing ? "PATCH" : "POST";
    const result = await cli.run(
      args,
      [
        list(existing ? [{ ...hook, active: false, events: ["push"] }] : []),
        {
          command: "gh",
          args: [
            "api",
            "--method",
            method,
            "--input",
            "-",
            `repos/example-org/example-repo/hooks${existing ? "/12" : ""}`,
          ],
          json: hook,
        },
        list([hook]),
      ],
      `${secret}\r\n`,
    );
    expect(result.code).toBe(0);
    expect(result.stdout).toContain(existing ? "updated" : "created");
    expect(result.stdout + result.stderr).not.toContain(secret);
    expect(JSON.parse(result.calls[1].stdin)).toMatchObject({
      active: true,
      events: hook.events,
      config: { ...hook.config, secret },
    });
    expect(result.calls[1].args.join(" ")).not.toContain(secret);
  });
}
test("webhook setup refuses duplicate matching hooks before mutation", async ({ cli }) => {
  const result = await cli.run(args, [list([hook, { ...hook, id: 13 }])], `${secret}\n`);
  expect(result.code).toBe(4);
  expect(result.stderr).toContain("multiple");
  expect(result.calls).toHaveLength(1);
});
test("webhook setup detects a failed reconciliation", async ({ cli }) => {
  const result = await cli.run(
    args,
    [list([]), { command: "gh", args: ["POST"], json: hook }, list([])],
    secret,
  );
  expect(result.code).toBe(4);
  expect(result.stderr).toContain("reconcil");
});
test("webhook setup redacts mutation errors containing the stdin secret", async ({ cli }) => {
  const result = await cli.run(
    args,
    [list([]), { command: "gh", args: ["POST"], code: 1, stderr: `failed request: ${secret}` }],
    secret,
  );
  expect(result.code).toBe(4);
  expect(result.stderr).not.toContain(secret);
});
test("webhook setup rejects empty stdin without contacting GitHub", async ({ cli }) => {
  const result = await cli.run(args);
  expect(result.code).toBe(4);
  expect(result.stderr).toContain("single line");
});
