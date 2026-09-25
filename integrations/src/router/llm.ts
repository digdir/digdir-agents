/**
 * Minimal OpenAI-compatible HTTP client for the first-line router: one chat
 * call with structured output and one embeddings call. Deliberately no SDK —
 * the router is a small, optional hot path, and every failure here is the
 * caller's cue to fall back to an unannotated event.
 */
export class OpenAiClient {
  private readonly baseUrl: string;
  private readonly apiKey: string;
  private readonly timeoutMs: number;

  constructor(baseUrl: string, apiKey: string, timeoutMs: number) {
    this.baseUrl = baseUrl;
    this.apiKey = apiKey;
    this.timeoutMs = timeoutMs;
  }

  private async post(endpoint: string, body: unknown): Promise<unknown> {
    const res = await fetch(`${this.baseUrl}${endpoint}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        ...(this.apiKey ? { authorization: `Bearer ${this.apiKey}` } : {}),
      },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    if (!res.ok) {
      const detail = (await res.text().catch(() => "")).slice(0, 300);
      throw new Error(`${endpoint} -> HTTP ${res.status}${detail ? `: ${detail}` : ""}`);
    }
    return res.json();
  }

  /**
   * One chat completion constrained to a JSON schema (structured output).
   * Returns the parsed message content; the caller still validates the shape —
   * model output is data, never trusted.
   */
  async chatJson(
    model: string,
    system: string,
    user: string,
    schemaName: string,
    schema: Record<string, unknown>,
  ): Promise<unknown> {
    const data = (await this.post("/chat/completions", {
      model,
      temperature: 0,
      messages: [
        { role: "system", content: system },
        { role: "user", content: user },
      ],
      response_format: {
        type: "json_schema",
        json_schema: { name: schemaName, strict: true, schema },
      },
    })) as { choices?: Array<{ message?: { content?: string } }> };
    const content = data.choices?.[0]?.message?.content;
    if (!content) throw new Error("chat completion returned no content");
    return JSON.parse(content);
  }

  /** Embeds one text; throws unless the response carries a numeric vector. */
  async embed(model: string, text: string): Promise<number[]> {
    const data = (await this.post("/embeddings", { model, input: text })) as {
      data?: Array<{ embedding?: unknown }>;
    };
    const embedding = data.data?.[0]?.embedding;
    if (!Array.isArray(embedding) || embedding.length === 0 || !embedding.every((n) => typeof n === "number")) {
      throw new Error("embeddings response has no valid vector");
    }
    return embedding as number[];
  }
}
