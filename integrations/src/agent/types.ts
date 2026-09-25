/**
 * Shared types for the handoff to the proxy-agent queue. integrations is the
 * "receiver" that proxy-agent's README describes: it translates Slack/GitHub
 * events into one JSON line in `triggers/inbox.jsonl`, and posts proxy-agent's
 * results (`triggers/results.jsonl` + `triggers/logs/<id>.log`) back to the
 * originating thread/issue.
 */

/** One line appended to `triggers/inbox.jsonl`. Matches proxy-agent's event format. */
export interface QueueEvent {
  id: string;
  /** "agent" = a delegated task handed off from another agent. */
  source: "slack" | "github" | "agent";
  type: string;
  received_at: string;
  prompt: string;
  payload: Record<string, unknown>;
  /**
   * First-line router annotations (optional — absent when the router is off
   * or failed). The router only annotates; it never drops or reroutes events.
   */
  classification?: "action" | "feedback" | "ack" | "delegate";
  related_activities?: RelatedActivity[];
}

/**
 * An open activity (Slack thread / GitHub issue) the router found similar to
 * the event. References only — never the activity's text.
 */
export interface RelatedActivity {
  kind: "slack-thread" | "github-issue";
  key: string;
  ref: Record<string, unknown>;
  /** Cosine similarity [0..1] against the activity's title/first message. */
  score: number;
}

/**
 * Where a result for a given event id should be posted back to. Also carries
 * the "working" reaction to clear once the answer is posted (if any). These
 * fields are persisted in the pending map, so cleanup survives a restart.
 */
export type ReplyContext = (
  | {
      kind: "slack";
      channel: string;
      threadTs: string;
      /** ts of the message the working reaction sits on. */
      messageTs?: string;
      /** Working reaction to remove once answered. */
      workingReaction?: string;
    }
  | {
      kind: "github";
      owner: string;
      repo: string;
      issueNumber: number;
      /** Reactions endpoint + id of the working reaction to remove once answered. */
      reactionsUrl?: string;
      reactionId?: number;
    }
) & {
  /** Delegation hops consumed for this originating event (loop guard). */
  hops?: number;
  /**
   * Set when the pending result comes from a delegated task: who delegated it
   * and under which event id. Used to send the delegator a `delegation-outcome`
   * debrief event once the answer has been delivered.
   */
  origin?: { agent: string; eventId: string };
};

/**
 * One line read from `triggers/results.jsonl`, written by proxy-agent. `intent`
 * and `reply` are produced by the agent's classification step; older results
 * without them fall back to posting the raw log.
 */
export interface ResultLine {
  id: string;
  status: string;
  exit_code?: number;
  log?: string;
  /** How the agent classified the input. */
  intent?: "action" | "feedback" | "ack" | "delegate" | string;
  /** Clean answer text to post back (as opposed to the raw log). */
  reply?: string;
  /** With `intent: "delegate"`: the task to hand off to another agent. */
  delegate?: {
    /** Target agent name — must match a configured route. */
    agent?: string;
    /** Complete, self-contained task description for the target agent. */
    prompt?: string;
    /** Optional extra context (e.g. an issue URL) passed into the event. */
    payload?: Record<string, unknown>;
  };
  /**
   * Set by proxy-agent when RESULT_MARKER was found in the log but the JSON
   * block could not be parsed (e.g. wrapped in a markdown fence). Used by the
   * queue to avoid posting the raw internal log as a public comment on GitHub.
   */
  extraction_failed?: boolean;
  started_at?: string;
  finished_at?: string;
}

/**
 * How a result should be delivered to its origin:
 *   - "message": post the text (a threaded reply / issue comment).
 *   - "ack": post nothing, just add an acknowledgement reaction to the origin.
 */
export type Delivery =
  | { kind: "message"; text: string }
  | { kind: "ack" };

/**
 * Posters the result watcher calls to deliver a result. A connector that is
 * disabled simply has no poster registered, and the watcher logs and skips.
 */
export interface ResultPosters {
  slack?: (reply: Extract<ReplyContext, { kind: "slack" }>, delivery: Delivery) => Promise<void>;
  github?: (reply: Extract<ReplyContext, { kind: "github" }>, delivery: Delivery) => Promise<void>;
}
