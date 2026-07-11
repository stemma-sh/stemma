// Re-editing a paragraph must NEVER accept its prior pending tracked change.
// Edit 1, commit, then edit 2 in the SAME paragraph: edit 1's redline must
// survive (not get baked in), and the rail shows BOTH suggestions.
import { chromium } from "playwright";

const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";
let pass = 0, fail = 0;
const check = (n, ok) => { console.log(`${ok ? "PASS" : "FAIL"}  ${n}`); ok ? pass++ : fail++; };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
const errs = [];
page.on("pageerror", (e) => errs.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") errs.push(m.text()); });

await page.goto(BASE, { waitUntil: "networkidle" });
await page.locator("#samples button", { hasText: "Simple text" }).click();
await page.waitForFunction(() => /Opened/.test(document.querySelector("#status").textContent), null, { timeout: 15000 });
await page.waitForTimeout(400);

const sel = (w) => page.evaluate((word) => {
  const tw = document.createTreeWalker(document.querySelector(".ProseMirror"), NodeFilter.SHOW_TEXT);
  let n; while ((n = tw.nextNode())) { const i = n.textContent.indexOf(word); if (i >= 0) {
    const r = document.createRange(); r.setStart(n, i); r.setEnd(n, i + word.length);
    const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); return; } }
}, w);
const saved = () => page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit")?.textContent || ""), null, { timeout: 10000 });

// Edit 1: test -> EXAM, commit.
await sel("test"); await page.keyboard.type("EXAM");
await page.keyboard.press("Control+Enter"); await saved(); await page.waitForTimeout(300);
check("after edit 1: del 'test' + ins 'EXAM'",
  (await page.locator(".ProseMirror del").allTextContents()).join("").includes("test") &&
  (await page.locator(".ProseMirror ins").allTextContents()).join("").includes("EXAM"));

// Edit 2: baz -> QUUX in the SAME paragraph, commit.
await sel("baz"); await page.keyboard.type("QUUX");
await page.keyboard.press("Control+Enter"); await saved(); await page.waitForTimeout(300);

const dels = (await page.locator(".ProseMirror del").allTextContents()).join("|");
const inss = (await page.locator(".ProseMirror ins").allTextContents()).join("|");
check("re-edit PRESERVES edit 1's deletion (del 'test' still there)", dels.includes("test"));
check("re-edit PRESERVES edit 1's insertion (ins 'EXAM' still there)", inss.includes("EXAM"));
check("re-edit adds the new change (del 'baz' + ins 'QUUX')", dels.includes("baz") && inss.includes("QUUX"));
check("rail shows BOTH suggestions (2 cards)", (await page.locator(".suggest-card").count()) === 2);
check("no console/page errors", errs.length === 0);
if (errs.length) console.log(errs.slice(0, 3).join("\n"));
console.log(`\nREEDIT: ${fail === 0 ? "ALL PASSED" : fail + " FAILED"}`);
await browser.close();
process.exit(fail === 0 ? 0 : 1);
