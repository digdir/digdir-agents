#!/usr/bin/env node
// Builds the architecture diagrams from the hand-written sources in src/.
//
// Each source is a plain SVG that marks where the shared parts go with a
// `<!-- @shared -->` comment. The build replaces it with the shared stylesheet,
// the shared definitions and the fonts, embedded so a diagram renders the same
// in any browser and on GitHub, and writes the result beside this script.
//
//   node build.mjs              write the built SVGs
//   node build.mjs --check      fail when a built SVG is missing or stale
//   node build.mjs --png DIR    also render each diagram to DIR/<name>.png at 2x

import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, readdirSync, writeFileSync, existsSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));
const srcDir = join(root, "src");
const marker = "<!-- @shared -->";

function font(family, file, weights) {
  const data = readFileSync(join(root, "fonts", file)).toString("base64");
  return `@font-face { font-family: "${family}"; font-weight: ${weights}; src: url(data:font/woff2;base64,${data}) format("woff2"); }`;
}

function shared() {
  const css = readFileSync(join(srcDir, "shared.css"), "utf8").trim();
  const defs = readFileSync(join(srcDir, "shared-defs.svg"), "utf8").trim();
  const fonts = [
    font("Inter", "Inter-latin.woff2", "400 700"),
    font("JetBrains Mono", "JetBrainsMono-latin.woff2", "400 600"),
  ].join("\n");
  return `<style>\n${fonts}\n${css}\n</style>\n${defs}`;
}

function build() {
  const sharedBlock = shared();
  const built = [];
  for (const file of readdirSync(srcDir).filter((f) => f.endsWith(".svg") && f !== "shared-defs.svg").sort()) {
    const source = readFileSync(join(srcDir, file), "utf8");
    if (!source.includes(marker)) {
      throw new Error(`src/${file} has no ${marker} marker`);
    }
    const header = `<!-- Built from src/${file} by build.mjs; edit the source and run \`make diagrams\`. -->\n`;
    const output = source.replace(/^(<svg[^>]*>\n?)/, `$1${header}`).replace(marker, sharedBlock);
    built.push({ file, path: join(root, file), output });
  }
  return built;
}

function renderPng(svgPath, outDir) {
  const svg = readFileSync(svgPath, "utf8");
  const [, , width, height] = svg.match(/viewBox="([^"]+)"/)[1].split(/\s+/).map(Number);
  const out = join(outDir, basename(svgPath, ".svg") + ".png");
  const html = join(outDir, basename(svgPath, ".svg") + ".html");
  // An HTML wrapper sizes the page to the diagram exactly.
  writeFileSync(
    html,
    `<!doctype html><style>html,body{margin:0}img{display:block}</style><img src="file://${svgPath}" width="${width}" height="${height}">`,
  );
  execFileSync(
    process.env.CHROMIUM ?? "chromium",
    [
      "--headless=new",
      "--disable-gpu",
      "--hide-scrollbars",
      "--force-device-scale-factor=2",
      `--window-size=${Math.ceil(width)},${Math.ceil(height)}`,
      `--screenshot=${out}`,
      `file://${html}`,
    ],
    { stdio: "ignore" },
  );
  return out;
}

const args = process.argv.slice(2);
const check = args.includes("--check");
const pngIndex = args.indexOf("--png");
const pngDir = pngIndex >= 0 ? resolve(args[pngIndex + 1] ?? "") : null;

const built = build();
if (check) {
  const stale = built.filter(({ path, output }) => !existsSync(path) || readFileSync(path, "utf8") !== output);
  if (stale.length > 0) {
    console.error(`Stale architecture diagrams: ${stale.map(({ file }) => file).join(", ")}. Run \`make diagrams\`.`);
    process.exit(1);
  }
  console.log("✓ Architecture diagrams are up to date");
} else {
  for (const { path, output } of built) {
    writeFileSync(path, output);
  }
  console.log(`✓ Built ${built.length} architecture diagrams`);
}

if (pngDir) {
  mkdirSync(pngDir, { recursive: true });
  for (const { path } of built) {
    console.log(renderPng(path, pngDir));
  }
}
