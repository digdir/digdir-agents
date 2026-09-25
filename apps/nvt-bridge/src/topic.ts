import { createHash } from "node:crypto";
import type { QueueEvent } from "./types.ts";

/** Samme tegnsett som integrations sanerer event-id-er til. */
const UNSAFE = /[^a-zA-Z0-9._-]/g;

/**
 * Delta-suffikset integrations legger på delegerte events
 * (`queue.ts`: `${result.id}-d${hops}`). `-d1`, `-d2`, ...
 */
const DELTA_SUFFIX = /-d\d+$/;

/**
 * Avleder topicet et event tilhører: opphavstråden (Slack-tråden eller
 * GitHub-issuet), ikke det enkelte eventet.
 *
 * Regelen er identisk med `Get-TopicKey` i `scripts/agent-runner.ps1`, som er
 * dagens referanseimplementasjon: `payload.origin.event_id` når den finnes
 * (delegerte events bærer opphavs-id-en der), ellers eventets egen id — så
 * ett anker-suffiks `-dN` av, så sanering.
 *
 * Merk at strippingen er ankret og skjer én gang: `x-d1-d2` blir `x-d1`, ikke
 * `x`. Det er tilsiktet likhet med ps1-runneren — `payload.origin.event_id`
 * er den opprinnelige id-en, så for delegerte events er strippingen normalt
 * et no-op, og den betyr bare noe når vi faller tilbake på `evt.id`.
 * `-outcome`-suffikset (debrief-events) strippes ikke.
 */
export function topicFor(event: QueueEvent): string {
  const origin = (event.payload?.origin ?? {}) as { event_id?: unknown };
  const raw =
    typeof origin.event_id === "string" && origin.event_id !== ""
      ? origin.event_id
      : event.id;
  return raw.replace(DELTA_SUFFIX, "").replace(UNSAFE, "_");
}

/** Sanerer en event-id til samme tegnsett integrations bruker. */
export function safeId(id: string): string {
  return id.replace(UNSAFE, "_");
}

export interface InstanceNameOptions {
  prefix: string;
  maxLength: number;
}

/**
 * Avleder nvt-instansnavnet for et topic.
 *
 * Navnet må være kort og DNS-vennlig: nvt bygger både compose-prosjektnavnet
 * (`agent-<navn>`) og code-server-vertsnavnet
 * (`http://<navn>.agent.localhost:4090`) av det, og en DNS-etikett tar maks 63
 * tegn. Topic-id-ene fra GitHub er lange nok til å bli et problem alene
 * (`github-digdir-digdir-ai-agents-97-c5114846564` er 45 tegn før prefiks og
 * compose-prefiks).
 *
 * Derfor: kortet slug + kort hash av det FULLE topicet. Hashen gjør at to
 * topics som kortes til samme slug likevel får ulike instanser, og at navnet
 * er **deterministisk** — mister vi `state/topics.json`, peker samme topic
 * fortsatt på samme instans og dermed samme workspace.
 */
export function instanceNameFor(
  topic: string,
  opts: InstanceNameOptions,
): string {
  const hash = createHash("sha256").update(topic).digest("hex").slice(0, 8);
  const prefix = slugify(opts.prefix);
  // Fast del: <prefix>- ... -<hash8>
  const fixed = (prefix === "" ? 0 : prefix.length + 1) + 1 + hash.length;
  const room = Math.max(0, opts.maxLength - fixed);
  const slug = slugify(topic).slice(0, room).replace(/-+$/, "");
  return [prefix, slug, hash].filter((p) => p !== "").join("-");
}

/** Små bokstaver, kun a-z0-9 og enkle bindestreker — trygt som DNS-etikett. */
function slugify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}
