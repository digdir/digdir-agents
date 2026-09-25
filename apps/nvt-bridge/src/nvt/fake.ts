import type { DoneOutcome, NvtDriver, NvtInstance } from "./driver.ts";

/** Én ting som skjedde mot fake-driveren, i rekkefølge. */
export type FakeCall =
  | { kind: "ensure"; topic: string; instance: string }
  | { kind: "ready"; instance: string; topic: string }
  | { kind: "prompt"; instance: string; topic: string; prompt: string }
  | { kind: "wait"; instance: string; topic: string }
  | { kind: "stop"; instance: string; topic: string };

/**
 * Testdouble for nvt. Deterministisk: ingen timere, ingen Docker. Testene
 * styrer hva som skjer per prompt via `onPrompt`, og leser `calls` for å
 * verifisere rekkefølge og serialisering.
 */
export class FakeNvtDriver implements NvtDriver {
  readonly calls: FakeCall[] = [];
  /** Instanser som er «oppe» akkurat nå. */
  readonly live = new Set<string>();
  /** Alle instansnavn som noen gang er opprettet (workspace beholdes). */
  readonly everCreated = new Set<string>();
  /** Hvor mange prompts som er inne i sesjonen samtidig, per instans. */
  readonly concurrentPrompts = new Map<string, number>();
  /** Høyeste samtidighet observert per instans — 1 betyr serielt. */
  readonly maxConcurrentPrompts = new Map<string, number>();

  /**
   * Kalles for hver prompt. Returverdien avgjør hva `waitForDone` svarer.
   * Default: `signal done` med en gang.
   */
  onPrompt: (ctx: {
    instance: NvtInstance;
    prompt: string;
  }) => Promise<DoneOutcome> | DoneOutcome = () => ({
    kind: "done",
    at: "1970-01-01T00:00:00.000Z",
  });

  /**
   * Kalles av `waitUntilReady`. Kast for å simulere en sesjon som aldri blir
   * klar (onboarding-dialogen står, tmux svarer ikke) — broen skal da skrive en
   * `status:"error"`-linje UTEN å ha sendt prompten.
   */
  onReady: (ctx: { instance: NvtInstance }) => Promise<void> | void = () => {};

  private nextOutcome = new Map<string, DoneOutcome>();
  /** Instanser `waitUntilReady` har svart OK for. */
  private readonly ready = new Set<string>();

  async ensureInstance(topic: string, instance: string): Promise<NvtInstance> {
    this.calls.push({ kind: "ensure", topic, instance });
    this.live.add(instance);
    this.everCreated.add(instance);
    return { topic, instance };
  }

  async waitUntilReady(instance: NvtInstance): Promise<void> {
    this.calls.push({
      kind: "ready",
      instance: instance.instance,
      topic: instance.topic,
    });
    await this.onReady({ instance });
    this.ready.add(instance.instance);
  }

  async sendPrompt(instance: NvtInstance, prompt: string): Promise<void> {
    // Kontrakten, håndhevet: en prompt før klar-sjekken ville blitt spist av
    // claude-onboardingen (M0-funn 5). Det skal ikke kunne skje uoppdaget.
    if (!this.ready.has(instance.instance)) {
      throw new Error(
        `kontraktbrudd: sendPrompt til ${instance.instance} uten at waitUntilReady har svart OK`,
      );
    }
    this.calls.push({
      kind: "prompt",
      instance: instance.instance,
      topic: instance.topic,
      prompt,
    });
    const inFlight = (this.concurrentPrompts.get(instance.instance) ?? 0) + 1;
    this.concurrentPrompts.set(instance.instance, inFlight);
    this.maxConcurrentPrompts.set(
      instance.instance,
      Math.max(this.maxConcurrentPrompts.get(instance.instance) ?? 0, inFlight),
    );
    this.nextOutcome.set(
      instance.instance,
      await this.onPrompt({ instance, prompt }),
    );
  }

  async waitForDone(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<DoneOutcome> {
    this.calls.push({
      kind: "wait",
      instance: instance.instance,
      topic: instance.topic,
    });
    const outcome =
      this.nextOutcome.get(instance.instance) ??
      ({ kind: "timeout", waitedMs: opts.timeoutMs } satisfies DoneOutcome);
    this.nextOutcome.delete(instance.instance);
    this.concurrentPrompts.set(
      instance.instance,
      Math.max(0, (this.concurrentPrompts.get(instance.instance) ?? 1) - 1),
    );
    return outcome;
  }

  async stopInstance(instance: NvtInstance): Promise<void> {
    this.calls.push({
      kind: "stop",
      instance: instance.instance,
      topic: instance.topic,
    });
    this.live.delete(instance.instance);
    // Ny sesjon neste gang ⇒ klar-sjekken må gjøres om igjen.
    this.ready.delete(instance.instance);
  }

  /** Prompt-tekstene som er injisert, i rekkefølge. */
  prompts(): string[] {
    return this.calls
      .filter((c): c is Extract<FakeCall, { kind: "prompt" }> => c.kind === "prompt")
      .map((c) => c.prompt);
  }
}
