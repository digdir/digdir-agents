import assert from "node:assert/strict";
import { test } from "node:test";
import { isOwned, parseCodeowners } from "./codeowners.ts";

const REAL_CODEOWNERS = `
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
  const rules = parseCodeowners(REAL_CODEOWNERS);
  assert.equal(isOwned(rules, "integrations/src/config.ts"), true);
  assert.equal(isOwned(rules, "integrations/README.md"), false);
});

test("matcher gjenkjenner **/x på hvilken som helst dybde", () => {
  const rules = parseCodeowners(REAL_CODEOWNERS);
  assert.equal(isOwned(rules, "docker-compose.yml"), true);
  assert.equal(isOwned(rules, "agents/foo/docker-compose.yml"), true);
  assert.equal(isOwned(rules, "agents/foo/Dockerfile"), true);
});

test("matcher gjenkjenner * innad i ett stinivå (agents/*/CLAUDE.md)", () => {
  const rules = parseCodeowners(REAL_CODEOWNERS);
  assert.equal(isOwned(rules, "agents/proxy-agent/CLAUDE.md"), true);
  assert.equal(isOwned(rules, "agents/proxy-agent/nested/CLAUDE.md"), false);
});

test("matcher gjenkjenner katalogmønster (agents/*/docker/skills/)", () => {
  const rules = parseCodeowners(REAL_CODEOWNERS);
  assert.equal(isOwned(rules, "agents/proxy-agent/docker/skills/foo.md"), true);
  assert.equal(isOwned(rules, "agents/proxy-agent/docker/other.md"), false);
});

test("filer utenfor alle mønstre er ikke eid", () => {
  const rules = parseCodeowners(REAL_CODEOWNERS);
  assert.equal(isOwned(rules, "README.md"), false);
  assert.equal(isOwned(rules, "workflows/documentation/utils/differ/package.json"), false);
});

test("tomme linjer og kommentarer ignoreres", () => {
  const rules = parseCodeowners("\n# bare en kommentar\n\n");
  assert.deepEqual(rules, []);
});
