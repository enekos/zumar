;; WasmGC spike — the phase-3 question in miniature.
;;
;; Goal: confirm that (a) the browser/Node actually runs WasmGC opcodes, and
;; (b) the architecture "GC heap for state, linear memory for the wire
;; buffer" works — i.e. a hand-emitted module can produce the exact bytes
;; that the real www/zumar-wire.js decoder reads. State lives in a GC
;; `struct` reached through a typed global (genuine GC, not linear-memory
;; ints); the render is serialized into linear memory and handed to JS as
;; (offset 0, length), exactly as a wasm-bindgen module does today.
;;
;; This is deliberately a single-digit counter — itoa isn't what's being
;; tested. The point is the type system and the boundary.

(module
  ;; --- GC heap ---------------------------------------------------------
  (type $model (struct (field $count (mut i32))))
  (global $state (mut (ref null $model)) (ref.null $model))

  ;; --- linear memory for the outbound wire buffer ----------------------
  (memory (export "mem") 1)

  (func $boot (global.set $state (struct.new $model (i32.const 0))))
  (start $boot)

  (func $digit (result i32)
    (i32.add (i32.const 48)
      (i32.rem_u
        (struct.get $model $count (global.get $state))
        (i32.const 10))))

  ;; init() -> len.  Serializes InitialRender for `span [ text "<count>" ]`:
  ;;   ver=1  node(element "span", 0 attrs, 1 child: text "<d>")  events=0 cmds=0 subs=0
  (func (export "init") (result i32)
    (i32.store8 (i32.const 0) (i32.const 1))    ;; wire version
    (i32.store8 (i32.const 1) (i32.const 1))    ;; node tag: element
    (i32.store8 (i32.const 2) (i32.const 4))    ;; tag string length
    (i32.store8 (i32.const 3) (i32.const 115))  ;; 's'
    (i32.store8 (i32.const 4) (i32.const 112))  ;; 'p'
    (i32.store8 (i32.const 5) (i32.const 97))   ;; 'a'
    (i32.store8 (i32.const 6) (i32.const 110))  ;; 'n'
    (i32.store8 (i32.const 7) (i32.const 0))    ;; attr count
    (i32.store8 (i32.const 8) (i32.const 1))    ;; child count
    (i32.store8 (i32.const 9) (i32.const 0))    ;; child node tag: text
    (i32.store8 (i32.const 10) (i32.const 1))   ;; text length
    (i32.store8 (i32.const 11) (call $digit))   ;; the digit
    (i32.store8 (i32.const 12) (i32.const 0))   ;; events
    (i32.store8 (i32.const 13) (i32.const 0))   ;; cmds
    (i32.store8 (i32.const 14) (i32.const 0))   ;; subs
    (i32.const 15))

  ;; dispatch(delta) -> len.  Mutates the GC struct, serializes an Update
  ;; with a single SetText at path [0].
  (func (export "dispatch") (param $delta i32) (result i32)
    (struct.set $model $count (global.get $state)
      (i32.add
        (struct.get $model $count (global.get $state))
        (local.get $delta)))
    (i32.store8 (i32.const 0) (i32.const 1))    ;; wire version
    (i32.store8 (i32.const 1) (i32.const 1))    ;; patch count
    (i32.store8 (i32.const 2) (i32.const 1))    ;; patch op: setText
    (i32.store8 (i32.const 3) (i32.const 1))    ;; path depth
    (i32.store8 (i32.const 4) (i32.const 0))    ;; path[0]
    (i32.store8 (i32.const 5) (i32.const 1))    ;; text length
    (i32.store8 (i32.const 6) (call $digit))    ;; the digit
    (i32.store8 (i32.const 7) (i32.const 0))    ;; events
    (i32.store8 (i32.const 8) (i32.const 0))    ;; cmds
    (i32.store8 (i32.const 9) (i32.const 0))    ;; subs
    (i32.const 10)))
