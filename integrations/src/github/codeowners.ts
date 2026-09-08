/**
 * Minimal CODEOWNERS pattern matcher — enough of the gitignore-style subset
 * GitHub documents (trailing "/" for a directory, "*" within one path
 * segment, leading "**\/" for any depth) to answer "does this changed file
 * fall under a CODEOWNERS-protected path?" for the review notifier (issue
 * #115): a PR with the `auto-merge` label that still touches such a path is
 * one branch protection will block on Code Owners regardless of the label
 * (see doc/pr-prosess.md), so it still needs a human notified.
 */
export interface CodeownersRule {
  regex: RegExp;
}

export function parseCodeowners(content: string): CodeownersRule[] {
  const rules: CodeownersRule[] = [];
  for (const rawLine of content.split("\n")) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const pattern = line.split(/\s+/)[0];
    if (!pattern) continue;
    rules.push({ regex: patternToRegex(pattern) });
  }
  return rules;
}

export function isOwned(rules: CodeownersRule[], filePath: string): boolean {
  const normalized = filePath.replace(/^\/+/, "");
  return rules.some((r) => r.regex.test(normalized));
}

function patternToRegex(pattern: string): RegExp {
  let p = pattern;
  const isDir = p.endsWith("/");
  if (isDir) p = p.slice(0, -1);
  // A slash anywhere but the (already-stripped) trailing position anchors
  // the pattern to the repo root, mirroring gitignore's rule.
  const anchored = p.includes("/");
  if (p.startsWith("/")) p = p.slice(1);

  let out = "";
  let i = 0;
  while (i < p.length) {
    if (p.startsWith("**/", i)) {
      out += "(?:.*/)?";
      i += 3;
    } else if (p[i] === "*") {
      out += "[^/]*";
      i += 1;
    } else {
      out += p[i].replace(/[.+^${}()|[\]\\]/g, "\\$&");
      i += 1;
    }
  }

  const prefix = anchored ? "^" : "^(?:.*/)?";
  // A directory rule ("scripts/") owns the files *inside* the directory, so the
  // separator is required — a plain file named "scripts" is not a match.
  const suffix = isDir ? "/.*$" : "$";
  return new RegExp(prefix + out + suffix);
}
