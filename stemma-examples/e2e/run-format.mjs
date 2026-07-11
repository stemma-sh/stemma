import { chromium } from "playwright";
const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";
let failures = 0;
const check = (n, c) => { console.log(`${c ? "PASS" : "FAIL"}  ${n}`); if (!c) failures++; };
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
const errors = [];
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
page.on("pageerror", (e) => errors.push(String(e)));

await page.goto(BASE, { waitUntil: "networkidle" });
await page.locator("#samples button", { hasText: "Simple text" }).click();
await page.waitForFunction(() => /Opened/.test(document.querySelector("#status").textContent) && document.querySelector("#status").textContent.includes("Simple text"), null, { timeout: 15000 });
check("plain paragraph has no bold initially", (await page.locator(".ProseMirror p strong").count()) === 0);
check("bold toolbar enabled after open", !(await page.locator("#fmt-bold").isDisabled()));

// Editing (direct) mode so the result is a clean bold, not a whole-para redline.
await page.click("#mode-editing");
const para = page.locator(".ProseMirror p[data-id]").first();
await para.click({ clickCount: 3 }); // select the paragraph text
check("bold inactive before toggle", (await page.getAttribute("#fmt-bold", "aria-pressed")) === "false");

await page.click("#fmt-bold");
check("bold active after toggle (selection is bold)", (await page.getAttribute("#fmt-bold", "aria-pressed")) === "true");
await page.waitForFunction(() => /Saving/.test(document.querySelector("#commit").textContent), null, { timeout: 5000 });
check("formatting change armed the commit", /Saving/.test(await page.locator("#commit").textContent()));

await page.keyboard.press("Control+Enter"); // commit via keyboard (focus stays in editor)
await page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit").textContent), null, { timeout: 15000 });
check("bold round-tripped through the engine (renders <strong> from /rich)", (await page.locator(".ProseMirror p strong").count()) >= 1);
check("commit cleared after sync", /All changes saved/.test(await page.locator("#commit").textContent()));
check("no console/page errors", errors.length === 0);
if (errors.length) console.log("  errors:", errors.slice(0, 5));
await browser.close();
console.log(`\nFORMAT: ${failures === 0 ? "ALL PASSED" : failures + " FAILED"}`);
process.exit(failures === 0 ? 0 : 1);
