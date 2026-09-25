import { existsSync } from "node:fs";
import { realpath, stat } from "node:fs/promises";
import path from "node:path";

/**
 * Sti-validering mot M0-funn 2: nvt-sjekkouten (og alt annet som
 * bind-mountes inn i instansen) må ligge et sted **uid 1000 kommer gjennom**.
 *
 * Hvorfor det er et reelt problem og ikke teori: workspacet bind-mountes på
 * samme absolutte sti inne i containeren, og i non-root-modus kjører agenten
 * som `1000:1000`. Ligger sjekkouten under `/root` (mode 700, eier 0), får
 * bootstrap `Permission denied` — men først *inne* i containeren, som en
 * kryptisk feil langt fra årsaken. M0 brant en økt på nettopp dette, så broen
 * sier det fra før den starter å polle.
 *
 * Sjekken håndheves bare når broen faktisk *ser* hosten: på Linux, og ikke i
 * container. Kjører broen selv i container (den dokumenterte driftsformen), er
 * bare `NVT_ROOT` og triggers-katalogen bind-mountet — mellomleddene er
 * mount-point-foreldre Docker har laget, med helt andre mode-bits enn hostens.
 * Da ville både falske avvisninger og falsk trygghet vært mulig, så funnene
 * logges som advarsel i stedet. Det samme gjelder macOS, der Docker Desktop
 * mapper eierskap i virtiofs-laget. Se `assertAgentCanTraverse`.
 */

/** Uid/gid agenten kjører som i nvt-ens non-root-modus (`--user non-root`). */
export const AGENT_UID = 1000;
export const AGENT_GID = 1000;

export interface StatResult {
  mode: number;
  uid: number;
  gid: number;
}

export type StatFn = (target: string) => Promise<StatResult>;

export interface TraversalProblem {
  path: string;
  /** Mangler stien helt, er `mode`/`uid` ukjent. */
  reason: "missing" | "not-traversable" | "not-writable";
  mode?: number;
  uid?: number;
}

/** `/srv/nvt-agent` → `["/", "/srv", "/srv/nvt-agent"]`. */
export function ancestorPaths(absolute: string): string[] {
  const normalized = path.posix.normalize(absolute);
  const segments = normalized.split("/").filter((s) => s !== "");
  const out = ["/"];
  let current = "";
  for (const segment of segments) {
    current += `/${segment}`;
    out.push(current);
  }
  return out;
}

/**
 * Kan uid 1000 traversere (`x`) denne katalogen? Vi ser bare på traverserings-
 * biten: lese- og skrivetilgang på selve innholdet er en annen sak, og den
 * feiler i det minste med en forståelig melding.
 */
export function agentCanTraverse(info: StatResult): boolean {
  if ((info.mode & 0o001) !== 0) return true;
  if (info.uid === AGENT_UID && (info.mode & 0o100) !== 0) return true;
  if (info.gid === AGENT_GID && (info.mode & 0o010) !== 0) return true;
  return false;
}

/** Kan uid 1000 skrive i katalogen? Brukes for triggers/, som agenten appender i. */
export function agentCanWrite(info: StatResult): boolean {
  if ((info.mode & 0o002) !== 0) return true;
  if (info.uid === AGENT_UID && (info.mode & 0o200) !== 0) return true;
  if (info.gid === AGENT_GID && (info.mode & 0o020) !== 0) return true;
  return false;
}

/**
 * Alle ledd i stien uid 1000 ikke kommer gjennom, øverste først.
 *
 * `absolute` forventes å være realpath-løst (se `assertAgentCanTraverse`):
 * vandringen er leksikalsk, så en symlenke midt i stien ville ellers skjult at
 * *målets* foreldre er ugjennomtrengelige.
 */
export async function traversalProblems(
  absolute: string,
  statFn: StatFn = defaultStat,
  opts: { requireWrite?: boolean } = {},
): Promise<TraversalProblem[]> {
  const problems: TraversalProblem[] = [];
  const dirs = ancestorPaths(absolute);
  for (const dir of dirs) {
    let info: StatResult;
    try {
      info = await statFn(dir);
    } catch {
      problems.push({ path: dir, reason: "missing" });
      // Finnes ikke leddet, sier resten av stien ingenting.
      break;
    }
    if (!agentCanTraverse(info)) {
      problems.push({
        path: dir,
        reason: "not-traversable",
        mode: info.mode & 0o7777,
        uid: info.uid,
      });
    }
    // Skrivetilgang kreves bare på katalogen selv, ikke på foreldrene.
    if (opts.requireWrite && dir === dirs.at(-1) && !agentCanWrite(info)) {
      problems.push({
        path: dir,
        reason: "not-writable",
        mode: info.mode & 0o7777,
        uid: info.uid,
      });
    }
  }
  return problems;
}

