/**
 * Alt bridgen gjør mot nvt-agent går gjennom dette interfacet. Tre grunner:
 *
 * 1. Kjernelogikken (dedupe, topic-avledning, serialisering, fallback-linja)
 *    kan testes uten Docker, mot `FakeNvtDriver`.
 * 2. Den ekte implementasjonen er en tynn adapter over `make agent-init` /
 *    `make agent-up` / `docker exec … agentdctl`, og skal **kalibreres mot
 *    M0-funnene** (issue #96) — den er det eneste stedet antakelser om
 *    compose-stier, containernavn og nettverk bor.
 * 3. Når k8s-sporet kommer (jf. planen), er det en ny implementasjon av
 *    dette interfacet, ikke en endring i kjernen.
 */

/** En levende (eller nettopp opprettet) nvt-instans. */
export interface NvtInstance {
  /** Topicet instansen er dedikert til. */
  topic: string;
  /** nvt-instansnavnet (`make agent-init NAME=<instance>`). */
  instance: string;
}

/** Utfallet av å vente på at agenten melder seg ferdig. */
export type DoneOutcome =
  | { kind: "done"; at: string }
  | { kind: "timeout"; waitedMs: number };

export interface NvtDriver {
  /**
   * Sørger for at topicet har en levende instans: `agent-init` + `agent-up`
   * hvis den ikke finnes, ellers gjenbruk av den levende sesjonen.
   * Må være idempotent — bridgen kaller den før hver prompt.
   */
  ensureInstance(topic: string, instance: string): Promise<NvtInstance>;

  /**
   * Venter til CLI-sesjonen faktisk kan ta imot en prompt, og kaster ved
   * timeout. **Må kalles før hver `sendPrompt`** — det er en del av kontrakten,
   * ikke en optimalisering.
   *
   * Grunnen er M0-funn 5: claude-onboardingen (velkomstskjerm + trust-dialog)
   * spiser prompts som injiseres for tidlig. `agentd` venter på at
   * `session-launched`-markøren og tmux-sesjonen finnes, men *ikke* på at
   * claude er kommet gjennom dialogene — da havner prompten i en dialog i
   * stedet for i sesjonen, og oppgaven forsvinner uten spor.
   *
   * Kaster ved timeout (aldri «antatt klar»): broen skriver da en
   * `status:"error"`-linje, og prompten sendes ikke.
   */
  waitUntilReady(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<void>;

  /**
   * Injiserer en prompt i instansens levende CLI-sesjon:
   * `agentdctl prompt --source host --external`. `--external` gir
   * «untrusted input»-preamblen — delegerte prompts er upålitelig input.
   *
   * Forutsetter at `waitUntilReady` har svart OK for instansen.
   */
  sendPrompt(instance: NvtInstance, prompt: string): Promise<void>;

  /**
   * Venter på `plugin.agent.signal.done` fra instansens `events.jsonl`.
   * Returnerer `timeout` uten å kaste når fristen går ut — bridgen skal da
   * skrive en `status:"error"`-linje, ikke anta suksess.
   */
  waitForDone(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<DoneOutcome>;

  /**
   * `agent-down` — tar ned en inaktiv instans. Workspacet beholdes, så
   * instansen kan gjenskapes med samme navn og samme arbeidskopi.
   */
  stopInstance(instance: NvtInstance): Promise<void>;
}
