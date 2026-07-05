// zumar-wire.js — decoder for the zumar binary wire format (version 1).
// Layout spec lives in crates/zumar-runtime/src/wire.rs; keep in lockstep.
// Decodes to exactly the shapes the JSON protocol used, so zumar.js is
// transport-agnostic.

class Reader {
  constructor(bytes) {
    this.b = bytes;
    this.i = 0;
    this.td = new TextDecoder();
  }
  u8() {
    if (this.i >= this.b.length) throw new Error("zumar-wire: truncated message");
    return this.b[this.i++];
  }
  vu() {
    let n = 0;
    let shift = 1;
    for (let k = 0; k < 10; k++) {
      const byte = this.u8();
      n += (byte & 0x7f) * shift;
      if ((byte & 0x80) === 0) return n;
      shift *= 128;
    }
    throw new Error("zumar-wire: varint too long");
  }
  str() {
    const len = this.vu();
    if (this.i + len > this.b.length) throw new Error("zumar-wire: truncated string");
    const s = this.td.decode(this.b.subarray(this.i, this.i + len));
    this.i += len;
    return s;
  }
  path() {
    const depth = this.vu();
    const p = new Array(depth);
    for (let i = 0; i < depth; i++) p[i] = this.vu();
    return p;
  }
  node() {
    if (this.u8() === 0) return { kind: "text", text: this.str() };
    const tag = this.str();
    const nattrs = this.vu();
    const attrs = {};
    for (let i = 0; i < nattrs; i++) {
      const name = this.str();
      attrs[name] = this.str();
    }
    const nchildren = this.vu();
    const children = new Array(nchildren);
    for (let i = 0; i < nchildren; i++) children[i] = this.node();
    return { kind: "element", tag, attrs, children };
  }
  patch() {
    const tag = this.u8();
    const path = this.path();
    switch (tag) {
      case 0: return { op: "replace", path, node: this.node() };
      case 1: return { op: "setText", path, text: this.str() };
      case 2: return { op: "setAttr", path, name: this.str(), value: this.str() };
      case 3: return { op: "removeAttr", path, name: this.str() };
      case 4: {
        const n = this.vu();
        const nodes = new Array(n);
        for (let i = 0; i < n; i++) nodes[i] = this.node();
        return { op: "appendChildren", path, nodes };
      }
      case 5: return { op: "truncateChildren", path, len: this.vu() };
      case 6: return { op: "insertChild", path, index: this.vu(), node: this.node() };
      case 7: return { op: "moveChild", path, from: this.vu(), to: this.vu() };
      default: throw new Error(`zumar-wire: unknown patch tag ${tag}`);
    }
  }
  cmdSpec() {
    const kind = this.u8();
    if (kind === 0) return { kind: "delay", ms: this.vu() };
    if (kind === 1) return { kind: "httpGet", url: this.str() };
    throw new Error(`zumar-wire: unknown cmd kind ${kind}`);
  }
  subSpec() {
    const kind = this.u8();
    if (kind === 0) return { kind: "every", ms: this.vu() };
    throw new Error(`zumar-wire: unknown sub kind ${kind}`);
  }
  tail(out) {
    const nevents = this.vu();
    out.events = new Array(nevents);
    for (let i = 0; i < nevents; i++) {
      out.events[i] = { name: this.str(), preventDefault: this.u8() === 1 };
    }
    const ncmds = this.vu();
    out.cmds = new Array(ncmds);
    for (let i = 0; i < ncmds; i++) {
      out.cmds[i] = { id: this.vu(), spec: this.cmdSpec() };
    }
    const nsubs = this.vu();
    out.subs = new Array(nsubs);
    for (let i = 0; i < nsubs; i++) {
      if (this.u8() === 0) {
        out.subs[i] = { op: "start", id: this.vu(), spec: this.subSpec() };
      } else {
        out.subs[i] = { op: "stop", id: this.vu() };
      }
    }
    return out;
  }
  version() {
    const v = this.u8();
    if (v !== 1) throw new Error(`zumar-wire: unsupported version ${v}`);
  }
}

export function decodeInit(bytes) {
  const r = new Reader(bytes);
  r.version();
  return r.tail({ root: r.node() });
}

export function decodeUpdate(bytes) {
  const r = new Reader(bytes);
  r.version();
  const n = r.vu();
  const patches = new Array(n);
  for (let i = 0; i < n; i++) patches[i] = r.patch();
  return r.tail({ patches });
}
