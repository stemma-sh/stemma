import { chromium } from "playwright";
const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";
const DOCX = process.env.DOCX;
const DELAY = 800;

let failures = 0;
const check = (name, cond) => { console.log(`${cond ? "PASS" : "FAIL"}  ${name}`); if (!cond) failures++; };

const browser = await chromium.launch();

// ── 1. Masking: a slow server, but the redline shows instantly ────────────────
{
  const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  // Every /apply is delayed by DELAY ms.
  await page.route("**/apply", async (route) => {
    await new Promise((r) => setTimeout(r, DELAY));
    await route.continue();
  });

  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.setInputFiles("#file", DOCX);
  await page.waitForSelector(".ProseMirror [data-id]", { timeout: 20000 });

  const t = page.locator('.ProseMirror p:has-text("[Company Name]")').first();
  await t.click({ clickCount: 3 });
  await page.keyboard.type("Acme Corporation");

  const t0 = Date.now();
  await page.keyboard.press("Control+Enter");           // commit (suggesting/tracked)
  await page.waitForSelector(".ProseMirror ins", { timeout: 600 }); // must beat the 800ms server
  const shownAfter = Date.now() - t0;
  check(`redline shown optimistically in ${shownAfter}ms (server delayed ${DELAY}ms)`, shownAfter < DELAY);
  check("optimistic redline has <del> too", (await page.locator(".ProseMirror del").count()) >= 1);
  check("block marked pending (unconfirmed)", (await page.locator(".pm-pending").count()) >= 1);
  check("status shows syncing", /Saving/.test(await page.locator("#commit").textContent()));

  // Now let the server respond and confirm.
  await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit").textContent), null, { timeout: 5000 });
  check("pending cleared after confirm", (await page.locator(".pm-pending").count()) === 0);
  check("authoritative redline still present", (await page.locator(".ProseMirror ins").count()) >= 1);
  check("masking test: no page errors", errors.length === 0);
  await page.close();
}

// ── 2. Rollback: the server rejects, the optimistic edit reverts ──────────────
{
  const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  // Force every /apply to fail with a StaleEdit-style error after a short delay.
  await page.route("**/apply", async (route) => {
    await new Promise((r) => setTimeout(r, 200));
    await route.fulfill({
      status: 422,
      contentType: "application/json",
      body: JSON.stringify({ code: "StaleEdit", error: "simulated stale guard" }),
    });
  });

  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.setInputFiles("#file", DOCX);
  await page.waitForSelector(".ProseMirror [data-id]", { timeout: 20000 });

  const t = page.locator('.ProseMirror p:has-text("[Company Name]")').first();
  await t.click({ clickCount: 3 });
  await page.keyboard.type("Acme Corporation");
  await page.keyboard.press("Control+Enter");

  // Optimistic redline appears first…
  await page.waitForSelector(".ProseMirror ins", { timeout: 600 });
  check("optimistic redline appeared before rejection", true);

  // …then the rejection rolls it back, surfacing a FRIENDLY error (not raw
  // engine jargon like "StaleEdit") and reverting to authoritative server state.
  await page.waitForFunction(() => document.querySelector("#status.error") !== null, null, { timeout: 5000 });
  check("rollback shows a friendly message (no raw engine code)", !/StaleEdit/.test(await page.locator("#status").textContent()));
  check("rolled back: optimistic insertion removed", (await page.locator(".ProseMirror ins").count()) === 0);
  check("rolled back: pending cleared", (await page.locator(".pm-pending").count()) === 0);
  check("rolled back: original text restored", /\[Company Name\]/.test(await page.locator(".ProseMirror").first().textContent()));
  check("error surfaced loudly", (await page.locator("#status.error").count()) === 1);
  check("rollback test: no page errors", errors.length === 0);
  await page.close();
}

await browser.close();
console.log(`\nLATENCY: ${failures === 0 ? "ALL PASSED" : failures + " FAILED"}`);
process.exit(failures === 0 ? 0 : 1);
