import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { expect } from "e2e";
import {
  test,
  fixture,
  root,
  githubView,
  githubSteps,
  reviewPage,
  review,
  thread,
  type Step,
} from "../support/cli.js";

const github = ["63", "--repo", "example-org/example-repo", "--forge", "github"];

for (const args of [
  ["--help"],
  ["status", "--help"],
  ["findings", "--help"],
  ["wait", "--help"],
  ["gateway-token", "--help"],
  ["gateway-webhook", "setup", "--help"],
]) {
  test(`help: ${args.join(" ")}`, async ({ cli }) => {
    const result = await cli.run(args);
    expect(result.code).toBe(0);
    expect(result.stdout).toContain("Usage:");
    expect(result.calls).toEqual([]);
  });
}
test("version matches the crate", async ({ cli }) => {
  const manifest = await readFile(join(root, "Cargo.toml"), "utf8");
  const version = /^version = "([^"]+)"/m.exec(manifest)?.[1];
  expect((await cli.run(["--version"])).stdout.trim()).toBe(`babysit ${version}`);
});
for (const args of [
  [],
  ["invalid"],
  ["status", "--all"],
  ["findings", "--no-reviews"],
  ["wait", "--interval", "0"],
  ["wait", "--events"],
  ["status", "--bots", ","],
  ["gateway-webhook", "setup", "--repo", "../repo"],
]) {
  test(`usage errors exit 4: ${args.join(" ") || "no command"}`, async ({ cli }) => {
    const result = await cli.run(args);
    expect(result.code).toBe(4);
    expect(result.stderr).toContain(args.length ? "error:" : "Usage:");
    expect(result.calls).toEqual([]);
  });
}
for (const [name, check, findings, code] of [
  ["clean", "SUCCESS", false, 0],
  ["findings", "SUCCESS", true, 1],
  ["failure takes precedence over findings", "FAILURE", true, 2],
  ["pending takes precedence over findings", null, true, 3],
] as const) {
  test(`GitHub status: ${name}`, async ({ cli }) => {
    const view = await githubView();
    view.statusCheckRollup = [
      { name: "CI", status: check ? "COMPLETED" : "IN_PROGRESS", conclusion: check },
    ];
    const result = await cli.run(
      ["status", ...github],
      githubSteps(view, findings ? [thread("src/bug.rs")] : []),
    );
    expect(result.code).toBe(code);
    expect(result.stdout).toContain(code === 3 ? "PENDING" : "SETTLED");
    expect(result.stdout).toContain(`findings=${findings ? 1 : 0}`);
    expect(result.calls[0].args.slice(0, 5)).toEqual([
      "pr",
      "view",
      "63",
      "-R",
      "example-org/example-repo",
    ]);
  });
}
test("reviews are required unless --no-reviews is set", async ({ cli }) => {
  const view = await githubView();
  expect((await cli.run(["status", ...github], githubSteps(view, [], []))).code).toBe(3);
  expect(
    (await cli.run(["status", ...github, "--no-reviews"], githubSteps(view, [], []))).code,
  ).toBe(0);
});
for (const state of ["MERGED", "CLOSED"]) {
  test(`${state} changes settle without reviews`, async ({ cli }) => {
    const view = await githubView();
    view.state = state;
    expect((await cli.run(["status", ...github], githubSteps(view, [], []))).code).toBe(0);
  });
}
test("findings filters humans, resolved and outdated threads; --all includes inactive findings", async ({
  cli,
}) => {
  const view = await githubView();
  const threads = [
    thread("active.rs"),
    thread("resolved.rs", true),
    thread("outdated.rs", false, true),
    thread("human.rs", false, false, "human"),
  ];
  const normal = await cli.run(["findings", ...github], githubSteps(view, threads));
  expect(normal.code).toBe(0);
  expect(normal.stdout).toContain("findings (1)");
  expect(normal.stdout).toContain("active.rs:7");
  expect(normal.stdout).not.toContain("resolved.rs");
  const all = await cli.run(["findings", ...github, "--all"], githubSteps(view, threads));
  expect(all.stdout).toContain("findings (3)");
  expect(all.stdout).toContain("resolved.rs");
  expect(all.stdout).toContain("outdated.rs");
  expect(all.stdout).not.toContain("human.rs");
});
test("custom bot selection controls both settlement and findings", async ({ cli }) => {
  const view = await githubView();
  const result = await cli.run(
    ["status", ...github, "--bots", "custom-bot"],
    githubSteps(
      view,
      [thread("custom.rs", false, false, "custom-bot"), thread("ignored.rs")],
      [review(view.headRefOid, "", "custom-bot")],
    ),
  );
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("findings=1");
  expect(result.stdout).toContain("review custom-bot");
});
test("CodeRabbit nitpicks are opt in", async ({ cli }) => {
  const view = await githubView();
  const body = await readFile(join(root, "tests/fixtures/coderabbit-review-body.md"), "utf8");
  const reviews = [review(view.headRefOid, body, "coderabbitai")];
  expect((await cli.run(["findings", ...github], githubSteps(view, [], reviews))).stdout).toContain(
    "findings: none",
  );
  expect(
    (await cli.run(["findings", ...github, "--nitpicks"], githubSteps(view, [], reviews))).stdout,
  ).toContain("findings (");
});
test("wait polls pending checks until they settle and prints findings", async ({ cli }) => {
  const view = await githubView();
  const pending = { ...view, statusCheckRollup: [{ name: "CI", status: "IN_PROGRESS" }] };
  const result = await cli.run(
    ["wait", ...github, "--interval", "1", "--timeout", "8"],
    [...githubSteps(pending), ...githubSteps(view, [thread("bug.rs")])],
  );
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("SETTLED");
  expect(result.stdout).toContain("bug.rs:7");
});
test("wait --all prints resolved findings even when nothing is unresolved", async ({ cli }) => {
  const result = await cli.run(
    ["wait", ...github, "--all"],
    githubSteps(await githubView(), [
      thread("resolved.rs", true),
      thread("outdated.rs", false, true),
    ]),
  );
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("findings (2)");
  expect(result.stdout).toContain("resolved.rs");
  expect(result.stdout).toContain("outdated.rs");
});
test("wait deadline reports the last authoritative pending snapshot", async ({ cli }) => {
  const view = await githubView();
  const result = await cli.run(
    ["wait", ...github, "--timeout", "1", "--interval", "5"],
    githubSteps(view, [], []),
  );
  expect(result.code).toBe(3);
  expect(result.stdout).toContain("TIMEOUT");
  expect(result.stdout).toContain("findings=0");
});
test("wait retries an external CLI failure", async ({ cli }) => {
  const result = await cli.run(
    ["wait", ...github, "--timeout", "8", "--interval", "1"],
    [
      { command: "gh", args: ["pr", "view"], code: 1, stderr: "HTTP 503 unavailable" },
      ...githubSteps(await githubView()),
    ],
  );
  expect(result.code).toBe(0);
});
for (const response of [{ stdout: "not json" }, { code: 1, stderr: "HTTP 403 forbidden" }]) {
  test(`forge failure: ${response.stderr ?? response.stdout}`, async ({ cli }) => {
    const result = await cli.run(
      ["status", ...github],
      [{ command: "gh", args: ["pr", "view"], ...response }],
    );
    expect(result.code).toBe(4);
    expect(result.stderr).toContain("gh pr view");
  });
}
test("GraphQL pagination fetches subsequent review threads", async ({ cli }) => {
  const view = await githubView();
  const first = reviewPage([review(view.headRefOid)]);
  first.data.repository.pullRequest.reviewThreads.pageInfo = {
    hasNextPage: true,
    endCursor: "next-thread",
  };
  const steps = githubSteps(view);
  steps[1].json = first;
  steps.push({
    command: "gh",
    args: ["graphql", "reviewThreadsCursor=next-thread"],
    json: reviewPage([], [thread("page-two.rs")]),
  });
  const result = await cli.run(["findings", ...github], steps);
  expect(result.stdout).toContain("page-two.rs");
});
test("malformed GraphQL data fails rather than reporting clean", async ({ cli }) => {
  const steps = githubSteps(await githubView());
  steps[1].json = { data: { repository: null } };
  expect((await cli.run(["status", ...github], steps)).code).toBe(4);
});
test("auto-detection and omitted PR use the current GitLab branch", async ({ cli }) => {
  const steps = await gitlabSteps();
  const result = await cli.run(
    ["status", "--no-reviews"],
    [
      {
        command: "git",
        args: ["remote", "get-url", "origin"],
        stdout: "git@gitlab.example.com:group/subgroup/project.git\n",
      },
      ...steps,
    ],
  );
  expect(result.code).toBe(0);
  expect(result.calls[1].args).toEqual(["mr", "view", "-F", "json"]);
});
async function gitlabSteps(
  options: { status?: string; discussions?: unknown[]; state?: string } = {},
): Promise<Step[]> {
  const mr = await fixture<Record<string, unknown>>("gitlab-mr-open.json");
  return [
    { command: "glab", args: ["mr", "view"], json: { ...mr, state: options.state ?? "opened" } },
    {
      command: "glab",
      args: [
        "projects/1234/pipelines/9876/jobs?per_page=100&page=1",
        "--hostname",
        "gitlab.example.com",
      ],
      json: [{ name: "CI", status: options.status ?? "success" }],
    },
    {
      command: "glab",
      args: ["projects/1234/merge_requests/42/discussions?per_page=100&page=1"],
      json: options.discussions ?? [],
    },
    {
      command: "glab",
      args: [`projects/1234/repository/commits/${mr.sha}`],
      json: { committed_date: "2026-07-06T12:00:00Z" },
    },
  ];
}
for (const [status, code] of [
  ["success", 0],
  ["failed", 2],
  ["running", 3],
] as const) {
  test(`GitLab status: ${status}`, async ({ cli }) => {
    const result = await cli.run(
      ["status", "42", "--forge", "gitlab", "--repo", "group/subgroup/project", "--no-reviews"],
      await gitlabSteps({ status }),
    );
    expect(result.code).toBe(code);
    expect(result.stdout).toContain("Add GitLab support");
  });
}
test("GitLab findings and resolved filtering", async ({ cli }) => {
  const discussions = await fixture<unknown[]>("gitlab-discussions.json");
  const normal = await cli.run(
    ["findings", "42", "--forge", "gitlab"],
    await gitlabSteps({ discussions }),
  );
  expect(normal.code).toBe(0);
  expect(normal.stdout).toContain("findings (1)");
  expect(normal.stdout).toContain("src/gitlab.ts:27");
  const all = await cli.run(
    ["findings", "42", "--forge", "gitlab", "--all"],
    await gitlabSteps({ discussions }),
  );
  expect(all.stdout).toContain("findings (2)");
  expect(all.stdout).toContain("src/old.ts:9");
});
test("GitLab wait settles after the bot review", async ({ cli }) => {
  const discussions = await fixture<unknown[]>("gitlab-discussions.json");
  const result = await cli.run(
    ["wait", "42", "--forge", "gitlab", "--interval", "1", "--timeout", "8"],
    [...(await gitlabSteps()), ...(await gitlabSteps({ discussions }))],
  );
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("SETTLED");
});
for (const [forge, url, expected] of [
  ["gitlab", "wss://gateway.test/watch", "only for GitHub"],
  ["github", "ws://gateway.test/watch", "wss://"],
  ["github", "wss://gateway.test/watch?token=bad", "wss://host/watch"],
]) {
  test(`invalid event mode: ${forge} ${url}`, async ({ cli }) => {
    const result = await cli.run(["wait", "--forge", forge, "--events", "--gateway-url", url]);
    expect(result.code).toBe(4);
    expect(result.stderr).toContain(expected);
  });
}
for (const action of ["enroll", "rotate"]) {
  test(`token ${action} rejects invalid input before touching Keychain`, async ({ cli }) => {
    const result = await cli.run(["gateway-token", action], [], "two\nlines\n");
    expect(result.code).toBe(4);
    expect(result.stderr).toContain("single line");
    expect(result.stderr).not.toContain("two");
  });
}
for (const number of ["0", "000", "18446744073709551616"]) {
  test(`invalid change number is rejected before invoking a forge: ${number}`, async ({ cli }) => {
    const result = await cli.run(["status", number, "--forge", "github"]);
    expect(result.code).toBe(4);
    expect(result.calls).toEqual([]);
    expect(result.stderr).toContain("invalid PR number");
  });
}
test("wait times out without a snapshot after a retryable forge failure", async ({ cli }) => {
  const result = await cli.run(
    ["wait", ...github, "--timeout", "1", "--interval", "5"],
    [{ command: "gh", args: ["pr", "view"], code: 1, stderr: "HTTP 503 unavailable" }],
  );
  expect(result.code).toBe(3);
  expect(result.stdout).toContain("TIMEOUT: no authoritative snapshot");
});
test("GitLab malformed MR data fails before calling other APIs", async ({ cli }) => {
  const result = await cli.run(
    ["status", "--forge", "gitlab"],
    [{ command: "glab", args: ["mr", "view"], json: {} }],
  );
  expect(result.code).toBe(4);
  expect(result.stderr).toContain("glab mr view");
});
test("GitLab job pagination preserves a failed check on the second page", async ({ cli }) => {
  const steps = await gitlabSteps();
  steps[1].json = Array.from({ length: 100 }, (_, index) => ({
    name: `CI ${index}`,
    status: "success",
  }));
  steps.splice(2, 0, {
    command: "glab",
    args: ["projects/1234/pipelines/9876/jobs?per_page=100&page=2"],
    json: [{ name: "late failure", status: "failed" }],
  });
  const result = await cli.run(["status", "42", "--forge", "gitlab", "--no-reviews"], steps);
  expect(result.code).toBe(2);
  expect(result.stdout).toContain("late failure");
  expect(result.stdout).toContain("checks=100/101");
});
test("wait --all also retains inactive findings on timeout", async ({ cli }) => {
  const result = await cli.run(
    ["wait", ...github, "--all", "--timeout", "1", "--interval", "5"],
    githubSteps(await githubView(), [thread("resolved.rs", true)], []),
  );
  expect(result.code).toBe(3);
  expect(result.stdout).toContain("TIMEOUT");
  expect(result.stdout).toContain("resolved.rs");
});
