/**
 * `.agents/<navn>/agent.yaml` — instanskonfigurasjonen nvt leser.
 *
 * Hvorfor broen genererer den i stedet for å la `agent-init` gjøre det:
 * nvt-malen kommer med `plugins: []` og et *kommentert* eksempel som bruker
 * `identity.mode: provider`. Det er nøyaktig det som ikke virker for oss
 * (M0-funn 4): `static_token`/broker-token-providere kan ikke rapportere
 * commit-identitet, så identiteten må stå eksplisitt i configen. Uten den
 * feiler `git commit` inne i instansen med «Author identity unknown» — etter
 * at agenten har gjort hele jobben.
 *
 * `agent-init.sh` skriver bare malen når fila ikke finnes fra før (den
 * synkroniserer `runtime.user` og sine egne markør-eide blokker på nytt-kjør).
 * Derfor legger broen fila på plass *før* init, og rører aldri en config som
 * allerede finnes — et menneske kan ha justert den i en levende sesjon.
 *
 * Bot-navn og e-post kommer fra env, aldri fra repoet (jf. planen:
 * «botnavnet holdes utenfor repoet»).
 */

export interface AgentConfigVars {
  /** `runtime.command` — `claude` eller `codex`. */
  agentType: string;
  /** Argumentene nvt gir CLI-en. Tom liste for `interactive`. */
  runtimeArgs: string[];
  /** `root` | `non-root`. Skal være `non-root` (M0-funn 1). */
  userMode: string;
  /** Providernavnet inne i instansen (fritt valgt). */
  gitProvider: string;
  /** Navnet på providerens grant i brokeren (`.broker/agents.yaml`). */
  brokerProvider: string;
  /** Glob mot normalisert repo-target, f.eks. `github.com/digdir/*`. */
  targetMatch: string;
  /** URL-prefiks for credential-regelen, f.eks. `https://github.com/digdir/`. */
  urlMatch: string;
  /** Commit-identitet — bot-kontoen. Fra env, ikke fra repoet. */
  identityName: string;
  identityEmail: string;
}

/**
 * Rendrer configen. Alle verdier skrives som YAML-strenger via `JSON.stringify`
 * (gyldig YAML double-quoted scalar), så en env-verdi med kolon, `#` eller
 * linjeskift kan ikke bryte ut av sin egen node.
 */
export function renderAgentConfig(vars: AgentConfigVars): string {
  requireNonEmpty(vars, [
    "agentType",
    "userMode",
    "gitProvider",
    "brokerProvider",
    "targetMatch",
    "urlMatch",
    "identityName",
    "identityEmail",
  ]);

  const args =
    vars.runtimeArgs.length === 0
      ? " []"
      : `\n${vars.runtimeArgs.map((a) => `    - ${q(a)}`).join("\n")}`;

  return `# Generert av nvt-bridge (apps/nvt-bridge/src/nvt/agentConfig.ts).
# Kalibrert mot M0-funnene i doc/plans/nvt-agent-integrasjon.md.
#
# Broen skriver denne fila EN gang, før 'agent-init'. Rediger den fritt
# etterpå — broen overskriver aldri en eksisterende config. Ett krav
# håndheves ved hver oppstart: identity.mode må være 'explicit'.
runtime:
  command: ${q(vars.agentType)}
  args:${args}
  # M0-funn 1: claude nekter --dangerously-skip-permissions som root, og
  # tmux-sesjonen dør innen 5 s. Agenten MÅ kjøre som 1000:1000.
  user: ${q(vars.userMode)}

tools:
  packages: []
  mise: []
  additional-paths: []
  shell: []

code-server:
  extensions: []
  settings:
    overwrite: false
    values: {}

expose:
  http: []

plugins:
  # Tokenet bor i brokeren (static_token-provider), ikke i instansens env.
  - name: git-host-credentials
    source: builtin
    config:
      default-provider: ${q(vars.gitProvider)}
      providers:
        - name: ${q(vars.gitProvider)}
          type: broker
          broker-provider: ${q(vars.brokerProvider)}
          match:
            - ${q(vars.targetMatch)}

  - name: git-credentials
    source: builtin
    when: before-agent
    config:
      credentials:
        - match: ${q(vars.urlMatch)}
          provider: ${q(vars.gitProvider)}
          # M0-funn 4: 'mode: provider' virker BARE for providere som kan
          # rapportere identitet (GitHub App). Med broker-token må navn og
          # e-post stå her, ellers har agenten ingen commit-identitet.
          identity:
            mode: explicit
            name: ${q(vars.identityName)}
            email: ${q(vars.identityEmail)}
`;
}

