import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

__REPORTER__

let raw = "";
process.stdin.setEncoding("utf8");
for await (const chunk of process.stdin) raw += chunk;

let state;
try {
  state = JSON.parse(raw);
} catch {
  process.exit(0);
}

const clean = (value) => String(value).replace(/[\u0000-\u001f\u007f-\u009f]/g, "");
const percentage = (value) => {
  if (value === null || value === undefined) return null;
  const number = Number(value);
  return Number.isFinite(number) ? Math.round(Math.min(100, Math.max(0, number))) : null;
};
const remaining = (window) => {
  const used = percentage(window?.used_percentage);
  return used === null ? null : 100 - used;
};
const compactPath = (path) => {
  const home = homedir();
  if (path === home) return "~";
  return path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;
};
const branch = (cwd) => {
  try {
    return execFileSync("git", ["-C", cwd, "symbolic-ref", "--quiet", "--short", "HEAD"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: 250,
    }).trim();
  } catch {
    return "";
  }
};

const segments = [];
const model = [state.model?.display_name, state.effort?.level].filter(Boolean).map(clean).join(" ");
if (model) segments.push(model);

const cwd = state.workspace?.current_dir ?? state.cwd;
if (cwd) {
  segments.push(clean(compactPath(cwd)));
  const currentBranch = branch(cwd);
  if (currentBranch) segments.push(clean(currentBranch));
}

const contextUsed = percentage(state.context_window?.used_percentage);
if (contextUsed !== null) segments.push(`Context ${contextUsed}% used`);

const fiveHourLeft = remaining(state.rate_limits?.five_hour);
if (fiveHourLeft !== null) segments.push(`5h ${fiveHourLeft}% left`);
const weeklyLeft = remaining(state.rate_limits?.seven_day);
if (weeklyLeft !== null) segments.push(`weekly ${weeklyLeft}% left`);

if (state.version) segments.push(clean(state.version));
if (typeof state.fast_mode === "boolean") segments.push(`Fast ${state.fast_mode ? "on" : "off"}`);

process.stdout.write(segments.join(" · "));

// agentd gives agents the login with the most of its limits left, and only the harness learns how
// much of a Claude login is used. A reading is sent when it changes, or again after ten minutes,
// since agentd forgets readings when it restarts.
const windows = ["five_hour", "seven_day"]
  .map((name) => state.rate_limits?.[name])
  .filter((window) => Number.isFinite(Number(window?.used_percentage)) && Number.isFinite(Number(window?.resets_at)))
  .map((window) => ({ usedPercent: Number(window.used_percentage), resetsAt: Math.trunc(Number(window.resets_at)) }));
if (windows.length > 0 && /^[0-9a-f-]+$/i.test(sessionId ?? "")) {
  const marker = join(tmpdir(), `agentctl-usage-${sessionId}.json`);
  const reading = JSON.stringify(windows);
  let sent = null;
  try {
    if (Date.now() - statSync(marker).mtimeMs < 600_000) sent = readFileSync(marker, "utf8");
  } catch {}
  if (reading !== sent && (await reportUsage(windows))) {
    try {
      writeFileSync(marker, reading);
    } catch {}
  }
}
