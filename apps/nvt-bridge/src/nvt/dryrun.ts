import type { DoneOutcome, NvtDriver, NvtInstance } from "./driver.ts";

/**
 * Tørrkjøring: logger hva som VILLE blitt kjørt mot nvt, uten å røre Docker.
 * Nyttig for å verifisere ruting, topic-avledning og at filkontrakten henger
 * sammen før M0-oppsettet står.
 *
 * `waitForDone` svarer alltid `timeout` — da skriver broen en
 * `status:"error"`-linje som forklarer at dette var en tørrkjøring. Aldri en
 * fabrikert suksess, heller ikke her.
 */
export class DryRunNvtDriver implements NvtDriver {
  private readonly log: (message: string) => void;

  constructor(log: (message: string) => void) {
    this.log = log;
  }

  async ensureInstance(topic: string, instance: string): Promise<NvtInstance> {
    this.log(
      `[dry-run] bash scripts/agent-init.sh --name ${instance} --user non-root ` +
        `&& make agent-up NAME=${instance}`,
    );
    return { topic, instance };
  }

  async waitUntilReady(instance: NvtInstance): Promise<void> {
    this.log(
      `[dry-run] klar-sjekk for ${instance.instance}: session-launched-markør + tmux-panel ` +
        `(Enter gjennom onboardingen ved behov)`,
    );
  }

  async sendPrompt(instance: NvtInstance, prompt: string): Promise<void> {
    this.log(
      `[dry-run] docker exec agent-${instance.instance}-agent-1 agentdctl prompt ` +
        `--source host --external <${prompt.length} tegn>`,
    );
  }

  async waitForDone(): Promise<DoneOutcome> {
    return { kind: "timeout", waitedMs: 0 };
  }

  async stopInstance(instance: NvtInstance): Promise<void> {
    this.log(`[dry-run] make agent-down NAME=${instance.instance}`);
  }
}
