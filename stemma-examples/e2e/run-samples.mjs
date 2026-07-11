import { chromium } from "playwright";
const BASE = (process.env.BASE || "http://127.0.0.1:3137") + "/editor/";

let failures = 0;
const check = (name, cond) => { console.log(`${cond ? "PASS" : "FAIL"}  ${name}`); if (!cond) failures++; };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
const errors = [];
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
page.on("pageerror", (e) => errors.push(String(e)));

await page.goto(BASE, { waitUntil: "networkidle" });
const btns = page.locator("#samples button");
check("5 sample buttons present", (await btns.count()) === 5);

const samples = ["SAFE agreement", "Simple text", "Table", "Images", "Equations"];
for (const label of samples) {
  console.log(`→ load sample: ${label}`);
  await page.locator("#samples button", { hasText: label }).click();
  await page.waitForFunction((l) => {
    const s = document.querySelector("#status");
    return s.classList.contains("ok") && /Opened/.test(s.textContent) && s.textContent.includes(l);
  }, label, { timeout: 15000 });
  const blocks = await page.locator(".ProseMirror [data-id]").count();
  check(`${label}: loaded with blocks (${blocks})`, blocks > 0);
}

// The Table sample must surface its table as a read-only placeholder.
await page.locator("#samples button", { hasText: "Table" }).click();
await page.waitForFunction(() => /Opened/.test(document.querySelector("#status").textContent) && document.querySelector("#status").textContent.includes("Table"), null, { timeout: 15000 });
check("Table sample renders a real table", (await page.locator(".pm-table").count()) >= 1);

check("no console/page errors across all samples", errors.length === 0);
if (errors.length) console.log("  errors:", errors.slice(0, 5));
await browser.close();
console.log(`\nSAMPLES: ${failures === 0 ? "ALL PASSED" : failures + " FAILED"}`);
process.exit(failures === 0 ? 0 : 1);
