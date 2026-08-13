import { chromium } from "playwright-core";
const exe = process.env.HOME + "/Library/Caches/ms-playwright/chromium_headless_shell-1228/chrome-headless-shell-mac-arm64/chrome-headless-shell";
const browser = await chromium.launch({ executablePath: exe, headless: true });
const shots = [
  { url: "http://localhost:1420/?screen=onboarding&ob_step=1", name: "onboarding-1", w: 1472, h: 812 },
  { url: "http://localhost:1420/?screen=onboarding&ob_step=2", name: "onboarding-2", w: 1472, h: 812 },
  { url: "http://localhost:1420/?screen=onboarding&ob_step=3", name: "onboarding-3", w: 1472, h: 812 },
  { url: "http://localhost:1420/?screen=onboarding&ob_step=4", name: "onboarding-4", w: 1472, h: 812 },
  { url: "http://localhost:1420/?screen=locked", name: "locked", w: 1180, h: 780 },
  { url: "http://localhost:1420/?screen=app", name: "app-accueil", w: 1180, h: 780 },
  { url: "http://localhost:1420/bar.html", name: "bar", w: 572, h: 66 },
];
for (const s of shots) {
  const page = await browser.newPage({ viewport: { width: s.w, height: s.h } });
  await page.goto(s.url, { waitUntil: "networkidle" });
  await page.waitForTimeout(700);
  await page.screenshot({ path: `/tmp/syn-shots/${s.name}.png` });
  await page.close();
}
// Réglages (modale) : ouvrir depuis l'app
const page = await browser.newPage({ viewport: { width: 1180, height: 780 } });
await page.goto("http://localhost:1420/?screen=app", { waitUntil: "networkidle" });
await page.waitForTimeout(600);
await page.click("text=Réglages");
await page.waitForTimeout(400);
await page.click(".settings-tab:has-text('Règles')");
await page.waitForTimeout(400);
await page.screenshot({ path: "/tmp/syn-shots/settings-regles.png" });
// Connecteurs
await page.click(".settings-close");
await page.click(".side-item:has-text('Connecteurs')");
await page.waitForTimeout(500);
await page.screenshot({ path: "/tmp/syn-shots/app-connecteurs.png" });
// Conversations
await page.click(".side-item:has-text('Conversations')");
await page.waitForTimeout(400);
await page.screenshot({ path: "/tmp/syn-shots/app-conversations.png" });
await page.close();
await browser.close();
console.log("OK");
