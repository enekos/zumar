// zumar.js — the entire JS half of the framework.
//
// Responsibilities: materialize SerNode trees, apply patches, delegate
// events, execute commands, manage subscription handles. It holds no app
// state and knows nothing about messages: an event is reported to Wasm as
// (node path, event name, payload scalars) and the vdom decides what it
// means. Render results arrive wire-encoded (see zumar-wire.js); no JSON
// crosses the boundary in either direction. This file is the part a
// zumar-lang compiler keeps verbatim. Examples symlink it into their www/.

import { decodeInit, decodeUpdate } from "./zumar-wire.js";

export function mount(app, root, opts = {}) {
  const listening = new Set();
  const preventDefaults = new Map(); // event name -> bool, refreshed per render
  const subHandles = new Map(); // sub id -> interval handle

  // Client-mode auth helpers. Same-origin cookies ride every request (so a
  // login httpPost's Set-Cookie sticks and later fetches are authenticated);
  // an optional bearer token covers agent/service clients with no cookie jar.
  const authInit = (extra) => {
    const headers = { ...(extra || {}) };
    if (opts.bearer) headers["Authorization"] = `Bearer ${opts.bearer}`;
    return { credentials: "same-origin", headers };
  };

  // Every program step (dispatch/resolve/notify) returns the same shape:
  // patches to apply, event specs, commands to run, subscription deltas.
  const step = (result) => {
    apply(root, result.patches);
    if (result.events.length) ensure(result.events);
    for (const cmd of result.cmds) exec(cmd);
    for (const delta of result.subs) subDelta(delta);
  };

  // Sync transports (wasm, gc) return update bytes from every call; async
  // transports (live) return nothing and push updates via app.onUpdate.
  const maybe = (bytes) => {
    if (bytes) step(decodeUpdate(bytes));
  };

  const exec = (cmd) => {
    const done = (ok, status, body) => maybe(app.resolve(cmd.id, ok, status, body));
    const s = cmd.spec;
    switch (s.kind) {
      case "delay":
        setTimeout(() => done(), s.ms);
        break;
      case "httpGet":
        fetch(s.url, authInit()).then(
          async (r) => done(r.ok, r.status, await r.text()),
          (e) => done(false, 0, String(e))
        );
        break;
      case "httpPost":
        fetch(s.url, {
          method: "POST",
          body: s.body,
          ...authInit({ "Content-Type": "application/json" }),
        }).then(
          async (r) => done(r.ok, r.status, await r.text()),
          (e) => done(false, 0, String(e))
        );
        break;
      default:
        console.warn("zumar: unknown cmd", s);
    }
  };

  const subDelta = (d) => {
    if (d.op === "start") {
      if (d.spec.kind === "every") {
        const fire = () => maybe(app.notify(d.id, Date.now()));
        subHandles.set(d.id, setInterval(fire, d.spec.ms));
      } else {
        console.warn("zumar: unknown sub", d.spec);
      }
    } else {
      clearInterval(subHandles.get(d.id));
      subHandles.delete(d.id);
    }
  };

  const ensure = (specs) => {
    for (const spec of specs) {
      preventDefaults.set(spec.name, spec.preventDefault);
      if (listening.has(spec.name)) continue;
      listening.add(spec.name);
      root.addEventListener(spec.name, (e) => {
        const path = pathOf(root, e.target);
        if (path === null) return;
        if (preventDefaults.get(spec.name)) e.preventDefault();
        const t = e.target;
        maybe(app.dispatch(
          Uint32Array.from(path),
          spec.name,
          t && "value" in t ? String(t.value) : undefined,
          t && typeof t.checked === "boolean" ? t.checked : undefined,
          // the key slot doubles as wheel direction: keyboard events carry
          // e.key, wheel events carry the sign of e.deltaY as "in"/"out"
          typeof e.key === "string" ? e.key
            : typeof e.deltaY === "number" ? (e.deltaY < 0 ? "in" : "out")
            : undefined
        ));
      });
    }
  };

  const init = decodeInit(app.init());
  root.replaceChildren(create(init.root));
  ensure(init.events);
  for (const cmd of init.cmds) exec(cmd);
  for (const delta of init.subs) subDelta(delta);
  if (app.onUpdate) app.onUpdate((bytes) => step(decodeUpdate(bytes)));
}

