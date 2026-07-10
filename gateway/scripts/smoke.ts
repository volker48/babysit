import { createHmac } from "node:crypto";
import WebSocket from "ws";

const values = Object.fromEntries(
  process.argv
    .slice(2)
    .map((value, index, all) => [value, all[index + 1]])
    .filter(([key]) => key.startsWith("--")),
);
const gatewayUrl = values["--gateway-url"];
const repository = values["--repository"];
const watcherToken = values["--watcher-token"];
const webhookSecret = values["--webhook-secret"];
if (!gatewayUrl || !repository || !watcherToken || !webhookSecret) {
  throw new Error(
    "usage: pnpm smoke -- --gateway-url URL --repository OWNER/REPO --watcher-token TOKEN --webhook-secret SECRET",
  );
}

const webhookUrl = gatewayUrl
  .replace(/^wss:/, "https:")
  .replace(/\/watch\/.*$/, "/webhooks/github");
const body = JSON.stringify({ repository: { full_name: repository }, sha: "smoke-head" });
const signature = createHmac("sha256", webhookSecret).update(body).digest("hex");
const socket = new WebSocket(gatewayUrl, { headers: { Authorization: `Bearer ${watcherToken}` } });
await waitForOpen(socket);
socket.send(
  JSON.stringify({
    type: "register",
    version: 1,
    watch: { forge: "github", host: "github.com", repository, number: 0, headOid: "smoke-head" },
    after: null,
  }),
);
await waitForFrame(socket, "ready");
const response = await fetch(webhookUrl, {
  method: "POST",
  headers: {
    "content-type": "application/json",
    "x-github-event": "status",
    "x-hub-signature-256": `sha256=${signature}`,
  },
  body,
});
if (!response.ok) throw new Error(`webhook failed: ${response.status}`);
await waitForFrame(socket, "wake");
console.log("verified signed webhook to authenticated watcher wake; CLI refetch was not executed.");
console.log(
  `Run manually: babysit wait PR --repo ${repository} --events --gateway-url ${gatewayUrl}`,
);
socket.close();

function waitForOpen(socket: WebSocket): Promise<void> {
  return new Promise((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", () => reject(new Error("watcher connection failed")));
  });
}

function waitForFrame(socket: WebSocket, type: string): Promise<void> {
  return new Promise((resolve, reject) => {
    socket.once("message", (value) => {
      if (JSON.parse(value.toString()).type === type) resolve();
      else reject(new Error(`expected ${type} frame`));
    });
    socket.once("error", reject);
  });
}
