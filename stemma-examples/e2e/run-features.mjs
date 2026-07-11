// Feature suite: exercises every AUTHORING capability and asserts each one
// ROUND-TRIPS through the engine (survives a fresh /rich read), not just a local
// DOM change. Run via run.sh (which starts the server) or with BASE set.
import { chromium } from "playwright";
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const BASE = process.env.BASE || "http://127.0.0.1:3137";
const EDITOR = BASE + "/editor/";
let failures = 0;
const log = [];
const check = (name, cond) => { if (!cond) failures++; log.push(`${cond ? "PASS" : "FAIL"}  ${name}`); };

async function openSample(page, label) {
  await page.goto(EDITOR, { waitUntil: "networkidle" });
  await page.locator("#samples button", { hasText: label }).click();
  await page.waitForFunction(() => /Opened/.test(document.querySelector("#status").textContent), null, { timeout: 20000 });
  await page.click("#mode-editing");
  await page.waitForTimeout(300);
}
const synced = (page) => page.waitForFunction(() => /All changes saved/.test(document.querySelector("#commit").textContent), null, { timeout: 20000 });
// Select the first occurrence of `word` in the first body paragraph.
const selectWord = (page, word) => page.evaluate((w) => {
  const p = document.querySelector(".ProseMirror p[data-id]");
  const walk = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
  let n; while ((n = walk.nextNode())) { const i = n.textContent.indexOf(w); if (i >= 0) { const r = document.createRange(); r.setStart(n, i); r.setEnd(n, i + w.length); const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); break; } }
}, word);

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1300, height: 800 } });
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));

