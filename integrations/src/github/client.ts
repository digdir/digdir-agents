import type { GithubConfig } from "../config.ts";
import { parseCodeowners, type CodeownersRule } from "./codeowners.ts";

/** Minimal shape of a GitHub notification we care about. */
export interface GithubNotification {
  id: string;
  reason: string;
  updated_at: string;
  subject: {
    title: string;
    url: string | null;
    latest_comment_url: string | null;
    type: string;
  };
  repository: {
    full_name: string;
    name: string;
    owner: { login: string };
  };
}

export interface NotificationsResult {
  notifications: GithubNotification[];
  /** Server-suggested minimum seconds until the next poll. */
  pollIntervalSeconds: number | null;
  /** Value to send back as If-Modified-Since on the next poll. */
  lastModified: string | null;
  /** True when the server returned 304 Not Modified (no new notifications). */
  notModified: boolean;
}

/** An open PR's fields relevant to the review-notify watcher (issue #115). */
export interface PullRequestCandidate {
  owner: string;
  repo: string;
  number: number;
  title: string;
  url: string;
  author: string;
  isDraft: boolean;
  reviewDecision: string | null;
  labels: string[];
}

export class GithubClient {
  /** Fine-grained token for issues/PRs/reactions/user. */
  private readonly actionToken: string;
  /** Token for the notifications API (classic PAT with `notifications` scope). */
  private readonly notificationsToken: string;
  private readonly baseUrl: string;
  private readonly graphqlUrl: string;

  constructor(config: GithubConfig) {
    this.actionToken = config.token;
    this.notificationsToken = config.notificationsToken;
    this.baseUrl = config.apiBaseUrl;
    // github.com: https://api.github.com -> https://api.github.com/graphql.
    // GHE: https://HOST/api/v3 -> https://HOST/api/graphql (no /v3).
    this.graphqlUrl = this.baseUrl.endsWith("/api/v3")
      ? `${this.baseUrl.slice(0, -"/v3".length)}/graphql`
      : `${this.baseUrl}/graphql`;
  }

  private headers(token: string, extra?: Record<string, string>): Record<string, string> {
    return {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "integrations",
      ...extra,
    };
  }