// --- XSS hardening (same policy as elm/virtual-dom) ---------------------
// The vdom builds DOM exclusively via createElement/createTextNode — no
// innerHTML anywhere — so markup in model data is inert by construction.
// What remains is attribute-level injection when *values* flow from user
// data into sensitive attributes; these guards close that:
//   - `on*` attributes never reach the DOM (events go through dispatch);
//   - javascript:/data:text/html URLs are dropped from URL attributes;
//   - srcdoc is blocked; <script> renders as an inert placeholder.

const URL_ATTRS = new Set(["href", "src", "action", "formaction", "xlink:href"]);

function safeTag(tag) {
  if (tag.toLowerCase() === "script") {
    console.warn("zumar: <script> elements are not allowed; rendering placeholder");
    return "z-blocked";
  }
  return tag;
}

function setSafeAttr(el, name, value) {
  const n = name.toLowerCase();
  if (n.startsWith("on") || n === "srcdoc") {
    console.warn(`zumar: attribute "${name}" is not allowed and was dropped`);
    return;
  }
  if (URL_ATTRS.has(n)) {
    // Strip control/space chars first: "java\tscript:" is a live vector.
    const v = value.replace(/[\u0000-\u0020]/g, "").toLowerCase();
    if (v.startsWith("javascript:") || v.startsWith("data:text/html")) {
      console.warn(`zumar: unsafe URL in "${name}" was dropped`);
      return;
    }
  }
  el.setAttribute(name, value);
}

// --- tree materialization ---------------------------------------------

function create(node) {
  if (node.kind === "text") return document.createTextNode(node.text);
  const el = document.createElement(safeTag(node.tag));
  for (const [name, value] of Object.entries(node.attrs)) {
    setSafeAttr(el, name, value);
  }
  for (const child of node.children) el.appendChild(create(child));
  return el;
}

// --- path addressing ---------------------------------------------------
// A path is a list of childNodes indices from the app root (root.firstChild).
// [] is the app root itself. Valid because zumar owns every node under the
// mount point, so DOM structure mirrors the vdom exactly.

function nodeAt(root, path) {
  let node = root.firstChild;
  for (const i of path) node = node.childNodes[i];
  return node;
}

function pathOf(root, target) {
  const appRoot = root.firstChild;
  let node = target;
  const path = [];
  while (node && node !== appRoot) {
    const parent = node.parentNode;
    if (!parent || node === root) return null; // outside our tree
    path.unshift(Array.prototype.indexOf.call(parent.childNodes, node));
    node = parent;
  }
  return node === appRoot ? path : null;
}

// --- patch application --------------------------------------------------
// Patches arrive in DFS order and are safe to apply sequentially (see
// zumar-core::patch docs). "value"/"checked" also update the live DOM
// properties — setAttribute alone doesn't reach an input the user has
// touched (the attribute/property split), which is what makes controlled
// inputs work.

function apply(root, patches) {
  for (const p of patches) {
    switch (p.op) {
      case "replace":
        nodeAt(root, p.path).replaceWith(create(p.node));
        break;
      case "setText":
        nodeAt(root, p.path).nodeValue = p.text;
        break;
      case "setAttr": {
        const n = nodeAt(root, p.path);
        setSafeAttr(n, p.name, p.value);
        if (p.name === "value" && "value" in n && n.value !== p.value) n.value = p.value;
        if (p.name === "checked" && "checked" in n) n.checked = true;
        break;
      }
      case "removeAttr": {
        const n = nodeAt(root, p.path);
        n.removeAttribute(p.name);
        if (p.name === "value" && "value" in n) n.value = "";
        if (p.name === "checked" && "checked" in n) n.checked = false;
        break;
      }
      case "appendChildren": {
        const parent = nodeAt(root, p.path);
        for (const child of p.nodes) parent.appendChild(create(child));
        break;
      }
      case "truncateChildren": {
        const parent = nodeAt(root, p.path);
        while (parent.childNodes.length > p.len) parent.removeChild(parent.lastChild);
        break;
      }
      case "insertChild": {
        const parent = nodeAt(root, p.path);
        parent.insertBefore(create(p.node), parent.childNodes[p.index] ?? null);
        break;
      }
      case "moveChild": {
        // The diff guarantees from > to, so the reference node's index is
        // unaffected by the implicit removal insertBefore performs.
        const parent = nodeAt(root, p.path);
        parent.insertBefore(parent.childNodes[p.from], parent.childNodes[p.to]);
        break;
      }
      default:
        console.warn("zumar: unknown patch op", p);
    }
  }
}
