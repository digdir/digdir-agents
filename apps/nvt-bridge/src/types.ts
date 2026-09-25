/**
 * Filkontrakten mot integrations. Speiler `integrations/src/agent/types.ts`
 * med vilje — bridgen skal se ut som en helt vanlig agent utenfra, så disse
 * to må holdes i takt. Bridgen leser `QueueEvent` og skriver `ResultLine`.
 */

/** Én linje i `triggers/inbox.jsonl`. */
export interface QueueEvent {
  id: string;
  source: string;
  type: string;
  received_at: string;
  prompt: string;
  payload?: Record<string, unknown>;
  classification?: string;
  related_activities?: unknown[];
}

/**
 * Én linje i `triggers/results.jsonl`. Valgfrie felter utelates (aldri
 * `null`) — samme mønster som agent-entrypointene bygger med jq.
 */
export interface ResultLine {
  id: string;
  status: "ok" | "error";
  exit_code?: number;
  log?: string;
  intent?: string;
  reply?: string;
  delegate?: {
    agent?: string;
    prompt?: string;
    payload?: Record<string, unknown>;
  };
  extraction_failed?: boolean;
  started_at?: string;
  finished_at?: string;
}

/**
 * Bridgens syn på ett topic. Persisteres i `state/topics.json` slik at en
 * omstart av bridgen gjenbruker den levende instansen i stedet for å lage
 * en ny (og dermed et nytt workspace og en ny samtale).
 */
export interface TopicRecord {
  /** nvt-instansnavn (compose-prosjekt `agent-<instance>`). */
  instance: string;
  created_at: string;
  /** Sist gang en prompt ble injisert — grunnlaget for TTL-nedtaking. */
  last_prompt_at?: string;
  /** Siste event-id bridgen dispatchet for dette topicet. */
  last_event_id?: string;
  /**
   * Event-id som er injisert men ennå ikke kvittert ut med en resultatlinje.
   * Overlever en omstart av bridgen, slik at den ikke sender samme prompt inn
   * i en levende sesjon to ganger.
   */
  in_flight_event_id?: string;
  /** Antall prompts injisert i denne sesjonen. */
  prompts: number;
  /** Om bridgen tror instansen er oppe. `agent-down` setter denne til false. */
  up: boolean;
}

export interface TopicsState {
  version: 1;
  topics: Record<string, TopicRecord>;
}
