#!/usr/bin/env node
import { readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { basename } from "node:path";

// Script only the external command boundary; an unexpected call always fails.
const path = process.env.BABYSIT_E2E_SCRIPT;
const steps = JSON.parse(readFileSync(path, "utf8"));
const step = steps.shift();
const command = basename(process.argv[1]);
const args = process.argv.slice(2);
const stdin = args.includes("--input") ? readFileSync(0, "utf8") : "";
appendFileSync(`${path}.calls`, `${JSON.stringify({ command, args, stdin })}\n`);
if (!step || step.command !== command || !step.args.every((arg) => args.includes(arg))) {
  console.error(`Unexpected external command: ${command} ${JSON.stringify(args)}`);
  process.exit(97);
}
writeFileSync(path, JSON.stringify(steps));
if (step.stderr) process.stderr.write(step.stderr);
process.stdout.write(step.stdout ?? JSON.stringify(step.json ?? null));
process.exit(step.code ?? 0);
