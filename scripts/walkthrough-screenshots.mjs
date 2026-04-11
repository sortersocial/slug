#!/usr/bin/env node
/**
 * Full-page screenshots of the walkthrough thread in each theme.
 * Private room URLs require a session: this visits /login first (mock OAuth),
 * then the thread. Run `bb walkthrough-fixture` first; reads
 * /tmp/slug-walkthrough-fixture/summary.json for URLs when present.
 *
 * Requires Playwright (`npm i playwright` somewhere, or e.g. `/tmp/node_modules/playwright`).
 *
 * Usage:
 *   node scripts/walkthrough-screenshots.mjs
 *   SLUG_BASE_URL=http://127.0.0.1:8080 THREAD_URL=... node scripts/walkthrough-screenshots.mjs
 */
import { readFileSync, mkdirSync, existsSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { createRequire } from "module";

const require = createRequire(import.meta.url);
function loadPlaywright() {
  try {
    return require("playwright");
  } catch {
    return require("/tmp/node_modules/playwright");
  }
}
const { chromium } = loadPlaywright();

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const outDir = join(root, ".cursor-screenshots");

function loadSummary() {
  const p = "/tmp/slug-walkthrough-fixture/summary.json";
  if (!existsSync(p)) return null;
  try {
    return JSON.parse(readFileSync(p, "utf8"));
  } catch {
    return null;
  }
}

const summary = loadSummary();
const baseUrl =
  process.env.SLUG_BASE_URL ||
  summary?.base_url ||
  "http://127.0.0.1:8080";
const threadUrl =
  process.env.THREAD_URL || summary?.summary?.room?.thread_url;

if (!threadUrl) {
  console.error(
    "Set THREAD_URL or run bb walkthrough-fixture (summary at /tmp/slug-walkthrough-fixture/summary.json)."
  );
  process.exit(1);
}

mkdirSync(outDir, { recursive: true });

const themes = [
  ["default", "01-thread-default.png"],
  ["retro", "02-thread-retro-spare.png"],
  ["retro_craft", "03-thread-retro-craft.png"],
];

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 900, height: 1000 } });
const page = await context.newPage();

await page.goto(new URL("/login", baseUrl).href, { waitUntil: "domcontentloaded", timeout: 60_000 });
await page.waitForURL(
  (url) =>
    url.origin === new URL(baseUrl).origin &&
    (url.pathname === "/" || url.pathname === "/auth/complete"),
  { timeout: 45_000 }
);

await page.goto(threadUrl, { waitUntil: "domcontentloaded", timeout: 30_000 });
await page.getByRole("link", { name: "log out" }).waitFor({ state: "visible", timeout: 15_000 });

for (const [name, file] of themes) {
  await page.evaluate((n) => {
    const el = document.getElementById("theme-stylesheet");
    if (el) el.href = "/static/theme_" + n + ".css";
    const sw = document.getElementById("theme-switcher");
    if (sw) sw.textContent = n === "retro_craft" ? "craft" : n;
  }, name);
  await page.waitForTimeout(400);
  await page.screenshot({ path: join(outDir, file), fullPage: true });
}

await browser.close();
console.log("Wrote", themes.map(([, f]) => join(outDir, f)).join(", "));
