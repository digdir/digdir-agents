import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { isOwned, parseCodeowners } from "./codeowners.ts";

const LEGACY_CODEOWNERS = `
# CODEOWNERS
agents/*/CLAUDE.md            @olebhansen
agents/*/docker/skills/       @olebhansen
agents/*/docker/entrypoint.sh @olebhansen

integrations/src/             @olebhansen
**/docker-compose.yml         @olebhansen
**/Dockerfile                 @olebhansen
scripts/                      @olebhansen

.github/                      @olebhansen
`;

test("matcher gjenkjenner filer under integrations/src/", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  assert.equal(isOwned(rules, "integrations/src/config.ts"), true);
  assert.equal(isOwned(rules, "integrations/README.md"), false);
});

test("matcher gjenkjenner **/x på hvilken som helst dybde", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  assert.equal(isOwned(rules, "docker-compose.yml"), true);
  assert.equal(isOwned(rules, "agents/foo/docker-compose.yml"), true);
  assert.equal(isOwned(rules, "agents/foo/Dockerfile"), true);
});

test("matcher gjenkjenner * innad i ett stinivå (agents/*/CLAUDE.md)", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  assert.equal(isOwned(rules, "agents/proxy-agent/CLAUDE.md"), true);
  assert.equal(isOwned(rules, "agents/proxy-agent/nested/CLAUDE.md"), false);
});

test("matcher gjenkjenner katalogmønster (agents/*/docker/skills/)", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  assert.equal(isOwned(rules, "agents/proxy-agent/docker/skills/foo.md"), true);
  assert.equal(isOwned(rules, "agents/proxy-agent/docker/other.md"), false);
});

test("katalogregel (scripts/) matcher ikke en fil som heter scripts", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  assert.equal(isOwned(rules, "scripts"), false);
  assert.equal(isOwned(rules, "scripts/foo.ts"), true);
  assert.equal(isOwned(rules, "scripts/nested/foo.ts"), true);
});

test("katalogregel matcher ikke katalognavnet som fil på dypere nivå heller", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  // "agents/*/docker/skills/" eier filer i skills/, ikke en fil ved navn skills.
  assert.equal(isOwned(rules, "agents/proxy-agent/docker/skills"), false);
  assert.equal(isOwned(rules, "integrations/src"), false);
  assert.equal(isOwned(rules, "integrations/src/config.ts"), true);
});

test("filer utenfor alle mønstre er ikke eid", () => {
  const rules = parseCodeowners(LEGACY_CODEOWNERS);
  assert.equal(isOwned(rules, "README.md"), false);
  assert.equal(isOwned(rules, "workflows/documentation/utils/differ/package.json"), false);
});

test("tomme linjer og kommentarer ignoreres", () => {
  const rules = parseCodeowners("\n# bare en kommentar\n\n");
  assert.deepEqual(rules, []);
});

test("siste treff vinner, og en linje uten eier fjerner eierskapet", () => {
  const rules = parseCodeowners(`
*                 @olebhansen
/doc/
/doc/pr-prosess.md @olebhansen
/agentctl/        @martinothamar
`);
  assert.equal(isOwned(rules, "integrations/src/index.ts"), true);
  assert.equal(isOwned(rules, "en/helt/ny/sti.ts"), true);
  assert.equal(isOwned(rules, "doc/plans/x.md"), false);
  assert.equal(isOwned(rules, "doc/pr-prosess.md"), true);
  assert.equal(isOwned(rules, "agentctl/src/lib.rs"), true);
});

// Repoets egen CODEOWNERS: låser hvilke stier som kan merges uten review.
// Feiler denne, har en endring gjort en sensitiv sti trygg (eller omvendt) —
// oppdater listene her bevisst.
const REPO_CODEOWNERS = parseCodeowners(
  readFileSync(new URL("../../../.github/CODEOWNERS", import.meta.url), "utf-8"),
);

test("repoets CODEOWNERS: kun dokumentasjon er trygt", () => {
  for (const path of [
    "README.md",
    "LICENSE",
    "docs/presentations/agentctl-sandbox-sdk/slides.md",
    "agents/ny-agent/README.md",
    "integrations/README.md",
  ]) {
    assert.equal(isOwned(REPO_CODEOWNERS, path), false, path);
  }
});

test("repoets CODEOWNERS: alt annet krever review", () => {
  for (const path of [
    // agent-instrukser og -definisjoner, uansett filnavn
    "agents/ny-agent/agent.yaml",
    "agents/ny-agent/instructions.md",
    "agents/self-dev/README.md",
    "agents/self-dev/Dockerfile",
    // tjenester og avhengighetene deres
    "integrations/src/config.ts",
    "integrations/package.json",
    "integrations/package-lock.json",
    "integrations/tsconfig.json",
    "docs/presentations/agentctl-sandbox-sdk/package.json",
    // drift og prosessen selv
    "renovate.json",
    ".gitignore",
    ".github/CODEOWNERS",
    ".github/workflows/ci.yml",
    "images/agentctl-base/Dockerfile",
    "agentctl/src/lib.rs",
    // stier som ikke finnes (lenger)
    "doc/index.md",
    "apps/ny-tjeneste/index.ts",
    "ny-mappe/fil.txt",
  ]) {
    assert.equal(isOwned(REPO_CODEOWNERS, path), true, path);
  }
});
