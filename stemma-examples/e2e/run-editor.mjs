import { chromium } from "playwright";
const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";
const DOCX = process.env.DOCX;

let failures = 0;
const check = (name, cond) => { console.log(`${cond ? "PASS" : "FAIL"}  ${name}`); if (!cond) failures++; };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
const errors = [];
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
page.on("pageerror", (e) => errors.push(String(e)));

console.log(`→ ${BASE}`);
await page.goto(BASE, { waitUntil: "networkidle" });
check("title", (await page.title()).includes("editor"));

await page.setInputFiles("#file", DOCX);
await page.waitForSelector(".ProseMirror [data-id]", { timeout: 20000 });
check("blocks rendered", (await page.locator(".ProseMirror [data-id]").count()) > 10);

// 1. Suggesting (default) → tracked change.
console.log("→ suggesting-mode edit");
check("suggesting is default", (await page.getAttribute("#mode-suggesting", "aria-pressed")) === "true");
const t = page.locator('.ProseMirror p:has-text("[Company Name]")').first();
await t.click({ clickCount: 3 });
await page.keyboard.type("Acme Corporation");
await page.waitForFunction(() => /Saving/.test(document.querySelector("#commit").textContent), null, { timeout: 5000 });
await page.click("#commit");
await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit").textContent), null, { timeout: 15000 });
check("tracked: <ins> present", (await page.locator(".ProseMirror ins").count()) >= 1);
check("tracked: <del> present", (await page.locator(".ProseMirror del").count()) >= 1);

// 2. Editing (direct) → no redline, block stays editable.
console.log("→ editing-mode (direct) edit");
await page.click("#mode-editing");
check("editing mode active", (await page.getAttribute("#mode-editing", "aria-pressed")) === "true");
const insBefore = await page.locator(".ProseMirror ins").count();
const p1 = page.locator('.ProseMirror p[data-id="p_1"]').first();
await p1.click({ clickCount: 3 });
await page.keyboard.type("Directly edited legend.");
await page.waitForFunction(() => /Saving/.test(document.querySelector("#commit").textContent), null, { timeout: 5000 });
// Commit via the keyboard (Mod-Enter) so focus stays in the editor — the real
// in-editor flow, and what lets us prove the caret survives the commit.
await page.keyboard.press("Control+Enter");
await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit").textContent), null, { timeout: 15000 });
const insAfter = await page.locator(".ProseMirror ins").count();
check("direct edit added NO new insertion", insAfter === insBefore);
check("direct-edited block has no redline", await page.evaluate(() => {
  const el = document.querySelector('[data-id="p_1"]');
  return el && el.querySelectorAll("ins,del").length === 0 && /Directly edited/.test(el.textContent);
}));
check("direct-edited block stays editable (re-committable)", await page.evaluate(() => {
  const el = document.querySelector('[data-id="p_1"]');
  return el && !el.classList.contains("pm-readonly");
}));

// Patch-not-rebuild: the caret survived the commit (no full re-render), so
// typing continues in the SAME block right where we left off.
console.log("→ caret preserved across commit");
await page.keyboard.type(" CARET-OK");
check("caret preserved across commit (typed text lands in same block)", await page.evaluate(() =>
  /Directly edited legend\. CARET-OK/.test(document.querySelector('[data-id="p_1"]')?.textContent || "")));

// And a tracked commit deep in the document doesn't scroll us back to the top.
// (Commit via keyboard so the test doesn't auto-scroll a toolbar button into view.)
console.log("→ scroll preserved across a tracked commit");
await page.click("#mode-suggesting");
const lastP = page.locator('.ProseMirror p[data-id]').last();
await lastP.click({ clickCount: 3 });
await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
await page.keyboard.type("Tail edit.");
await page.waitForFunction(() => /Saving/.test(document.querySelector("#commit").textContent), null, { timeout: 5000 });
const scrollBefore = await page.evaluate(() => window.scrollY);
await page.keyboard.press("Control+Enter");
await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit").textContent), null, { timeout: 15000 });
const scrollAfter = await page.evaluate(() => window.scrollY);
check(`scroll preserved across commit (${scrollBefore}→${scrollAfter})`, scrollBefore > 100 && Math.abs(scrollAfter - scrollBefore) < 80);

// 3. Export.
const [dl] = await Promise.all([
  page.waitForEvent("download", { timeout: 15000 }),
  page.click("#export"),
]);
const fs = await import("node:fs");
const path = await dl.path();
check("export is a .docx", dl.suggestedFilename().endsWith(".docx"));
check("export is a zip (PK)", path && fs.readFileSync(path).subarray(0, 2).toString("latin1") === "PK");

check("no console/page errors", errors.length === 0);
if (errors.length) console.log("  errors:", errors.slice(0, 5));
await browser.close();
console.log(`\nEDITOR: ${failures === 0 ? "ALL PASSED" : failures + " FAILED"}`);
process.exit(failures === 0 ? 0 : 1);