  /** Returns the login of the account the token belongs to. */
  async getAuthenticatedLogin(): Promise<string> {
    const res = await fetch(`${this.baseUrl}/user`, { headers: this.headers(this.actionToken) });
    if (!res.ok) {
      throw new Error(`GET /user failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { login: string };
    return body.login;
  }

  async listNotifications(lastModified: string | null): Promise<NotificationsResult> {
    const res = await fetch(`${this.baseUrl}/notifications?all=false`, {
      headers: this.headers(
        this.notificationsToken,
        lastModified ? { "If-Modified-Since": lastModified } : undefined,
      ),
    });

    const pollHeader = res.headers.get("x-poll-interval");
    const pollIntervalSeconds = pollHeader ? Number(pollHeader) : null;

    if (res.status === 304) {
      return { notifications: [], pollIntervalSeconds, lastModified, notModified: true };
    }
    if (!res.ok) {
      let hint = "";
      if (res.status === 403) {
        hint =
          " — the notifications token lacks access. The notifications API needs a" +
          " classic PAT with the `notifications` scope (fine-grained PATs do not" +
          " work here); set GITHUB_TOKEN_CLASSIC_NOTIFICATIONS.";
      }
      throw new Error(`GET /notifications failed: ${res.status} ${await res.text()}${hint}`);
    }

    const notifications = (await res.json()) as GithubNotification[];
    return {
      notifications,
      pollIntervalSeconds,
      lastModified: res.headers.get("last-modified") ?? lastModified,
      notModified: false,
    };
  }

  /** Returns the logins currently assigned to an issue or PR (via its API URL). */
  async listAssignees(subjectUrl: string): Promise<string[]> {
    const res = await fetch(subjectUrl, { headers: this.headers(this.actionToken) });
    if (!res.ok) {
      throw new Error(`GET ${subjectUrl} failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { assignees?: Array<{ login: string }> };
    return (body.assignees ?? []).map((a) => a.login);
  }

  /**
   * Removes the given assignee from an issue or pull request. Only the listed
   * login is removed; any other assignees are left in place.
   */
  async removeAssignee(owner: string, repo: string, issueNumber: number, login: string): Promise<void> {
    const url = `${this.baseUrl}/repos/${owner}/${repo}/issues/${issueNumber}/assignees`;
    const res = await fetch(url, {
      method: "DELETE",
      headers: this.headers(this.actionToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ assignees: [login] }),
    });
    if (!res.ok) {
      const hint =
        res.status === 403
          ? " — GITHUB_TOKEN needs write access to Issues (and Pull requests, for PRs)."
          : "";
      throw new Error(`DELETE assignees failed: ${res.status} ${await res.text()}${hint}`);
    }
  }

  /**
   * Adds a reaction to a reactions endpoint URL and returns its id (so it can
   * later be removed). Idempotent server-side: a repeated identical reaction
   * returns 200 with the existing reaction instead of 201.
   */
  async addReaction(reactionsUrl: string, content: string): Promise<number | null> {
    const res = await fetch(reactionsUrl, {
      method: "POST",
      headers: this.headers(this.actionToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ content }),
    });
    if (res.status !== 200 && res.status !== 201) {
      throw new Error(`POST reaction failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { id?: number };
    return body.id ?? null;
  }

  /** Removes a previously added reaction (by id) from a reactions endpoint. */
  async removeReaction(reactionsUrl: string, reactionId: number): Promise<void> {
    const res = await fetch(`${reactionsUrl}/${reactionId}`, {
      method: "DELETE",
      headers: this.headers(this.actionToken),
    });
    // 204 No Content on success; 404 if already gone — both are fine.
    if (!res.ok && res.status !== 404) {
      throw new Error(`DELETE reaction failed: ${res.status} ${await res.text()}`);
    }
  }

  /** Returns the author login of a GitHub resource (issue, PR, or comment) given its API URL. */
  async getResourceAuthor(url: string): Promise<string | null> {
    const res = await fetch(url, { headers: this.headers(this.actionToken) });
    if (!res.ok) {
      throw new Error(`GET ${url} failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { user?: { login: string } };
    return body.user?.login ?? null;
  }

  /**
   * Returns the actor who performed the most recent assignment to the given bot
   * login. Events come oldest-first; per_page=100 keeps the latest assign within
   * the first page for all but extremely busy issues (where the lookup then
   * returns null and the event is treated as human-triggered — fail-open).
   */
  async getLastAssigner(owner: string, repo: string, issueNumber: number, botLogin: string): Promise<string | null> {
    const url = `${this.baseUrl}/repos/${owner}/${repo}/issues/${issueNumber}/events?per_page=100`;
    const res = await fetch(url, { headers: this.headers(this.actionToken) });
    if (!res.ok) {
      throw new Error(`GET ${url} failed: ${res.status} ${await res.text()}`);
    }
    const events = (await res.json()) as Array<{
      event: string;
      assignee?: { login: string };
      actor: { login: string };
    }>;

    const lastAssignEvent = events.findLast(
      (e) => e.event === "assigned" && e.assignee?.login === botLogin,
    );
    return lastAssignEvent?.actor.login ?? null;
  }

  /** Fetches the body text at an issue/PR or comment API URL (for prompts). */
  async getBody(url: string): Promise<string> {
    const res = await fetch(url, { headers: this.headers(this.actionToken) });
    if (!res.ok) {
      throw new Error(`GET ${url} failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { body?: string | null };
    return (body.body ?? "").trim();
  }

  /** Posts a comment on an issue or pull request. Requires Issues write. */
  async addIssueComment(owner: string, repo: string, issueNumber: number, body: string): Promise<void> {
    const url = `${this.baseUrl}/repos/${owner}/${repo}/issues/${issueNumber}/comments`;
    const res = await fetch(url, {
      method: "POST",
      headers: this.headers(this.actionToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ body }),
    });
    if (res.status !== 201) {
      const hint =
        res.status === 403 ? " — GITHUB_TOKEN needs write access to Issues." : "";
      throw new Error(`POST comment failed: ${res.status} ${await res.text()}${hint}`);
    }
  }

  /** Marks a notification thread as read so it stops reappearing in polls. */
  async markThreadRead(threadId: string): Promise<void> {
    const res = await fetch(`${this.baseUrl}/notifications/threads/${threadId}`, {
      method: "PATCH",
      headers: this.headers(this.notificationsToken),
    });
    // 205 Reset Content on success; 404 if already gone — both are fine.
    if (!res.ok && res.status !== 205 && res.status !== 404) {
      throw new Error(`PATCH thread failed: ${res.status} ${await res.text()}`);
    }
  }

  private async graphql<T>(query: string, variables: Record<string, unknown>): Promise<T> {
    const res = await fetch(this.graphqlUrl, {
      method: "POST",
      headers: this.headers(this.actionToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ query, variables }),
    });
    if (!res.ok) {
      throw new Error(`POST /graphql failed: ${res.status} ${await res.text()}`);
    }
    const body = (await res.json()) as { data?: T; errors?: Array<{ message: string }> };
    if (body.errors && body.errors.length > 0) {
      throw new Error(`GraphQL error: ${body.errors.map((e) => e.message).join("; ")}`);
    }
    if (!body.data) {
      throw new Error("GraphQL response missing data");
    }
    return body.data;
  }

  /**
   * Lists open pull requests in a repo with the fields the review notifier
   * (issue #115) needs to decide whether a PR is awaiting human review:
   * draft status, review decision, author and labels.
   */
  async listOpenPullRequestCandidates(owner: string, repo: string): Promise<PullRequestCandidate[]> {
    const query = `
      query($owner: String!, $repo: String!, $cursor: String) {
        repository(owner: $owner, name: $repo) {
          pullRequests(states: OPEN, first: 50, after: $cursor, orderBy: {field: UPDATED_AT, direction: DESC}) {
            nodes {
              number
              title
              url
              isDraft
              reviewDecision
              author { login }
              labels(first: 20) { nodes { name } }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
      }`;

    interface Node {
      number: number;
      title: string;
      url: string;
      isDraft: boolean;
      reviewDecision: string | null;
      author: { login: string } | null;
      labels: { nodes: Array<{ name: string }> };
    }
    interface Data {
      repository: {
        pullRequests: { nodes: Node[]; pageInfo: { hasNextPage: boolean; endCursor: string | null } };
      } | null;
    }

    type Connection = NonNullable<Data["repository"]>["pullRequests"];

    const results: PullRequestCandidate[] = [];
    let cursor: string | null = null;
    do {
      const data: Data = await this.graphql<Data>(query, { owner, repo, cursor });
      const conn: Connection | undefined = data.repository?.pullRequests;
      if (!conn) break;
      for (const n of conn.nodes) {
        results.push({
          owner,
          repo,
          number: n.number,
          title: n.title,
          url: n.url,
          author: n.author?.login ?? "unknown",
          isDraft: n.isDraft,
          reviewDecision: n.reviewDecision,
          labels: n.labels.nodes.map((l) => l.name),
        });
      }
      cursor = conn.pageInfo.hasNextPage ? conn.pageInfo.endCursor : null;
    } while (cursor);
    return results;
  }

  /** Lists the file paths changed by a pull request (paginated, 100/page). */
  async listPullRequestFiles(owner: string, repo: string, number: number): Promise<string[]> {
    const files: string[] = [];
    let page = 1;
    for (;;) {
      const url = `${this.baseUrl}/repos/${owner}/${repo}/pulls/${number}/files?per_page=100&page=${page}`;
      const res = await fetch(url, { headers: this.headers(this.actionToken) });
      if (!res.ok) {
        throw new Error(`GET ${url} failed: ${res.status} ${await res.text()}`);
      }
      const body = (await res.json()) as Array<{ filename: string }>;
      files.push(...body.map((f) => f.filename));
      if (body.length < 100) break;
      page += 1;
    }
    return files;
  }

  /**
   * Fetches and parses a repo's CODEOWNERS file (checked at the locations
   * GitHub itself recognizes). Returns null if none of them exist.
   */
  async getCodeowners(owner: string, repo: string): Promise<CodeownersRule[] | null> {
    for (const path of [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"]) {
      const url = `${this.baseUrl}/repos/${owner}/${repo}/contents/${path}`;
      const res = await fetch(url, { headers: this.headers(this.actionToken) });
      if (res.status === 404) continue;
      if (!res.ok) {
        throw new Error(`GET ${url} failed: ${res.status} ${await res.text()}`);
      }
      const body = (await res.json()) as { content?: string; encoding?: string };
      if (body.content && body.encoding === "base64") {
        return parseCodeowners(Buffer.from(body.content, "base64").toString("utf-8"));
      }
    }
    return null;
  }
}

/**
 * Resolves the reactions endpoint for a notification. Prefers the specific
 * comment that triggered the notification; falls back to the issue/PR itself.
 * Returns null when no usable target can be derived.
 */
export function reactionsUrlFor(notification: GithubNotification): string | null {
  const { latest_comment_url, url } = notification.subject;

  // A comment URL looks like .../issues/comments/{id} or .../pulls/comments/{id}.
  if (latest_comment_url && latest_comment_url.includes("/comments/")) {
    return `${latest_comment_url}/reactions`;
  }

  // Otherwise react on the issue itself. Pull requests expose reactions through
  // their issues counterpart, so normalise /pulls/ -> /issues/.
  if (url) {
    const issueUrl = url.replace("/pulls/", "/issues/");
    return `${issueUrl}/reactions`;
  }

  return null;
}

/** Extracts the numeric issue/PR number from a subject URL. */
export function issueNumberFrom(subjectUrl: string | null): number | null {
  if (!subjectUrl) return null;
  const match = subjectUrl.match(/\/(?:issues|pulls)\/(\d+)$/);
  return match ? Number(match[1]) : null;
}

/**
 * A stable, filename-safe id for a notification event. Prefers the triggering
 * comment id (so each new comment is its own event); falls back to the thread's
 * last-updated timestamp. proxy-agent uses this as the log filename and dedupe key.
 */
export function eventIdFor(notification: GithubNotification): string {
  const { full_name } = notification.repository;
  const num = issueNumberFrom(notification.subject.url) ?? "x";
  const commentMatch = notification.subject.latest_comment_url?.match(/\/comments\/(\d+)$/);
  const suffix = commentMatch ? `c${commentMatch[1]}` : notification.updated_at;
  return `github-${full_name}-${num}-${suffix}`.replace(/[^A-Za-z0-9._-]/g, "-");
}