export interface PathCheckOptions {
  statFn?: StatFn;
  /** Løser symlenker før vandringen. Default `fs.realpath`. */
  realpathFn?: (target: string) => Promise<string>;
  /** Default `process.platform`. Håndheves bare på Linux (se over). */
  platform?: string;
  /**
   * Kjører broen selv i container? Default: finnes `/.dockerenv`. Da ser vi
   * ikke hostens mode-bits, og funnene blir en advarsel.
   */
  containerized?: boolean;
  /** Krev at agenten kan skrive i katalogen (triggers/). */
  requireWrite?: boolean;
  /** Rømningsluke: `NVT_BRIDGE_SKIP_PATH_CHECK=1`. */
  skip?: boolean;
  log?: (message: string) => void;
}

/**
 * Fail-fast før broen begynner å polle: kaster med en melding som peker på
 * *hvilket* ledd i stien som stopper uid 1000, og hva man gjør med det.
 *
 * Kaster bare når broen faktisk ser hosten (Linux, ikke i container) — ellers
 * logges funnet som advarsel, fordi mode-bitene vi leser da ikke er de agenten
 * møter. Se kommentaren øverst i fila.
 */
export async function assertAgentCanTraverse(
  label: string,
  absolute: string,
  opts: PathCheckOptions = {},
): Promise<void> {
  const log = opts.log ?? (() => {});
  if (opts.skip) {
    log(`${label}: sti-sjekken er hoppet over (NVT_BRIDGE_SKIP_PATH_CHECK)`);
    return;
  }
  if (!path.posix.isAbsolute(absolute)) {
    throw new Error(
      `${label} må være en absolutt sti (fikk "${absolute}"). ` +
        `Bind-mounts løses av hostens Docker-daemon, så relative stier kan ikke oversettes.`,
    );
  }

  // Symlenker: vandringen under er leksikalsk, så vi må se på den faktiske
  // stien. Feiler realpath (finnes ikke), sier vandringen det tydeligere.
  const realpathFn = opts.realpathFn ?? defaultRealpath;
  let resolved = absolute;
  try {
    resolved = await realpathFn(absolute);
  } catch {
    // Beholder den oppgitte stien; `traversalProblems` rapporterer «missing».
  }

  const problems = await traversalProblems(resolved, opts.statFn, {
    requireWrite: opts.requireWrite,
  });
  if (problems.length === 0) return;

  const details = problems
    .map((p) => {
      const mode = `mode ${(p.mode ?? 0).toString(8).padStart(4, "0")}, eier uid ${p.uid}`;
      if (p.reason === "missing") return `${p.path} (finnes ikke, eller broen får ikke lese den)`;
      if (p.reason === "not-writable") return `${p.path} (ikke skrivbar: ${mode})`;
      return `${p.path} (${mode})`;
    })
    .join(", ");
  const via = resolved === absolute ? "" : ` (realpath: "${resolved}")`;
  const message =
    `${label}="${absolute}"${via} er ikke tilgjengelig for uid ${AGENT_UID}: ${details}. ` +
    `Agenten kjører som ${AGENT_UID}:${AGENT_GID} (nvt --user non-root) og får ` +
    `«Permission denied» i bootstrap. Flytt sjekkouten til en sti alle kan traversere ` +
    `(f.eks. /srv/nvt-agent), eller gi leddet +x. Kjent M0-felle: /root er mode 0700.`;

  const platform = opts.platform ?? process.platform;
  const containerized = opts.containerized ?? existsSync("/.dockerenv");
  if (platform !== "linux") {
    log(`ADVARSEL: ${message} (ikke håndhevet på ${platform})`);
    return;
  }
  if (containerized) {
    // I container er mellomleddene mount-point-foreldre Docker har laget, ikke
    // hostens kataloger. Vi kan altså ikke stole på funnet — men det kan være
    // ekte, så det skal stå i loggen.
    log(
      `ADVARSEL: ${message} (broen kjører i container og ser ikke hostens mode-bits — ` +
        `sjekk stien på hosten selv)`,
    );
    return;
  }
  throw new Error(message);
}

const defaultStat: StatFn = async (target) => {
  const info = await stat(target);
  return { mode: info.mode, uid: info.uid, gid: info.gid };
};

const defaultRealpath = (target: string): Promise<string> => realpath(target);