export type IdentityVerdict =
  | { kind: "explicit" }
  | { kind: "provider"; line: number }
  /** `identity: {mode: …}` på én linje — utenfor det denne skanneren leser. */
  | { kind: "unreadable"; line: number }
  | { kind: "absent" };

/**
 * Leser identitetsmodusen ut av en eksisterende `agent.yaml`.
 *
 * Bevisst en liten skanner og ikke en YAML-parser: broen har null
 * runtime-avhengigheter, og spørsmålet er smalt nok. Vi leter etter
 * `mode:`-linjer som ligger ett nivå under en `identity:`-linje — ikke etter
 * `mode:` hvor som helst i fila (`code-server.settings.overwrite` og
 * `preseed.files[].mode` finnes også).
 */
export function identityMode(yamlText: string): IdentityVerdict {
  const lines = yamlText.split("\n");
  let identityIndent: number | null = null;
  let sawExplicit = false;
  let unreadableLine: number | null = null;

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i]!;
    const withoutComment = raw.replace(/#.*$/, "");
    if (withoutComment.trim() === "") continue;
    const indent = withoutComment.length - withoutComment.trimStart().length;
    const content = withoutComment.trim();

    if (identityIndent !== null && indent <= identityIndent) {
      // Blokka er slutt (samme eller lavere nivå enn `identity:` selv).
      identityIndent = null;
    }
    const inline = /^-?\s*identity:\s*(\S.*)$/.exec(content);
    if (inline) {
      // Flow-style (`identity: {mode: provider}`). Vi leser mode-en når den er
      // åpenbar, og flagger ellers at fila ikke kan verifiseres — heller det
      // enn å svare «ingen identitet» om en config som har en.
      if (/\bmode:\s*["']?provider\b/.test(inline[1]!)) return { kind: "provider", line: i + 1 };
      if (/\bmode:\s*["']?explicit\b/.test(inline[1]!)) sawExplicit = true;
      else unreadableLine ??= i + 1;
      continue;
    }
    if (/^-?\s*identity:\s*$/.test(content)) {
      identityIndent = indent;
      continue;
    }
    if (identityIndent === null) continue;

    const mode = /^-?\s*mode:\s*(\S+)\s*$/.exec(content);
    if (!mode) continue;
    const value = mode[1]!.replace(/^["']|["']$/g, "");
    if (value === "provider") return { kind: "provider", line: i + 1 };
    if (value === "explicit") sawExplicit = true;
  }
  if (sawExplicit) return { kind: "explicit" };
  if (unreadableLine !== null) return { kind: "unreadable", line: unreadableLine };
  return { kind: "absent" };
}

/**
 * Kaster hvis en eksisterende config har `identity.mode: provider`, og
 * returnerer en advarsel når identiteten mangler helt. Vi retter ikke fila
 * selv — den kan være håndredigert, og en stille omskriving er verre enn en
 * ærlig feil.
 */
export function identityProblem(
  yamlText: string,
  configPath: string,
): { level: "error" | "warn"; message: string } | undefined {
  const verdict = identityMode(yamlText);
  if (verdict.kind === "explicit") return undefined;
  if (verdict.kind === "provider") {
    return {
      level: "error",
      message:
        `${configPath} har 'identity.mode: provider' (linje ${verdict.line}). Det støttes ikke av ` +
        `broker-token/static_token-providere (M0-funn 4) — agenten ender uten commit-identitet og ` +
        `'git commit' feiler etter at jobben er gjort. Sett 'mode: explicit' med navn og e-post ` +
        `for bot-kontoen.`,
    };
  }
  if (verdict.kind === "unreadable") {
    return {
      level: "warn",
      message:
        `${configPath} har en 'identity'-blokk på én linje (linje ${verdict.line}) som broen ikke ` +
        `kan verifisere. Kontrollér selv at den er 'mode: explicit' med navn og e-post — ` +
        `'mode: provider' virker ikke for broker-token (M0-funn 4).`,
    };
  }
  return {
    level: "warn",
    message:
      `${configPath} har ingen 'identity'-blokk under git-credentials. Er ikke commit-identiteten ` +
      `satt et annet sted, feiler 'git commit' inne i instansen (M0-funn 4).`,
  };
}

function q(value: string): string {
  return JSON.stringify(value);
}

function requireNonEmpty(vars: AgentConfigVars, keys: (keyof AgentConfigVars)[]): void {
  const missing = keys.filter((k) => String(vars[k] ?? "").trim() === "");
  if (missing.length > 0) {
    throw new Error(
      `agent.yaml kan ikke genereres: mangler ${missing.join(", ")}. ` +
        `Commit-identitet og provider-navn må settes i .env (se .env.example).`,
    );
  }
}
