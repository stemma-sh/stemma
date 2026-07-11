// OMML → MathML conversion + MathJax rendering.
//
// A self-contained DOM
// traversal that converts Office MathML (OMML, w:oMath) to W3C MathML, then
// MathJax (loaded on demand from a CDN) typesets it to SVG. No build step.

const MATH_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const MATHML_NS = "http://www.w3.org/1998/Math/MathML";

export function ommlToMathml(omml) {
  try {
    const parser = new DOMParser();
    const doc = parser.parseFromString(omml, "application/xml");
    const parseError = doc.querySelector("parsererror");
    if (parseError) return { ok: false, error: `OMML parse error: ${parseError.textContent}`, omml };
    const align = extractAlign(doc);
    const mathml = convertDocument(doc);
    return { ok: true, mathml, align };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err), omml };
  }
}

function extractAlign(doc) {
  const root = doc.documentElement;
  if (localName(root) !== "oMathPara") return "center";
  const paraPr = lastMathChild(root, "oMathParaPr");
  if (!paraPr) return "center";
  const jcVal = propValLower(paraPr, "jc");
  if (jcVal === "left") return "left";
  if (jcVal === "right") return "right";
  return "center";
}

function localName(el) { return el.localName ?? el.nodeName.split(":").pop(); }
function mathChildren(parent, name) {
  const result = [];
  for (let i = 0; i < parent.children.length; i++) { const c = parent.children[i]; if (localName(c) === name) result.push(c); }
  return result;
}
function lastMathChild(parent, name) { const c = mathChildren(parent, name); return c.length > 0 ? c[c.length - 1] : null; }
function propVal(prEl, childName) {
  if (!prEl) return null;
  const child = lastMathChild(prEl, childName);
  if (!child) return null;
  return child.getAttributeNS(MATH_NS, "val") ?? child.getAttribute("m:val") ?? child.getAttribute("val");
}
function propValLower(prEl, childName) { const v = propVal(prEl, childName); return v ? v.toLowerCase() : null; }
function isOn(val) { return val === "on" || val === "1" || val === "true"; }

function convertDocument(doc) {
  const root = doc.documentElement;
  const name = localName(root);
  if (name === "oMathPara" || name === "oMath") return `<math xmlns="${MATHML_NS}">${convertChildren(root)}</math>`;
  return `<math xmlns="${MATHML_NS}">${convertElement(root)}</math>`;
}
function convertChildren(parent) { let out = ""; for (let i = 0; i < parent.children.length; i++) out += convertElement(parent.children[i]); return out; }

function convertElement(el) {
  const name = localName(el);
  switch (name) {
    case "oMathParaPr": case "fPr": case "sSubPr": case "sSupPr": case "sSubSupPr": case "sPrePr":
    case "naryPr": case "dPr": case "accPr": case "barPr": case "radPr": case "groupChrPr": case "limLowPr":
    case "limUppPr": case "phantPr": case "borderBoxPr": case "funcPr": case "mPr": case "eqArrPr":
    case "ctrlPr": case "argPr": case "rPr": case "mcPr": return "";
    case "oMathPara": return convertChildren(el);
    case "oMath": return convertChildren(el);
    case "ins": case "del": return "";
    case "r": return convertRun(el);
    case "f": return convertFraction(el);
    case "sSup": return convertSSup(el);
    case "sSub": return convertSSub(el);
    case "sSubSup": return convertSSubSup(el);
    case "sPre": return convertSPre(el);
    case "nary": return convertNary(el);
    case "d": return convertDelimiter(el);
    case "acc": return convertAcc(el);
    case "bar": return convertBar(el);
    case "rad": return convertRad(el);
    case "limLow": return convertLimLow(el);
    case "limUpp": return convertLimUpp(el);
    case "func": return convertFunc(el);
    case "m": return convertMatrix(el);
    case "eqArr": return convertEqArr(el);
    case "groupChr": return convertGroupChr(el);
    case "phant": return convertPhant(el);
    case "borderBox": return convertBorderBox(el);
    case "e": case "num": case "den": case "lim": case "sup": case "sub": case "deg": case "fName":
      return `<mrow>${convertChildren(el)}</mrow>`;
    case "mr": return `<mtr>${convertChildren(el)}</mtr>`;
    case "mc": return "";
    default: return convertChildren(el);
  }
}

function convertRun(el) {
  const rPr = lastMathChild(el, "rPr");
  const tEl = lastMathChild(el, "t");
  if (!tEl) return "";
  const text = tEl.textContent ?? "";
  if (text === "") return "";
  if (isOn(propValLower(rPr, "nor"))) return `<mtext>${escapeXml(text)}</mtext>`;
  return classifyMathText(text, rPr);
}

