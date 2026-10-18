import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import http from "node:http";
import https from "node:https";

__REPORTER__
// Pi does not wait for every handler, so reports queue to reach the platform in the order they
// were made, retries included. They are best effort: a failure must never disturb the conversation.
let reports: Promise<void> = Promise.resolve();
function send(event: string, fields: Record<string, string>): Promise<void> {
  reports = reports.then(() => report(event, fields)).catch(() => {});
  return reports;
}

export default function (pi: any): void {
  // Whether a turn start was reported and the run has not settled yet. Only then does settling
  // report a completion, so a run an extension started cannot advance the Session's turns.
  let running = false;

  pi.on("session_start", async (event: any, ctx: any) => {
    const nativeSessionId = ctx.sessionManager.getSessionId();
    if (typeof nativeSessionId !== "string" || nativeSessionId === "") return;
    const fields: Record<string, string> = { source: String(event.reason ?? ""), nativeSessionId };
    const transcriptPath = ctx.sessionManager.getSessionFile();
    if (typeof transcriptPath === "string" && transcriptPath !== "") fields.transcriptPath = transcriptPath;
    const started = send("sessionStart", fields);
    // Pi now waits for input unless a first prompt starts a run, which reports after this.
    const idle = running ? started : send("waitingForInput", { source: "idle" });
    await idle;
  });

  pi.on("before_agent_start", async (event: any) => {
    // Runs that custom extension messages start are not operator turns.
    if (event.messageType !== "user") return;
    running = true;
    await send("turnStarted", { source: "user" });
  });

  // Fires once Pi will not continue by itself: after retries, compaction and queued follow-ups.
  // Escape lands here too, with `aborted` set.
  pi.on("agent_settled", async (event: any) => {
    if (!running) return;
    running = false;
    await send("turnCompleted", { source: event.aborted ? "aborted" : "settled" });
  });

  // Pi asks for no approvals itself; an extension dialog is the only way it blocks on the operator.
  pi.on("ui_prompt_start", async () => {
    await send("waitingForInput", { source: "ui_prompt" });
  });
  pi.on("ui_prompt_end", async () => {
    if (running) await send("turnStarted", { source: "ui_prompt" });
  });
}
