import { spawn } from "node:child_process";
import { copyFile, chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test as base, expect } from "e2e";

export const root = fileURLToPath(new URL("../../", import.meta.url));
export async function fixture<T>(name: string): Promise<T> {
  return JSON.parse(await readFile(join(root, "tests/fixtures", name), "utf8")) as T;
}
export interface Step {
  command: "gh" | "glab" | "git";
  args: string[];
  json?: unknown;
  stdout?: string;
  stderr?: string;
  code?: number;
}
interface Call {
  command: string;
  args: string[];
  stdin: string;
}
interface Result {
  code: number | null;
  stdout: string;
  stderr: string;
  calls: Call[];
}
export interface Cli {
  run(args: string[], steps?: Step[], stdin?: string): Promise<Result>;
}

export const test = base.extend<{ cli: Cli }>({
  cli: async (_fixtures, use) => {
    const dir = await mkdtemp(join(tmpdir(), "babysit-e2e-"));
    try {
      for (const name of ["gh", "glab", "git"]) {
        const path = join(dir, name);
        await copyFile(new URL("forge-command.mjs", import.meta.url), path);
        await chmod(path, 0o755);
      }
      await use({
        async run(args, steps = [], stdin = "") {
          const script = join(dir, "script.json");
          await writeFile(script, JSON.stringify(steps));
          await writeFile(`${script}.calls`, "");
          const target = process.env.CARGO_TARGET_DIR
            ? resolve(process.env.CARGO_TARGET_DIR)
            : join(root, "target");
          const child = spawn(join(target, "debug/babysit"), args, {
            cwd: dir,
            env: {
              PATH: [dir, dirname(process.execPath), "/usr/bin", "/bin"].join(delimiter),
              BABYSIT_E2E_SCRIPT: script,
            },
            stdio: "pipe",
            timeout: 12_000,
          });
          let stdout = "";
          let stderr = "";
          child.stdout.setEncoding("utf8").on("data", (data: string) => {
            stdout += data;
          });
          child.stderr.setEncoding("utf8").on("data", (data: string) => {
            stderr += data;
          });
          const code = await new Promise<number | null>((resolve, reject) => {
            child.on("error", reject);
            child.on("close", (code, signal) => {
              if (signal) reject(new Error(`babysit terminated with ${signal}: ${stderr}`));
              else resolve(code);
            });
            child.stdin.on("error", reject);
            child.stdin.end(stdin);
          });
          const calls = (await readFile(`${script}.calls`, "utf8")).trim();
          expect(stderr).not.toContain("Unexpected external command");
          expect(JSON.parse(await readFile(script, "utf8"))).toEqual([]);
          return {
            code,
            stdout,
            stderr,
            calls: calls ? calls.split("\n").map((line) => JSON.parse(line) as Call) : [],
          };
        },
      });
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  },
});

export interface View {
  state: string;
  statusCheckRollup: unknown[];
  headRefOid: string;
  commits: { committedDate: string }[];
  [key: string]: unknown;
}
export async function githubView(): Promise<View> {
  const view = await fixture<View>("pr-view.json");
  return {
    ...view,
    state: "OPEN",
    statusCheckRollup: [
      { __typename: "CheckRun", name: "CI", status: "COMPLETED", conclusion: "SUCCESS" },
    ],
  };
}
export function connection(nodes: unknown[], cursor: string | null = null) {
  return { nodes, pageInfo: { hasNextPage: cursor !== null, endCursor: cursor } };
}
export function reviewPage(reviews: unknown[], threads: unknown[] = []) {
  return {
    data: {
      repository: {
        pullRequest: { reviews: connection(reviews), reviewThreads: connection(threads) },
      },
    },
  };
}
export function review(head: string, body = "", login = "cursor") {
  return { author: { login }, submittedAt: "2026-07-06T17:00:00Z", body, commit: { oid: head } };
}
export function thread(path: string, resolved = false, outdated = false, login = "cursor") {
  return {
    path,
    line: 7,
    startLine: null,
    isResolved: resolved,
    isOutdated: outdated,
    comments: {
      nodes: [
        {
          author: { login },
          body: "### Fix the bug\n\n**Medium Severity**\n\n<!-- DESCRIPTION START -->\nHandle this case.\n<!-- DESCRIPTION END -->",
        },
      ],
    },
  };
}
export function githubSteps(
  view: View,
  threads: unknown[] = [],
  reviews: unknown[] = [review(view.headRefOid)],
): Step[] {
  return [
    { command: "gh", args: ["pr", "view", "--json"], json: view },
    {
      command: "gh",
      args: ["api", "graphql", "owner=example-org", "name=example-repo", "number=63"],
      json: reviewPage(reviews, threads),
    },
  ];
}
