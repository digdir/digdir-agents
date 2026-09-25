import assert from "node:assert/strict";
import { test } from "node:test";
import { classifyPane, DEFAULT_READY_PATTERN, patternFromEnv } from "./ready.ts";

test("velkomstskjerm og trust-dialog klassifiseres som onboarding", () => {
  // Slik claude faktisk tegner dem: tekst inne i en ramme.
  assert.equal(classifyPane("│ ✻ Welcome to Claude Code!"), "onboarding");
  assert.equal(classifyPane("╭─────────╮\n│ Do you trust the files in this folder?"), "onboarding");
  assert.equal(classifyPane("Do you trust the files in this folder?"), "onboarding");
  assert.equal(classifyPane("  Press Enter to continue…"), "onboarding");
});

test("klar-prompten klassifiseres som ready", () => {
  assert.equal(classifyPane("│ > │\n  ? for shortcuts"), "ready");
  assert.equal(classifyPane("⏵⏵ Bypassing Permissions"), "ready");
});

test("en onboarding-dialog oppå en ellers klar sesjon er IKKE klar", () => {
  const pane = ["│ >", "? for shortcuts", "Do you trust the files in this folder?"].join("\n");
  assert.equal(
    classifyPane(pane),
    "onboarding",
    "dialogen fanger tastetrykket — å gjette «ready» her er nettopp M0-feilen",
  );
});

test("oppgavetekst i transkriptet utløser IKKE onboarding-tilstand", () => {
  // Panelet inneholder claudes transkript, altså den delegerte prompten —
  // upålitelig input. Uten linjeanker kunne avsender fått broen til å tro at en
  // dialog sto der, og dermed blokkert topicet (og fått Enter sendt inn i en
  // levende sesjon).
  const pane = [
    "> Fiks dette: dialogen «Do you trust the files in this folder?» henger",
    "● Ser på saken …",
    "│ > ",
    "  ? for shortcuts",
  ].join("\n");
  assert.equal(classifyPane(pane), "ready");
});

test("bare de siste linjene av panelet vurderes", () => {
  const pane = ["Welcome to Claude Code", ...Array(40).fill("● jobber …")].join("\n");
  assert.equal(classifyPane(pane), "unknown", "gammelt transkript skal ikke matche");
});

test("et panel som jobber er verken onboarding eller ready", () => {
  assert.equal(classifyPane("● Kjører tester …\n  npm test"), "unknown");
  assert.equal(classifyPane(""), "unknown");
});

test("mønstre kan overstyres fra env, og ugyldig regex stopper oppstarten", () => {
  const custom = patternFromEnv("klar til bruk", DEFAULT_READY_PATTERN, "NVT_READY_PATTERN");
  assert.equal(classifyPane("KLAR TIL BRUK", { ready: custom, onboarding: /aldri/ }), "ready");
  assert.equal(patternFromEnv("  ", DEFAULT_READY_PATTERN, "x"), DEFAULT_READY_PATTERN);
  assert.throws(() => patternFromEnv("(ubalansert", DEFAULT_READY_PATTERN, "NVT_READY_PATTERN"), /NVT_READY_PATTERN/);
});
