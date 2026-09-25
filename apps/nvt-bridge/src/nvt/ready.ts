/**
 * Klar-tilstand for CLI-sesjonen i instansen (M0-funn 5).
 *
 * `agentd` venter selv på at `session-launched`-markøren og tmux-sesjonen
 * finnes før den køer en prompt — men det sier bare at *tmux* lever. Claude kan
 * fortsatt stå på velkomstskjermen eller i trust-dialogen, og da går prompten
 * inn i dialogen i stedet for i sesjonen. Oppgaven forsvinner uten spor: ingen
 * feil, ingen `signal done`, bare en time med venting.
 *
 * Derfor leser broen tmux-panelet og klassifiserer det. Det er
 * mønstergjenkjenning mot et TUI, altså heuristikk — så mønstrene kan
 * overstyres med env (`NVT_READY_PATTERN`, `NVT_ONBOARDING_PATTERN`) uten
 * kodeendring når claude bytter ordlyd.
 */

export type PaneState =
  /** En onboarding-dialog står i veien. Et Enter tar oss videre. */
  | "onboarding"
  /** Sesjonen viser klar-prompt: trygt å injisere. */
  | "ready"
  /** Verken/eller — sesjonen kan være midt i arbeid, eller ikke tegnet ennå. */
  | "unknown";

/**
 * Skjermene som spiser prompten. `Press Enter to continue` og trust-dialogen er
 * de som traff i M0; resten er de andre førstegangsskjermene claude kan vise.
 *
 * Mønstrene er **linjeankrede** med vilje. Panelet inneholder også claudes
 * transkript, altså den delegerte prompten — upålitelig input. Uten anker
 * kunne en oppgavetekst som inneholdt «Do you trust the files» fått broen til
 * å tro at en dialog sto der, og dermed blokkert topicet.
 *
 * Ankeret godtar rammetegn og punktmarkører foran teksten (dialogene tegnes i
 * en boks: `│ ✻ Welcome to Claude Code!`), men **ikke** vanlig tekst eller
 * `>` — og et ekko av oppgaveteksten står alltid bak en `>`-prompt eller inne i
 * en setning. Det er skillet som gjør mønsteret trygt mot upålitelig input.
 */
export const DEFAULT_ONBOARDING_PATTERN =
  /^[\s│┃╭╮╰╯─━┌┐└┘✻✳✽*•]*(Welcome to Claude Code|Do you trust the files|Press Enter to continue|Choose the text style|Security notes)/im;

/**
 * Hvor mange linjer fra bunnen av panelet vi ser på. Dialogene tegnes på den
 * synlige skjermen; eldre transkript er bare støy som kan matche tilfeldig.
 */
export const PANE_TAIL_LINES = 24;

/**
 * Klar-prompten. `? for shortcuts` er footeren claude viser når input-boksen
 * tar imot tekst; `Bypassing Permissions` er indikatoren i
 * `--dangerously-skip-permissions`-modus.
 */
export const DEFAULT_READY_PATTERN = /(\? for shortcuts|Bypassing Permissions|│\s*>)/;

export interface PanePatterns {
  ready: RegExp;
  onboarding: RegExp;
}

export const DEFAULT_PANE_PATTERNS: PanePatterns = {
  ready: DEFAULT_READY_PATTERN,
  onboarding: DEFAULT_ONBOARDING_PATTERN,
};

/**
 * Onboarding vinner over klar-prompt: en dialog kan være tegnet oppå en sesjon
 * som ellers viser footeren, og da er det dialogen som får tastetrykket. Å
 * gjette «ready» der ville vært å injisere inn i dialogen — nøyaktig feilen vi
 * prøver å unngå.
 *
 * Klassifiseringen brukes bare på en **fersk** sesjon (se `waitUntilReady`).
 * Da er transkriptet tomt eller kort, og risikoen for at oppgavetekst matcher
 * er tilsvarende liten.
 */
export function classifyPane(
  paneText: string,
  patterns: PanePatterns = DEFAULT_PANE_PATTERNS,
): PaneState {
  const tail = lastLines(paneText, PANE_TAIL_LINES);
  if (patterns.onboarding.test(tail)) return "onboarding";
  if (patterns.ready.test(tail)) return "ready";
  return "unknown";
}

function lastLines(text: string, count: number): string {
  const lines = text.split("\n");
  return lines.length <= count ? text : lines.slice(-count).join("\n");
}

/**
 * Bygger et mønster fra env. Ugyldig regex skal stoppe oppstarten, ikke gi en
 * sesjon som aldri blir «klar».
 */
export function patternFromEnv(raw: string | undefined, fallback: RegExp, name: string): RegExp {
  const value = (raw ?? "").trim();
  if (value === "") return fallback;
  try {
    return new RegExp(value, "i");
  } catch (err) {
    throw new Error(`${name}: ugyldig regulært uttrykk (${err instanceof Error ? err.message : err})`);
  }
}