function classifyMathText(text, rPr) {
  let out = "", i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (isOperator(ch)) { out += `<mo>${escapeXml(ch)}</mo>`; i++; }
    else if (isDigit(ch)) {
      let num = ch; i++;
      while (i < text.length) {
        if (isDigit(text[i])) { num += text[i]; i++; }
        else if ((text[i] === "." || text[i] === ",") && i + 1 < text.length && isDigit(text[i + 1])) { num += text[i] + text[i + 1]; i += 2; }
        else break;
      }
      out += `<mn>${escapeXml(num)}</mn>`;
    } else {
      const variant = mathVariant(rPr);
      out += variant ? `<mi mathvariant="${variant}">${escapeXml(ch)}</mi>` : `<mi>${escapeXml(ch)}</mi>`;
      i++;
    }
  }
  return out;
}

function convertFraction(el) {
  const type = propValLower(lastMathChild(el, "fPr"), "type");
  const num = lastMathChild(el, "num"), den = lastMathChild(el, "den");
  const numMml = num ? `<mrow>${convertChildren(num)}</mrow>` : "<mrow/>";
  const denMml = den ? `<mrow>${convertChildren(den)}</mrow>` : "<mrow/>";
  if (type === "lin") return `<mrow>${numMml}<mo>/</mo>${denMml}</mrow>`;
  if (type === "skw") return `<mfrac bevelled="true">${numMml}${denMml}</mfrac>`;
  if (type === "nobar") return `<mfrac linethickness="0pt">${numMml}${denMml}</mfrac>`;
  return `<mfrac>${numMml}${denMml}</mfrac>`;
}
function convertSSup(el) { const b = lastMathChild(el, "e"), s = lastMathChild(el, "sup"); return `<msup><mrow>${b ? convertChildren(b) : ""}</mrow><mrow>${s ? convertChildren(s) : ""}</mrow></msup>`; }
function convertSSub(el) { const b = lastMathChild(el, "e"), s = lastMathChild(el, "sub"); return `<msub><mrow>${b ? convertChildren(b) : ""}</mrow><mrow>${s ? convertChildren(s) : ""}</mrow></msub>`; }
function convertSSubSup(el) { const b = lastMathChild(el, "e"), sb = lastMathChild(el, "sub"), sp = lastMathChild(el, "sup"); return `<msubsup><mrow>${b ? convertChildren(b) : ""}</mrow><mrow>${sb ? convertChildren(sb) : ""}</mrow><mrow>${sp ? convertChildren(sp) : ""}</mrow></msubsup>`; }
function convertSPre(el) { const b = lastMathChild(el, "e"), sb = lastMathChild(el, "sub"), sp = lastMathChild(el, "sup"); return `<mmultiscripts><mrow>${b ? convertChildren(b) : ""}</mrow><mprescripts/><mrow>${sb ? convertChildren(sb) : ""}</mrow><mrow>${sp ? convertChildren(sp) : ""}</mrow></mmultiscripts>`; }

function convertNary(el) {
  const pr = lastMathChild(el, "naryPr");
  const chr = propVal(pr, "chr") ?? "∫";
  const limLoc = propValLower(pr, "limLoc") ?? "subsup";
  const subHide = isOn(propValLower(pr, "subHide")), supHide = isOn(propValLower(pr, "supHide"));
  const subEl = lastMathChild(el, "sub"), supEl = lastMathChild(el, "sup"), baseEl = lastMathChild(el, "e");
  const opMml = `<mo>${escapeXml(chr)}</mo>`;
  const subMml = subEl ? `<mrow>${convertChildren(subEl)}</mrow>` : "<mrow/>";
  const supMml = supEl ? `<mrow>${convertChildren(supEl)}</mrow>` : "<mrow/>";
  const baseMml = baseEl ? `<mrow>${convertChildren(baseEl)}</mrow>` : "<mrow/>";
  let operatorPart;
  if (!subHide && !supHide) operatorPart = limLoc === "subsup" ? `<msubsup>${opMml}${subMml}${supMml}</msubsup>` : `<munderover>${opMml}${subMml}${supMml}</munderover>`;
  else if (subHide && !supHide) operatorPart = limLoc === "subsup" ? `<msup>${opMml}${supMml}</msup>` : `<mover>${opMml}${supMml}</mover>`;
  else if (!subHide && supHide) operatorPart = limLoc === "subsup" ? `<msub>${opMml}${subMml}</msub>` : `<munder>${opMml}${subMml}</munder>`;
  else operatorPart = opMml;
  return operatorPart + baseMml;
}

