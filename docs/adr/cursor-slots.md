# ADR: The cursor Slot — one taxonomy, per-language detectors

## Context

Two parallel slot-detection systems answer the same question — "what kind
of hole is the cursor in" — with no shared vocabulary: Perl's
`cursor_context.rs` (method position, hash key, import list, dispatch
arg-0, sig-help) and the pack languages' `cursor_sentinel.rs`
(sentinel-reparse member access, trigger-char gate, in-scope half). Every
consumer (completion handler, sig-help, the access filter slice E threaded
awkwardly) branches per-language and re-derives slot facts. This is the
N-path disease one tier above resolution — the same shape the CandidateSet
fixed — and it blocks type-constrained completion, which needs a typed
slot to hang on.

The CandidateSet ADR's honest boundary is unchanged: slot detection stays
OUTSIDE the resolution seam (it decides WHICH question to ask, never where
names come from). This ADR gives that boundary a shape.

## Decision

One closed vocabulary — `Slot` (cursor tier, no tree-sitter in consumers):

```rust
enum Slot {
    /// obj.| obj->| $x->|  — wants: entity content (members of receiver)
    Member { receiver: ReceiverCtx, op: MemberOp },
    /// $h->{| — wants: keys of the owner
    Key { owner: OwnerCtx },
    /// bare identifier — wants: the visible-universe projection (complete())
    Identifier { prefix: String },
    /// use Foo qw(|  /  #include "|  — wants: the named surface / headers
    Import { module: Option<String> },
    /// use |  /  Foo::| drill — wants: complete_modules() / sub-packages
    ModulePath { prefix: String },
    /// FOO x; — a type is expected here
    TypePosition { prefix: String },
    /// f(a, |) and `x == |` — wants: sig-help AND (future) type-constrained
    /// candidates. Carries the slot's EXPECTED TYPE when derivable.
    ArgPosition { callee: Option<CalleeCtx>, index: usize },
}
```

- **`detect_slot(doc, point) → Slot`** answers the identity question
  (Member/Key/Identifier/Import/ModulePath); `detect_call_slot` answers
  the orthogonal arg-position question sig-help needs. Per-language
  detectors live BEHIND both (Perl's = today's `cursor_context` logic,
  pack's = today's `cursor_sentinel` logic — re-expressed outputs, NOT
  rewritten internals). Consumers switch on `Slot`, never on language.
- **Each Slot variant declares its candidate question**: Identifier/
  ModulePath → CandidateSet projections; Member/Key → the entity-content
  seams (`PackageSymbol`/`ReceiverGated`/keys); Import → the named
  module's surface + the import affordance (`ImportFact` composition).
  The slot picks the question; the seams answer it. No slot ever
  enumerates names itself.
- **`Slot::expected_type()`** — the type-constrained completion hook:
  `ArgPosition` (param type at index) and comparison-shaped slots
  (`op_type == |` → the field's DOMAIN) return the expected
  `InferredType`; everything else `None`. Consumed by completion ranking
  at both the backend and symbols call sites, which reorder candidates by
  matching type.

## Scope

`Slot` carries no serialized state and no `EXTRACT_VERSION` dependency —
it is a cursor-time value, never cached. The per-language detectors keep
their own files and internals; consumers switch on `Slot` alone, never on
language.

Completion ranks candidates by `Slot::expected_type()` for `ArgPosition`
(param type at index) and comparison-shaped slots (`op_type == |` → the
field's domain) — wired in `lsp/symbols/completion.rs`. `TypePosition` is
detected (`FOO x;` in pack languages) but not yet a candidate source:
completion returns none for it. Perl's sig-help internals are unchanged;
only their slot verdict routes through `detect_slot`.
