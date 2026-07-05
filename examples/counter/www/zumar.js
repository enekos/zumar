// zumar.js — the entire JS half of the framework.
//
// Responsibilities: materialize SerNode trees, apply patches, and delegate
// events. It holds no app state and knows nothing about messages: an event
// is reported to Wasm as (node path, event name) and the vdom decides what
// it means. This file is the part a future zumar-lang compiler would keep
// verbatim.

export function mount(app, root) {
  const init = JSON.parse(app.init());
  root.replaceChildren(create(init.root));

  const listening = new Set();
  const ensure = (events) => {
    for (const name of events) {
      if (listening.has(name)) continue;
      listening.add(name);
      root.addEventListener(name, (e) => {
        const path = pathOf(root, e.target);
        if (path === null) return;
        const result = JSON.parse(app.dispatch(Uint32Array.from(path), name));
        apply(root, result.patches);
        ensure(result.events);
      });
    }
  };
  ensure(init.events);
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
// zumar-core::patch docs).

function apply(root, patches) {
  for (const p of patches) {
    switch (p.op) {
      case "replace":
        nodeAt(root, p.path).replaceWith(create(p.node));
        break;
      case "setText":
        nodeAt(root, p.path).nodeValue = p.text;
        break;
      case "setAttr":
        nodeAt(root, p.path).setAttribute(p.name, p.value);
        break;
      case "removeAttr":
        nodeAt(root, p.path).removeAttribute(p.name);
        break;
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
      default:
        console.warn("zumar: unknown patch op", p);
    }
  }
}