function convertDelimiter(el) {
  const pr = lastMathChild(el, "dPr");
  const open = propVal(pr, "begChr") ?? "(", close = propVal(pr, "endChr") ?? ")", sep = propVal(pr, "sepChr") ?? "|";
  const inner = mathChildren(el, "e").map((e) => `<mrow>${convertChildren(e)}</mrow>`).join("");
  let attrs = "";
  if (open !== "(") attrs += ` open="${escapeXml(open)}"`;
  if (close !== ")") attrs += ` close="${escapeXml(close)}"`;
  if (sep !== ",") attrs += ` separators="${escapeXml(sep)}"`;
  return `<mfenced${attrs}>${inner}</mfenced>`;
}
function convertAcc(el) { const chr = propVal(lastMathChild(el, "accPr"), "chr") ?? "̂"; const b = lastMathChild(el, "e"); return `<mover accent="true"><mrow>${b ? convertChildren(b) : ""}</mrow><mo>${escapeXml(chr)}</mo></mover>`; }
function convertBar(el) { const pos = propValLower(lastMathChild(el, "barPr"), "pos") ?? "top"; const b = lastMathChild(el, "e"); const m = `<mrow>${b ? convertChildren(b) : ""}</mrow>`; return pos === "bot" ? `<munder>${m}<mo>̲</mo></munder>` : `<mover>${m}<mo>¯</mo></mover>`; }
function convertRad(el) {
  const degHide = isOn(propValLower(lastMathChild(el, "radPr"), "degHide"));
  const b = lastMathChild(el, "e"), deg = lastMathChild(el, "deg");
  const baseMml = b ? convertChildren(b) : "";
  if (degHide) return `<msqrt>${baseMml}</msqrt>`;
  return `<mroot><mrow>${baseMml}</mrow>${deg ? `<mrow>${convertChildren(deg)}</mrow>` : "<mrow/>"}</mroot>`;
}
function convertLimLow(el) { const b = lastMathChild(el, "e"), l = lastMathChild(el, "lim"); return `<munder><mrow>${b ? convertChildren(b) : ""}</mrow><mrow>${l ? convertChildren(l) : ""}</mrow></munder>`; }
function convertLimUpp(el) { const b = lastMathChild(el, "e"), l = lastMathChild(el, "lim"); return `<mover><mrow>${b ? convertChildren(b) : ""}</mrow><mrow>${l ? convertChildren(l) : ""}</mrow></mover>`; }
function convertFunc(el) { const fn = lastMathChild(el, "fName"), b = lastMathChild(el, "e"); return `<mrow><mrow>${fn ? convertChildren(fn) : ""}</mrow><mo>⁡</mo><mrow>${b ? convertChildren(b) : ""}</mrow></mrow>`; }
function convertMatrix(el) { let inner = ""; for (const row of mathChildren(el, "mr")) { const cells = mathChildren(row, "e").map((e) => `<mtd><mrow>${convertChildren(e)}</mrow></mtd>`).join(""); inner += `<mtr>${cells}</mtr>`; } return `<mtable>${inner}</mtable>`; }
function convertEqArr(el) { let inner = ""; for (const e of mathChildren(el, "e")) inner += `<mtr><mtd><mrow>${convertChildren(e)}</mrow></mtd></mtr>`; return `<mtable>${inner}</mtable>`; }
function convertGroupChr(el) {
  const pr = lastMathChild(el, "groupChrPr");
  const chr = propVal(pr, "chr") ?? "⏟", pos = propValLower(pr, "pos") ?? "bot";
  const b = lastMathChild(el, "e"); const m = `<mrow>${b ? convertChildren(b) : ""}</mrow>`, c = `<mo>${escapeXml(chr)}</mo>`;
  return pos === "top" ? `<mover>${m}${c}</mover>` : `<munder>${m}${c}</munder>`;
}
function convertPhant(el) {
  const pr = lastMathChild(el, "phantPr");
  const b = lastMathChild(el, "e"); const m = `<mrow>${b ? convertChildren(b) : ""}</mrow>`;
  if (!isOn(propValLower(pr, "show") ?? "on")) return `<mphantom>${m}</mphantom>`;
  const zw = isOn(propValLower(pr, "zeroWid")), za = isOn(propValLower(pr, "zeroAsc")), zd = isOn(propValLower(pr, "zeroDesc"));
  if (zw || za || zd) { let a = ""; if (zw) a += ' width="0"'; if (za) a += ' height="0"'; if (zd) a += ' depth="0"'; return `<mpadded${a}>${m}</mpadded>`; }
  return m;
}
function convertBorderBox(el) {
  const pr = lastMathChild(el, "borderBoxPr");
  const b = lastMathChild(el, "e"); const baseMml = b ? convertChildren(b) : "";
  const n = [];
  if (!isOn(propValLower(pr, "hideTop"))) n.push("top");
  if (!isOn(propValLower(pr, "hideBot"))) n.push("bottom");
  if (!isOn(propValLower(pr, "hideLeft"))) n.push("left");
  if (!isOn(propValLower(pr, "hideRight"))) n.push("right");
  if (isOn(propValLower(pr, "strikeH"))) n.push("horizontalstrike");
  if (isOn(propValLower(pr, "strikeV"))) n.push("verticalstrike");
  if (isOn(propValLower(pr, "strikeBLTR"))) n.push("updiagonalstrike");
  if (isOn(propValLower(pr, "strikeTLBR"))) n.push("downdiagonalstrike");
  return n.length === 0 ? baseMml : `<menclose notation="${n.join(" ")}">${baseMml}</menclose>`;
}

