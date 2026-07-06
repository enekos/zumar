// zumar-gc.js — adapts a zuc-gc-emitted module's raw exports to the app
// object zumar.js mounts. The GC backend has no wasm-bindgen glue: paths go
// through `path_buf`, String payloads through `payload_buf` (UTF-8), event
// names map to their index in the init message's events array, and every
// call returns wire bytes at mem[0..len].
//
//   const { instance } = await WebAssembly.instantiate(bytes, {});
//   mount(gcApp(instance.exports), root);

const EMPTY_UPDATE = Uint8Array.of(1, 0, 0, 0, 0); // ver, 0 patches/events/cmds/subs

export function gcApp(exports) {
  const { mem, init, dispatch, path_buf, payload_buf } = exports;
  const utf8 = new TextEncoder();
  let eventNames = [];

  const read = (len) => new Uint8Array(mem.buffer.slice(0, len));

  // Tiny wire peek: the events array of an init message (version byte, one
  // node, then events) — decoded fully by zumar-wire.js on the caller side;
  // here we only need the names, so reuse the real decoder lazily.
  return {
    init() {
      const bytes = read(init());
      eventNames = peekEventNames(bytes);
      return bytes;
    },
    dispatch(path, name, value) {
      const idx = eventNames.indexOf(name);
      if (idx < 0) return EMPTY_UPDATE;
      new Uint32Array(mem.buffer, path_buf.value, path.length).set(path);
      let payloadLen = 0;
      if (typeof value === "string") {
        const bytes = utf8.encode(value);
        new Uint8Array(mem.buffer, payload_buf.value, bytes.length).set(bytes);
        payloadLen = bytes.length;
      }
      return read(dispatch(idx, path.length, payloadLen));
    },
    // The GC backend emits no commands or subscriptions yet.
    resolve() {
      return EMPTY_UPDATE;
    },
    notify() {
      return EMPTY_UPDATE;
    },
  };
}

// Minimal init-message walk to the events array (kept in lockstep with
// zumar-wire.js; duplicated so this adapter stays dependency-free).
function peekEventNames(b) {
  let i = 1; // skip version
  const vu = () => {
    let n = 0;
    let s = 1;
    for (;;) {
      const byte = b[i++];
      n += (byte & 0x7f) * s;
      if ((byte & 0x80) === 0) return n;
      s *= 128;
    }
  };
  const td = new TextDecoder();
  const str = () => {
    const len = vu();
    const s = td.decode(b.subarray(i, i + len));
    i += len;
    return s;
  };
  const skipNode = () => {
    if (b[i++] === 0) {
      str(); // text
      return;
    }
    str(); // tag
    let n = vu();
    while (n-- > 0) {
      str();
      str();
    }
    let c = vu();
    while (c-- > 0) skipNode();
  };
  skipNode();
  const count = vu();
  const names = [];
  for (let k = 0; k < count; k++) {
    names.push(str());
    i++; // preventDefault byte
  }
  return names;
}
