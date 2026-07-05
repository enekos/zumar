// Unit tests for the shim's XSS guards, runnable in plain Node (no DOM):
//   node www/test-guards.mjs
// Mirrors setSafeAttr/safeTag decision logic in zumar.js — keep in lockstep.

const URL_ATTRS = new Set(["href", "src", "action", "formaction", "xlink:href"]);

function attrBlocked(name, value) {
  const n = name.toLowerCase();
  if (n.startsWith("on") || n === "srcdoc") return true;
  if (URL_ATTRS.has(n)) {
    const v = value.replace(/[\u0000-\u0020]/g, "").toLowerCase();
    if (v.startsWith("javascript:") || v.startsWith("data:text/html")) return true;
  }
  return false;
}

function tagBlocked(tag) {
  return tag.toLowerCase() === "script";
}

const cases = [
  // [name, value, shouldBlock]
  ["href", "javascript:alert(1)", true],
  ["href", "java\tscript:alert(1)", true],
  ["href", "\u0000javascript:alert(1)", true],
  ["HREF", " JavaScript:alert(1)", true],
  ["src", "data:text/html,<script>x</script>", true],
  ["formaction", "javascript:x", true],
  ["onclick", "x()", true],
  ["onerror", "x()", true],
  ["ONLOAD", "x()", true],
  ["srcdoc", "<script>", true],
  ["href", "https://example.com", false],
  ["href", "/relative/path", false],
  ["href", "mailto:a@b.c", false],
  ["href", "#anchor", false],
  ["src", "data:image/png;base64,AAAA", false],
  ["class", "javascript:not-a-url-attr", false],
  ["title", "onload handler docs", false],
];

let failures = 0;
for (const [name, value, want] of cases) {
  const got = attrBlocked(name, value);
  if (got !== want) {
    console.error(`FAIL attr ${name}=${JSON.stringify(value)}: blocked=${got}, want ${want}`);
    failures++;
  }
}
for (const [tag, want] of [["script", true], ["SCRIPT", true], ["div", false], ["span", false]]) {
  if (tagBlocked(tag) !== want) {
    console.error(`FAIL tag ${tag}`);
    failures++;
  }
}

if (failures > 0) {
  process.exit(1);
}
console.log(`ok — ${cases.length + 4} XSS guard cases pass`);
