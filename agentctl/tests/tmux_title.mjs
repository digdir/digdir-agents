import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout } from "node:timers/promises";

// Arguments come from the runtime's actual command builders, not a copy of its policy.
const options = JSON.parse(process.argv[2]);
const attachment = JSON.parse(process.argv[3]);
const session = process.argv[4];
const agent = process.argv[5];
const directory = await mkdtemp(join(tmpdir(), "tmux-title-"));
const socket = join(directory, "socket");
const typescript = join(directory, "typescript");
const env = { ...process.env, TERM: "xterm-256color", TMUX: "" };
const { LC_ALL, LC_CTYPE, TMUX, ...inherited } = process.env;
const clientEnv = { ...inherited, ...attachment.environment };
const tmux = (...args) => execFileSync("tmux", ["-S", socket, "-f", "/dev/null", ...args], { encoding: "utf8", env }).trim();
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
// What the client wrote to the host terminal, as `script` records it.
const written = () => readFile(typescript, "latin1").catch(() => "");
const titled = (title) => `\x1b]0;${title}\x07`;
async function untilTitled(title) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if ((await written()).includes(titled(title))) return;
    await setTimeout(50);
  }
  assert.fail(`the terminal was never titled ${title}: ${JSON.stringify(await written())}`);
}

let client;
try {
  // Session names as the runtime records them: the tmux name is derived, the user's name is an option.
  const other = `${session}-other`;
  for (const [name, label] of [[session, "s1"], [other, "s2"]]) {
    tmux(...options, "new-session", "-d", "-x", "80", "-y", "10", "-s", name, "exec cat");
    tmux("set-option", "-t", `=${name}:`, "@agent_session_name", label);
  }

  const command = ["tmux", "-S", socket, ...attachment.arguments].map(quote).join(" ");
  client = spawn("script", ["-q", "-f", "-c", command, typescript], { env: clientEnv, stdio: ["pipe", "ignore", "ignore"] });
  await untilTitled(`${agent}/s1`);
  console.log("PASS: attaching names the host terminal after the agent and session");

  tmux("switch-client", "-n");
  await untilTitled(`${agent}/s2`);
  console.log("PASS: moving to the next session renames the host terminal");
} finally {
  client?.kill();
  try { tmux("kill-server"); } finally { await rm(directory, { recursive: true, force: true }); }
}
