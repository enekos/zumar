// zumar.js — the entire JS half of the framework.
//
// Responsibilities: materialize SerNode trees, apply patches, and delegate
// events. It holds no app state and knows nothing about messages: an event
// is reported to Wasm as (node path, event name, payload envelope) and the
// vdom decides what it means. This file is the part a future zumar-lang
// compiler would keep verbatim. Examples symlink it into their www/.

export function mount(app, root) {
  const listening = new Set();
  const preventDefaults = new Map(); // event name -> bool, refreshed per render
  const subHandles = new Map(); // sub id -> interval handle

  // Every program step (dispatch/resolve/notify) returns the same shape:
  // patches to apply, event specs, commands to run, subscription deltas.
  const step = (result) => {
    apply(root, result.patches);
    if (result.events.length) ensure(result.events);
    for (const cmd of result.cmds) exec(cmd);
    for (const delta of result.subs) subDelta(delta);
  };

  const exec = (cmd) => {
    const done = (payload) =>
      step(JSON.parse(app.resolve(cmd.id, JSON.stringify(payload))));
    const s = cmd.spec;
    switch (s.kind) {
      case "delay":
        setTimeout(() => done({}), s.ms);
        break;
      case "httpGet":
        fetch(s.url).then(
          async (r) => done({ ok: r.ok, status: r.status, body: await r.text() }),
          (e) => done({ ok: false, status: 0, body: String(e) })
        );
        break;
      default:
        console.warn("zumar: unknown cmd", s);
    }
  };

  const subDelta = (d) => {
    if (d.op === "start") {
      if (d.spec.kind === "every") {
        const fire = () =>
          step(JSON.parse(app.notify(d.id, JSON.stringify({ now: Date.now() }))));
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
        step(JSON.parse(
          app.dispatch(Uint32Array.from(path), spec.name, JSON.stringify(envelope(e)))
        ));
      });
    }
  };

  const init = JSON.parse(app.init());
  root.replaceChildren(create(init.root));
  ensure(init.events);
  for (const cmd of init.cmds) exec(cmd);
  for (const delta of init.subs) subDelta(delta);
}

// The standard payload envelope (see zumar-core EventPayload). Fields the
// target doesn't have are null; the vdom-side handler picks what it needs.
function envelope(e) {
  const t = e.target;
  return {
    value: t && "value" in t ? String(t.value) : null,
    checked: t && typeof t.checked === "boolean" ? t.checked : null,
    key: typeof e.key === "string" ? e.key : null,
  };
}

// --- tree materialization ---------------------------------------------

function create(node) {
  if (node.kind === "text") return document.createTextNode(node.text);
  const el = document.createElement(node.tag);
  for (const [name, value] of Object.entries(node.attrs)) {
    el.setAttribute(name, value);
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
        n.setAttribute(p.name, p.value);
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
