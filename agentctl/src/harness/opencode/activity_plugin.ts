import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

__REPORTER__
// Reports are best effort: a failure must never disturb the conversation.
async function send(event: string, fields: Record<string, string>): Promise<void> {
  try {
    await report(event, fields);
  } catch {}
}

// The database OpenCode keeps its sessions in, resolved as OpenCode resolves it. A release build
// uses `opencode.db`; other channels get their own file unless that is switched off.
function databasePath(channel: string): string | undefined {
  const data = join(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"), "opencode");
  const shared =
    ["latest", "dev", "beta", "next", "prod"].includes(channel) ||
    ["1", "true"].includes(process.env.OPENCODE_DISABLE_CHANNEL_DB ?? "");
  const name =
    process.env.OPENCODE_DB ?? (shared ? "opencode.db" : `opencode-${channel.replace(/[^a-zA-Z0-9._-]/g, "-")}.db`);
  return name === ":memory:" ? undefined : resolve(data, name);
}

const EXECUTION_ENDED = new Set([
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
]);

export default {
  id: "agent-platform.activity",
  async setup(ctx: any) {
    const transcriptPath = databasePath(String(ctx.app?.channel ?? "latest"));
    // The conversation the platform was last told about; a subagent's sessions are never it.
    let reported: string | undefined;
    const children = new Set<string>();
    // Whether a turn start was reported and its execution has not ended yet. Only then does an
    // end report a completion.
    let running = false;

    async function start(nativeSessionId: string, source: string): Promise<void> {
      reported = nativeSessionId;
      const fields: Record<string, string> = { source, nativeSessionId };
      if (transcriptPath !== undefined) fields.transcriptPath = transcriptPath;
      await send("sessionStart", fields);
    }

    async function observe(type: string, data: any): Promise<void> {
      const sessionID = typeof data?.sessionID === "string" && data.sessionID !== "" ? data.sessionID : undefined;
      if (type === "session.created" && sessionID !== undefined) {
        if (data.parentID) children.add(sessionID);
        else await start(sessionID, "startup");
      } else if (type === "session.execution.started" && sessionID !== undefined && !children.has(sessionID)) {
        // A resumed conversation, or one the operator switched to, was created before this server.
        if (sessionID !== reported) await start(sessionID, "resume");
        running = true;
        await send("turnStarted", { source: "user" });
      } else if (EXECUTION_ENDED.has(type) && sessionID !== undefined && !children.has(sessionID)) {
        if (!running) return;
        running = false;
        await send("turnCompleted", { source: type.slice("session.execution.".length) });
      } else if (type === "permission.asked" || type === "form.created") {
        // A subagent's question blocks the operator too.
        await send("waitingForInput", { source: type.split(".")[0] });
      } else if (type === "permission.replied" || type === "form.replied" || type === "form.cancelled") {
        if (running) await send("turnStarted", { source: type.split(".")[0] });
      }
    }

    // This server is the Session's own (`--standalone`), so it sees only the Session's events.
    const controller = new AbortController();
    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          await observe(String(event?.type ?? ""), event?.data);
        }
      } catch {}
    })();

    const model = typeof ctx.options?.model === "string" ? ctx.options.model : undefined;
    const effort = typeof ctx.options?.effort === "string" ? ctx.options.effort : undefined;
    const separator = model === undefined ? -1 : model.indexOf("/");
    const providerID = model?.slice(0, separator);
    const modelID = model?.slice(separator + 1);

    // The TUI picks a new conversation's model itself. Until it has loaded OpenCode's configuration
    // it takes a recent or the first listed model, and an initial prompt can be submitted in that
    // window. An agent's own model has none, so the selection becomes the model of each agent the
    // operator can pick, unless the Agent's configuration gives it one. Hidden agents, such as the
    // ones that write titles and compactions, keep OpenCode's choice.
    if (model !== undefined) {
      await ctx.agent.transform((editor: any) => {
        for (const agent of editor.list()) {
          if (agent.mode === "subagent" || agent.hidden === true || agent.model !== undefined) continue;
          editor.update(agent.id, (draft: any) => {
            draft.model = { providerID, id: modelID, ...(effort === undefined ? {} : { variant: effort }) };
          });
        }
      });
    }

    // OpenCode silently replaces a model its configuration lacks with another one, possibly a hosted
    // model elsewhere, and drops an unknown variant. Fail the request instead; the TUI shows why.
    // A selection the operator changes in the TUI is theirs, and is never second-guessed.
    let verified = model === undefined;
    await ctx.session.hook("context", async () => {
      if (verified || model === undefined) return;
      // Only a request sees the configured models; during setup the list is not settled yet.
      const listed = await ctx.model.list();
      const models: any[] = Array.isArray(listed?.data) ? listed.data : [];
      const found = models.find((candidate) => candidate?.providerID === providerID && candidate?.id === modelID);
      if (found === undefined) {
        throw new Error(`agentctl selected model ${model}, which this Agent's OpenCode configuration does not define`);
      }
      // `default` is OpenCode's name for the model without a variant.
      const variants: string[] = (found.variants ?? []).map((variant: any) => String(variant?.id));
      if (effort !== undefined && effort !== "default" && !variants.includes(effort)) {
        const defined = variants.length === 0 ? "no variants" : `only the variants ${variants.join(", ")}`;
        throw new Error(`agentctl selected effort ${effort} for model ${model}, which defines ${defined}`);
      }
      verified = true;
    });

    return () => controller.abort();
  },
};
