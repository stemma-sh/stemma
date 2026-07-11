// Editable table cells: selection, live suggesting-redline, bold, per-cell commit,
// and accept/reject all work INSIDE a cell like a body paragraph — and an in-cell
// edit never leaks a body structural op.
import { chromium } from "playwright";

const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";
let pass = 0, fail = 0;
const check = (n, ok) => { console.log(`${ok ? "PASS" : "FAIL"}  ${n}`); ok ? pass++ : fail++; };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
const errs = [];
page.on("pageerror", (e) => errs.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") errs.push("console:" + m.text()); });

await page.goto(BASE, { waitUntil: "networkidle" });
await page.locator("#samples button", { hasText: "Table" }).click();
await page.waitForFunction(() => /Opened/.test(document.querySelector("#status").textContent), null, { timeout: 15000 });
await page.waitForTimeout(500);

const CELL = '.pm-table td[data-row="0"][data-col="0"]';
const selectWordInCell = (w) => page.evaluate(({ CELL, w }) => {
  const td = document.querySelector(CELL);
  const tw = document.createTreeWalker(td, NodeFilter.SHOW_TEXT);
  let n; while ((n = tw.nextNode())) { const i = n.textContent.indexOf(w); if (i >= 0) {
    const r = document.createRange(); r.setStart(n, i); r.setEnd(n, i + w.length);
    const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); return; } }
}, { CELL, w });

const bodyBefore = await page.locator(".ProseMirror > p[data-id]").count();
const rowsBefore = await page.locator(".pm-table tr").count();
const cellsBefore = await page.locator(".pm-table td").count();

// 1. Selection works inside a cell.
await selectWordInCell("test");
check("select a word in a cell", (await page.evaluate(() => String(window.getSelection()))).trim() === "test");

// 2. Live suggesting-redline (type over the selected word).
await page.keyboard.type("EXAM");
await page.waitForTimeout(200);
check("type-over in cell → del 'test'", (await page.locator(".pm-table del").allTextContents()).join("").includes("test"));
check("type-over in cell → ins 'EXAM'", (await page.locator(".pm-table ins").allTextContents()).join("").includes("EXAM"));

// 3. No structural leak — body + table structure intact.
check("body paragraph count unchanged", (await page.locator(".ProseMirror > p[data-id]").count()) === bodyBefore);
check("table rows/cells unchanged",
  (await page.locator(".pm-table tr").count()) === rowsBefore && (await page.locator(".pm-table td").count()) === cellsBefore);

// 4. Enter inside a cell is guarded (no body paragraph created).
await page.locator(CELL).first().click();
await page.keyboard.press("Enter");
await page.waitForTimeout(150);
check("Enter in cell does NOT create a body paragraph", (await page.locator(".ProseMirror > p[data-id]").count()) === bodyBefore);

// 5. Bold applies inside a cell.
await selectWordInCell("EXAM");
await page.keyboard.press("Control+b");
await page.waitForTimeout(150);
check("bold applies in cell (td strong)", (await page.locator(".pm-table td strong").count()) >= 1);

// 6. Commit → the cell change round-trips.
await page.keyboard.press("Control+Enter");
await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit")?.textContent || ""), null, { timeout: 10000 }).catch(() => {});
await page.waitForTimeout(500);
check("after commit: cell redline persists", (await page.locator(".pm-table ins").allTextContents()).join("").includes("EXAM"));

// 7. Accept the cell change from the rail (targets the cell-paragraph id).
const card = page.locator(".suggest-card").first();
if (await card.count()) {
  await card.locator(".accept").first().click();
  await page.waitForTimeout(900);
  check("accept cell change → baked in (no redline, EXAM present)",
    (await page.locator(".pm-table del").count()) === 0 &&
    (await page.locator(".pm-table ins").count()) === 0 &&
    (await page.locator(CELL).first().textContent()).includes("EXAM"));
} else {
  check("a suggestion card appears for the cell change", false);
}

check("no console/page errors", errs.length === 0);
if (errs.length) console.log(errs.slice(0, 4).join("\n"));
console.log(`\nTABLE-CELL: ${fail === 0 ? "ALL PASSED" : fail + " FAILED"}`);
await browser.close();
process.exit(fail === 0 ? 0 : 1);