try {
  // ── Run formatting: text color round-trips as a real run color ──────────────
  await openSample(page, "Simple text");
  await selectWord(page, "test");
  await page.evaluate(() => { const c = document.getElementById("fmt-color"); c.value = "#ff0000"; c.dispatchEvent(new Event("input", { bubbles: true })); });
  await synced(page);
  const red = await page.evaluate(() => { const s = [...document.querySelectorAll(".ProseMirror p .pm-run")].find((x) => x.textContent.includes("test")); return s ? getComputedStyle(s).color : null; });
  check("run color round-trips (red)", red === "rgb(255, 0, 0)");

  // ── Bold in suggesting mode is a SURGICAL formatting change, not a whole-para redline ──
  await openSample(page, "Simple text");
  await page.click("#mode-suggesting"); // openSample defaults to editing; this test needs tracked changes
  await page.waitForTimeout(200);
  await selectWord(page, "now");
  await page.click("#fmt-bold");
  await synced(page); await page.waitForTimeout(300);
  check("bold one word → that word renders bold",
    (await page.locator(".ProseMirror p strong").allTextContents()).join("").includes("now"));
  check("bold one word → NO whole-paragraph redline (no del/ins)",
    (await page.locator(".ProseMirror del").count()) === 0 && (await page.locator(".ProseMirror ins").count()) === 0);
  await page.waitForSelector(".ProseMirror .pm-fmtchange", { timeout: 5000 }).catch(() => {});
  check("bold one word → an inline formatting-change indicator on the word",
    (await page.locator(".ProseMirror .pm-fmtchange").allTextContents()).join("").includes("now"));

  // ── Un-bold is also a surgical tracked change (full add + remove support) ────
  await page.waitForSelector(".suggest-card .accept", { timeout: 6000 }).catch(() => {});
  const acceptBtn = page.locator(".suggest-card .accept").first();
  if (await acceptBtn.count()) { await acceptBtn.click(); await synced(page); await page.waitForTimeout(400); }
  await selectWord(page, "now");
  await page.click("#fmt-bold"); // toggle OFF
  await synced(page); await page.waitForTimeout(400);
  check("un-bold one word → that word is no longer bold",
    !(await page.locator(".ProseMirror p strong").allTextContents()).join("").includes("now"));
  check("un-bold one word → NO whole-paragraph redline (no del/ins)",
    (await page.locator(".ProseMirror del").count()) === 0 && (await page.locator(".ProseMirror ins").count()) === 0);

  // ── Paragraph alignment ─────────────────────────────────────────────────────
  await page.evaluate(() => { const p = document.querySelector(".ProseMirror p[data-id]"); const r = document.createRange(); r.selectNodeContents(p); const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); });
  await page.click("#fmt-align-center");
  await synced(page);
  const align = await page.evaluate(() => getComputedStyle(document.querySelector(".ProseMirror p[data-id]")).textAlign);
  check("paragraph alignment round-trips (center)", align === "center");

  // ── Structural editing: Enter inserts a paragraph, Backspace merges ─────────
  await openSample(page, "Simple text");
  const before = await page.locator(".ProseMirror p[data-id]").count();
  await page.locator(".ProseMirror p[data-id]").first().click();
  await page.keyboard.press("End"); await page.keyboard.press("Enter"); await page.keyboard.type("A new paragraph.");
  await page.keyboard.press("Control+Enter"); await synced(page); await page.waitForTimeout(300);
  const afterInsert = await page.locator(".ProseMirror p[data-id]").count();
  check("Enter inserts a paragraph (round-trips)", afterInsert === before + 1);
  const np = page.locator(".ProseMirror p[data-id]", { hasText: "A new paragraph." }).first();
  await np.click(); await page.keyboard.press("Home"); await page.keyboard.press("Backspace");
  await page.keyboard.press("Control+Enter"); await synced(page); await page.waitForTimeout(300);
  check("Backspace merges a paragraph (round-trips)", (await page.locator(".ProseMirror p[data-id]").count()) === before);

  // ── Table renders as real editor content (cell paragraph nodes) ─────────────
  await openSample(page, "Table");
  check("table renders as PM content (cell paragraph nodes)",
    (await page.locator('.pm-table td[data-row="0"][data-col="0"] p[data-id]').count()) >= 1);
  const cellText = (await page.locator('.pm-table td[data-row="0"][data-col="0"]').first().textContent()).trim();
  check("cell text renders", cellText.includes("test"));

  // ── List: toggle a plain paragraph into a bullet ────────────────────────────
  await openSample(page, "SAFE agreement");
  await page.evaluate(() => { const p = document.querySelector('.ProseMirror p[data-id="p_1"]'); const r = document.createRange(); r.selectNodeContents(p); const s = window.getSelection(); s.removeAllRanges(); s.addRange(r); });
  await page.click("#fmt-bullet"); await synced(page); await page.waitForTimeout(400);
  check("bullet toggle adds a list marker (SetNumbering)", (await page.locator('.ProseMirror p[data-id="p_1"] .pm-num').count()) === 1);

  // ── Hyperlink authoring ─────────────────────────────────────────────────────
  await openSample(page, "Simple text");
  await selectWord(page, "test");
  await page.click("#fmt-link"); await page.waitForSelector("#inline-popover:not(.hidden)", { timeout: 5000 });
  await page.fill("#popover-input", "https://example.org/x"); await page.click("#popover-ok"); await page.waitForTimeout(150);
  await page.keyboard.press("Control+Enter"); await synced(page); await page.waitForTimeout(400);
  check("hyperlink round-trips as <a href>", (await page.locator('.ProseMirror a.pm-link[href="https://example.org/x"]', { hasText: "test" }).count()) >= 1);

  // ── Image insertion (InsertImage) ───────────────────────────────────────────
  const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKUlEQVR4nO3OMQEAAAjDMOZf9CCBR1Jg0nbkWg8AAAAAAAAAAAAAAB4XGzwBgUNRAaUAAAAASUVORK5CYII=", "base64");
  const pngPath = join(tmpdir(), "stemma-e2e-image.png");
  writeFileSync(pngPath, png);
  await openSample(page, "Simple text");
  await page.locator(".ProseMirror p[data-id]").first().click();
  await page.setInputFiles("#image-file", pngPath);
  await synced(page); await page.waitForTimeout(500);
  const imgSrc = await page.locator(".ProseMirror img.pm-img").first().getAttribute("src").catch(() => null);
  check("image insert round-trips as <img data:>", !!imgSrc && imgSrc.startsWith("data:"));
  check("exactly one image after insert", (await page.locator(".ProseMirror img.pm-img").count()) === 1);
  // Resize the image — the reconcile must NOT duplicate it.
  await page.locator(".ProseMirror img.pm-img").first().click();
  await page.waitForSelector("#image-bar:not(.hidden)", { timeout: 3000 });
  await page.click("#image-bigger");
  await synced(page); await page.waitForTimeout(500);
  check("exactly one image after resize (no duplication)", (await page.locator(".ProseMirror img.pm-img").count()) === 1);

  // ── Delete images via the dedicated delete_image op ─────────────────────────
  // Editing (direct) mode: deleting multiple images removes them and KEEPS their
  // paragraphs (a per-drawing op, not a paragraph delete), with no StaleEdit revert.
  await openSample(page, "Simple text"); // editing mode
  await page.locator(".ProseMirror p[data-id]").first().click();
  await page.keyboard.press("End"); await page.keyboard.press("Enter"); await page.keyboard.press("Enter");
  await synced(page); await page.waitForTimeout(300);
  await page.locator(".ProseMirror p[data-id]").nth(1).click();
  await page.setInputFiles("#image-file", pngPath); await synced(page); await page.waitForTimeout(300);
  await page.locator(".ProseMirror p[data-id]").nth(2).click();
  await page.setInputFiles("#image-file", pngPath); await synced(page); await page.waitForTimeout(300);
  check("two images inserted", (await page.locator(".ProseMirror img.pm-img").count()) === 2);
  const blocksBeforeDel = await page.locator(".ProseMirror > *[data-id]").count();
  await page.locator(".ProseMirror img.pm-img").nth(1).click(); await page.keyboard.press("Backspace");
  await page.locator(".ProseMirror img.pm-img").nth(0).click(); await page.keyboard.press("Backspace");
  await synced(page); await page.waitForTimeout(500);
  check("Editing: deleting multiple images removes them (no revert)",
    (await page.locator(".ProseMirror img.pm-img").count()) === 0);
  check("Editing: image delete keeps the paragraph (block count unchanged)",
    (await page.locator(".ProseMirror > *[data-id]").count()) === blocksBeforeDel);
  check("no stale-edit revert after deleting images",
    !/out-of-date|reverted/.test(await page.evaluate(() => document.querySelector("#status")?.textContent || "")));

  // Suggesting mode: deleting an image is a TRACKED deletion — the image stays
  // visible but renders struck (wrapped in <del>), accept/reject-able.
  await openSample(page, "Images"); // a standalone Normal image
  await page.click("#mode-suggesting"); await page.waitForTimeout(200);
  check("Images sample has one image", (await page.locator(".ProseMirror img.pm-img").count()) === 1);
  await page.locator(".ProseMirror img.pm-img").first().click(); await page.keyboard.press("Backspace");
  await synced(page); await page.waitForTimeout(500);
  check("Suggesting: deleted image renders struck (del)",
    (await page.locator(".ProseMirror del img.pm-img").count()) === 1);
  check("Suggesting: the image is tombstoned, not removed",
    (await page.locator(".ProseMirror img.pm-img").count()) === 1);
  // Accepting the deletion card from the rail removes the image (server resolve +
  // /rich reconcile; the optimistic pass is a no-op for the drawing but harmless).
  await page.waitForSelector(".suggest-card .accept", { timeout: 6000 }).catch(() => {});
  const imgAccept = page.locator(".suggest-card .accept").first();
  if (await imgAccept.count()) { await imgAccept.click(); await synced(page); await page.waitForTimeout(500); }
  check("Suggesting: accepting the deletion card removes the image",
    (await page.locator(".ProseMirror img.pm-img").count()) === 0);

  // ── Comments: author + sidebar + highlighted span ───────────────────────────
  await openSample(page, "Simple text");
  await selectWord(page, "test");
  await page.click("#fmt-comment"); await page.waitForSelector("#inline-popover:not(.hidden)", { timeout: 5000 });
  await page.fill("#popover-textarea", "Please clarify."); await page.click("#popover-ok");
  await synced(page).catch(() => {});
  await page.waitForTimeout(500);
  const comm = await page.evaluate(() => {
    const cards = document.querySelectorAll(".comment-card, #comments [data-comment-id]");
    const span = document.querySelector(".ProseMirror .pm-comment");
    return { cards: cards.length, body: cards[0]?.textContent.includes("Please clarify"), spanText: span?.textContent };
  });
  check("comment appears in the sidebar", comm.cards >= 1 && comm.body);
  check("commented span is highlighted", comm.spanText === "test");
} catch (err) {
  failures++;
  log.push(`FAIL  uncaught: ${err.message}`);
}

check("no console/page errors", errors.length === 0);
if (errors.length) log.push("  errors: " + errors.slice(0, 3).join(" | "));
await browser.close();
console.log(log.join("\n"));
console.log(`\nFEATURES: ${failures === 0 ? "ALL PASSED" : failures + " FAILED"}`);
process.exit(failures === 0 ? 0 : 1);
