# ADR: The cursor Slot — one taxonomy, per-language detectors

Status: accepted (design).

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

- **`detect_slot(doc, point) → Slot`** is the one entry; per-language
  detectors live BEHIND it (Perl's = today's `cursor_context` logic,
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
  `InferredType`; everything else `None`. A STUB consumed by nothing: the
  seam exists; ranking/filtering by expected type is a separate slice that
  plugs in.

## Loose-coupling / undo story (per the standing forks convention)

Additive: `Slot` is a new enum; the detectors keep their files and
internals; consumers migrate handler-by-handler with behavior byte-guards
(completion outputs identical on fixtures). Undoing = deleting the enum
and re-inlining the two detector calls — no serialized state, no
EXTRACT_VERSION change. If a genuine fork arises mid-implementation
(e.g. ReceiverCtx's shape), pick loose, log in `docs/open-forks.md`.

## Non-goals

- No new completion behavior in the migration slice (byte-identical).
- No type-constrained ranking yet (the stub is the deliverable).
- Perl's sig-help internals stay put; only their slot verdict routes
  through `detect_slot`.