function isDigit(ch) { const c = ch.charCodeAt(0); return c >= 0x30 && c <= 0x39; }
function isOperator(ch) {
  const c = ch.charCodeAt(0);
  if ("!+-=<>()[]{}|/\\,.;:?@#&*^_~\"".includes(ch)) return true;
  if (c >= 0x2200 && c <= 0x22ff) return true;
  if (c >= 0x2a00 && c <= 0x2aff) return true;
  if (c >= 0x27c0 && c <= 0x27ef) return true;
  if (c >= 0x2980 && c <= 0x29ff) return true;
  if (c >= 0x2190 && c <= 0x21ff) return true;
  if (c >= 0x27f0 && c <= 0x27ff) return true;
  if (c >= 0x2900 && c <= 0x297f) return true;
  if (c === 0x00b1 || c === 0x00d7 || c === 0x00f7) return true;
  if (c >= 0x2061 && c <= 0x2064) return true;
  return false;
}
function mathVariant(rPr) {
  if (!rPr) return null;
  const scr = propValLower(rPr, "scr"), sty = propValLower(rPr, "sty");
  if (!scr && !sty) return null;
  if (scr === "monospace") return "monospace";
  if (scr === "double-struck") return "double-struck";
  if (scr === "sans-serif") { if (sty === "b") return "bold-sans-serif"; if (sty === "bi") return "sans-serif-bold-italic"; if (sty === "i") return "sans-serif-italic"; return "sans-serif"; }
  if (scr === "fraktur") { if (sty === "b") return "bold-fraktur"; return "fraktur"; }
  if (scr === "script") { if (sty === "b") return "bold-script"; return "script"; }
  if (sty === "b") return "bold";
  if (sty === "bi") return "bold-italic";
  if (sty === "p") return "normal";
  return null;
}
function escapeXml(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"); }

// ─── MathJax (lazy CDN load) + render ────────────────────────────────────────

let loadPromise = null;
export function ensureMathJaxLoaded() {
  if (!loadPromise) loadPromise = doLoadMathJax();
  return loadPromise;
}
async function doLoadMathJax() {
  if (window.MathJax?.startup?.promise) { await window.MathJax.startup.promise; return; }
  window.MathJax = { options: { enableMenu: false }, startup: { typeset: false }, svg: { fontCache: "global" } };
  await new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.id = "MathJax-script"; script.async = true;
    script.src = "https://cdn.jsdelivr.net/npm/mathjax@3/es5/mml-svg.js";
    script.onload = () => resolve();
    script.onerror = () => reject(new Error("Failed to load MathJax"));
    document.head.appendChild(script);
  });
  if (window.MathJax?.startup?.promise) await window.MathJax.startup.promise;
}

// Convert OMML → MathML, insert into `host`, and typeset with MathJax. Falls back
// to a small label on failure (never throws into the editor).
export async function renderEquation(omml, host) {
  try {
    const res = ommlToMathml(omml);
    if (!res.ok) throw new Error(res.error);
    const parsed = new DOMParser().parseFromString(res.mathml, "application/xml");
    if (parsed.querySelector("parsererror") || parsed.documentElement.tagName.toLowerCase() !== "math") {
      throw new Error("MathML parse error");
    }
    host.replaceChildren(document.importNode(parsed.documentElement, true));
    await ensureMathJaxLoaded();
    if (window.MathJax?.typesetPromise) await window.MathJax.typesetPromise([host]);
    if (res.align && res.align !== "center") host.style.textAlign = res.align;
  } catch (err) {
    host.textContent = "∑ equation";
    host.title = (err && err.message) || "equation";
  }
}
