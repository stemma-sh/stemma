// LIVE tracked changes: in Suggesting mode the redline appears AS YOU TYPE
// (insertion green, deletion struck — nothing disappears-then-reappears), and the
// engine commit swaps the provisional marks (rev:null) for authoritative ones
// (real rev_id) seamlessly. Editing mode stays plain.
import { chromium } from "playwright";

const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";
let pass = 0, fail = 0;
const check = (n, ok) => { console.log(`${ok ? "PASS" : "FAIL"}  ${n}`); ok ? pass++ : fail++; };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
const errs = [];
page.on("pageerror", (e) => errs.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") errs.push(m.text()); });

const open = async () => {
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.locator("#samples button", { hasText: "Simple text" }).click();
  await page.waitForFunction(() => /Opened/.test(document.querySelector("#status").textContent), null, { timeout: 15000 });
  await page.waitForTimeout(400);
};
const selectWord = (w) => page.evaluate((word) => {
  const tw = document.createTreeWalker(document.querySelector(".ProseMirror"), NodeFilter.SHOW_TEXT);
  let n; while ((n = tw.nextNode())) { const i = n.textContent.indexOf(word); if (i >= 0) {
    const r = document.createRange(); r.setStart(n, i); r.setEnd(n, i + word.length);
    const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); return; } }
}, w);

await open();
// 1. AS-YOU-TYPE INSERTION — ins appears WITHOUT waiting for autosave.
await page.locator(".ProseMirror").first().click();
await page.keyboard.press("End");
await page.keyboard.type(" HELLO");
await page.waitForSelector(".ProseMirror ins", { timeout: 350 }).catch(() => {});
check("insertion shows ins as-you-type (<350ms, before autosave)", (await page.locator(".ProseMirror ins").allTextContents()).join("").includes("HELLO"));

// 2. AS-YOU-TYPE DELETION keeps the struck text visible.
await open();
await selectWord("test");
await page.keyboard.press("Backspace");
await page.waitForSelector(".ProseMirror del", { timeout: 350 }).catch(() => {});
check("deletion shows struck del as-you-type", (await page.locator(".ProseMirror del").allTextContents()).join("").includes("test"));
check("deleted text stays visible (struck, not removed)", (await page.locator(".ProseMirror").first().textContent()).includes("test"));

// 3. TYPE-OVER selection → del(old) + ins(new), then caret continues after the insertion.
await open();
await selectWord("test");
await page.keyboard.type("EXAM");
await page.waitForTimeout(150);
check("type-over → del 'test'", (await page.locator(".ProseMirror del").allTextContents()).join("").includes("test"));
check("type-over → ins 'EXAM'", (await page.locator(".ProseMirror ins").allTextContents()).join("").includes("EXAM"));
await page.keyboard.type("!");
await page.waitForTimeout(150);
check("caret lands after the insertion (EXAM! ordered in ins)", (await page.locator(".ProseMirror ins").allTextContents()).join("").includes("EXAM!"));

// 4. COMMIT SEAMLESSNESS — provisional (no rev) → authoritative (rev set), redline never vanishes.
await open();
await selectWord("test"); await page.keyboard.type("EXAM");
const provRev = await page.locator(".ProseMirror ins").first().getAttribute("data-rev");
await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit")?.textContent || ""), null, { timeout: 8000 }).catch(() => {});
await page.waitForTimeout(300);
const authRev = await page.locator(".ProseMirror ins").first().getAttribute("data-rev");
check("commit: provisional ins had no rev", !provRev);
check("commit: authoritative ins now has a rev (seamless swap)", !!authRev && authRev !== "");
check("commit: redline still present (EXAM)", (await page.locator(".ProseMirror ins").allTextContents()).join("").includes("EXAM"));

// 5. EDITING MODE = no redline (direct edits stay plain).
await open();
await page.locator("#mode-editing").click();
await page.locator(".ProseMirror").first().click(); await page.keyboard.press("End"); await page.keyboard.type(" PLAIN");
await page.waitForTimeout(200);
check("Editing mode: no ins/del marks (direct edit)", (await page.locator(".ProseMirror ins").count()) === 0 && (await page.locator(".ProseMirror del").count()) === 0);

check("no console/page errors", errs.length === 0);
if (errs.length) console.log(errs.slice(0, 3).join("\n"));
console.log(`\nLIVE-TRACK: ${fail === 0 ? "ALL PASSED" : fail + " FAILED"}`);
await browser.close();
process.exit(fail === 0 ? 0 : 1);
