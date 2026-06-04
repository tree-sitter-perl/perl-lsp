# QA design items — needs design, do not implement yet

> **STATUS (2026-06-05, EV 54)** — most of these landed during the sprint; the
> write-ups stay as reference / provenance. Per item:
> - **LANDED:** NAV (resolved-target edge + honest-miss) · A4 (SlotType witness +
>   reducer, v1 within-file) · E2 (helper `$c`, named-sub + inline) · D1 (multi-hop
>   `@ISA` — verified *already* closed) · B-tag (`%EXPORT_TAGS` incl. Readonly) ·
>   B4 (FQ-global `@Pkg::EXPORT` fold) · B2/B3 *consumer half* (bare/`:tag`/`-as`
>   import-binding).
> - **PARKED:** B2/B3 *re-export half* (Test::Most chains) — blocked on the upstream
>   X1 scanner thread-safety fix (`docs/parser-shortcomings.md`).
> - **PUNTED (next sprint):** MAIN-1, H1, MooseX::Role::Parameterized.
> - **MOVED to `docs/prompt-type-system-futures.md`:** NARROW-1 (narrowing/
>   flow-sensitivity — out of scope this sprint).

QA-findings clusters that are **not** quick fixes: each needs a design
decision (where the rule lives, what type/witness shape carries it,
whether it's core or plugin) before code. Implementing the smallest diff
here would violate rule #10 (no special-casing on shape) or paint over a
boundary the engine hasn't modeled yet. Each item below: the problem, why
it resists a local fix, the option space, and a recommendation.

Cross-refs: `docs/qa-findings.md` (the cluster letters), `docs/open-
problems.md` (the untyped-boundary statement), `docs/adr/receiver-gated-
dispatch.md` and `docs/adr/plugin-system.md` (the manifest seams a fix
would extend), the `exporters-core-vs-byo` memory note (B-cluster policy).

---

## E2 — helper/callback `$c` param typing (param typed by registration context)

**Problem.** `sub _helper($c, ...) { $c->stash(...) }` inside a Mojolicious
plugin types `$c` as the enclosing plugin package (its lexical home), so
every `$c->`-method flags. The author means `$c` to be a
`Mojolicious::Controller` — but that's known only from the *callback
contract* the sub is registered under (`$app->helper(foo => \&_helper)`),
not from anything lexically visible in the sub. ~75 FPs in Mojo alone.

**Why it needs design.** A sub parameter is currently typed from its
*lexical* surroundings (first param of a method → enclosing class via
`FirstParamInMethod`). Here the type flows from a **callsite in a
different statement** — the registration — into the param. That's
cross-procedure value-flow *into* a parameter, the exact gap
`docs/open-problems.md` calls boundary #4 and explicitly left out of the
route work. Special-casing "param named `$c` → Controller" is forbidden
(rule #10: the name carries no contract; tomorrow's `$ctrl` or `$self`
helper silently misses).

**Options.**
1. **Callback-contract manifest** — a plugin declares "subs registered via
   `helper`/`hook`/`under` have first param `Mojolicious::Controller`",
   analogous to `param_types()` / `dispatch_verbs()`. The builder, on
   seeing the registration, pushes a `FirstParam`-style witness on the
   referenced sub's param keyed to the declared type. Plugin owns the
   verb→type map; core stays generic.
2. **General callsite→param value-flow** — propagate the type of `\&sub`'s
   eventual invocation argument into the param across procedures. Correct
   and reusable, but it's the full boundary-#4 solve (a new inference
   axis), far larger than this FP.
3. Status quo + suppression heuristic — rejected (shape-branch).

**Recommendation.** Option 1. It's the receiver-gated/manifest pattern the
engine already uses, keeps the "which verb implies which type" knowledge in
the Mojo plugin, and is a strict subset of option 2 that a later general
value-flow pass can subsume. Ties directly to D1 (the catalyst multi-hop
`$c` is the same "param typed by external contract, then resolved through
inheritance" shape).

---

## D1 — multi-hop classic `use base` / `@ISA` method resolution at depth

**Problem.** Single-hop classic inheritance resolves (`SpamAssassin` 244
refs through one `@ISA` level). A 3-hop chain (`Result → BaseResult → Core`)
doesn't — methods defined two-plus levels up flag as unresolved.
Bugzilla ~304 FPs, schema-loader.

**Why it needs design.** `resolve_method_in_ancestors()` has a depth-20
DFS, so the *walk* isn't the cap — the breakage is that intermediate
ancestors aren't all present/typed at query time. Classic `@ISA`/`use base`
parents are cross-file; whether hop-2's parents are known depends on which
files were indexed/enriched, and enrichment runs **OPEN-only** (per CLAUDE.md
cross-file enrichment). So depth works when every hop happens to be an open
doc and degrades when a middle hop is a workspace-index/dependency file.
This is the same root as the catalyst multi-hop `$c`: ancestry resolution
that's correct one hop deep but loses the thread when a hop lives in a
file built without enrichment.

**Options.**
1. **Lift ancestry resolution onto the `MethodOnClass` query-time walk**
   for classic parents too — the registry already chases inheritance edges
   cross-file (`MethodOnClass{child} → Edge(MethodOnClass{parent})`); ensure
   classic `@ISA`/`use base` parents mint those edges at every hop, not just
   the open-doc ones, so the walk doesn't depend on enrichment having run.
2. Eagerly enrich the transitive ancestor set of open docs — narrower, but
   re-introduces the OPEN-only asymmetry one level out and risks fan-out.

**Recommendation.** Option 1, unified with the
`prompt-enrichment-inheritance-residual` Phase-2 work. The principle: depth
must be a property of the query-time edge walk (which already survives
enrichment), never of which files happened to be enriched. Verify the
`parents_of` seam emits classic-parent edges for *every* hop.

---

## B2 / B3 — exporter consumer-semantics SYSTEM (tags / renames / re-exports)

**Problem.** The consumer half of the exporter story is a large, coherent
FP source: tag/bundle expansion (`use M qw(:tag)`, `:DEFAULT`, `:all`,
`-V2`, `:log` driven by `%EXPORT_TAGS`); `-as` renames
(`use M foo => { -as => 'bar' }`); generic re-export (`use Test::Most`
re-exporting all of Test::More's `ok`/`is`/...). Individually:
Test::Most 253 FPs, Test::Spec 129, Type::Library/ResultDDL `-V2` 25/file.

**Why it needs design.** These aren't three bugs; they're one missing
**model of what a `use` imports**. Today import resolution is roughly
"names literally in the `qw()` list." A coherent fix must represent:
(a) a module's *export surface* — `@EXPORT`, `@EXPORT_OK`, `%EXPORT_TAGS`
(tag → name-set), and re-export chains (a module's surface *includes* the
surface of what it `use`s and re-exports); (b) the *consumer*'s import
spec — a list of selectors (bare name, tag, `-as` rename, regex, negation)
applied to that surface to produce the locally-bound name set. Picking off
`:all` alone, then `-as` alone, then Test::Most alone, builds three
shape-branches that don't compose (a `:tag` *with* an `-as` rename inside it
falls between them). This is squarely the "exporters are core's job"
decision (`exporters-core-vs-byo`): renames/bundles/re-exports are common
CPAN mechanics, not house style, so they belong in core's model — not a
per-module plugin.

**Options.**
1. **Two-stage model.** Stage A (producer, cross-file): each indexed module
   computes a resolved *export surface* = name → origin, with tags expanded
   and re-export `use` chains followed (bounded, seen-set). Stage B
   (consumer): the import spec is a selector list evaluated against that
   surface, yielding `(local_name, origin)` bindings — `-as` renames the
   local name, regex/negation filter, tags select sub-sets. Refs bind to
   `origin` for goto-def; locals suppress the unresolved warning.
2. Incremental per-feature patches — rejected; they don't compose and each
   re-touches the same import-parsing site.

**Recommendation.** Option 1, designed as one subsystem with the surface as
the shared data structure both stages name. It also subsumes B1
(re-export), B4 (`@EXPORT` at scale), B5 (callsite resolution — a bound
local name is resolvable everywhere, not just in the `qw` list), B7 (regex
import args). Sequence the producer surface first (it's the dependency);
the consumer selector evaluator reads it.

---

## A4 — hash-extracted invocant typing + the untyped-param/hash-element boundary

**Problem.** `my $x = $self->{field}; $x->method` types `$x` as `HashRef`
(the rep of a hash element), so `$x->method` flags even when `_field` holds
a typed object. SpamAssassin / perltidy (44) / Mojo. Sibling of A2 (`shift`
form) and the untyped-boundary open problem.

**Why it needs design.** This is **boundary #4** in `docs/open-problems.md`:
a value arriving from a hash element (or untyped param) has no declared
type, and the engine has deliberately not modeled "what type does this hash
slot hold." A naive fix — "extracting `$self->{x}` yields whatever was last
written to `$self->{x}`" — is real intra-procedural flow but collides with
the mutation-tracking the bag already does for `mutated_keys_on_class`, and
it's still silent when the write is cross-procedural (a constructor in
another file stuffs the slot). The boundary is principled, not an oversight:
modeling it wrong pollutes inference with `HashRef`-typed everything.

**Options.**
1. **Slot-type witnesses from observed writes.** When the builder sees
   `$self->{field} = <typed-expr>`, push a per-class slot→type witness;
   a later `my $x = $self->{field}` reads it. Covers same-file writes,
   degrades silently (no witness → stays untyped, today's behavior) for
   cross-file. Composes with the existing mutation Facts.
2. **Declared slot types** — a manifest (Moo `has`, DBIC columns already do
   this for accessors) extends to raw `$self->{field}` reads. Narrower but
   only helps framework classes.
3. Leave deferred (current state) — honest, but A4 stays a standing FP.

**Recommendation.** Option 1 as the general mechanism, *informed by* option
2's declarations where present (a `has`-synthesized slot already knows its
type). Keep the failure mode "no evidence → untyped," never "no evidence →
`HashRef`," so it can't regress typed chains. This is a genuine new inference
axis — schedule it with the boundary-#4 / value-flow work, not as a patch.

---

## H1 — duplicate-package resolution (two files `package Foo;` — which wins?)

**Problem.** Two files declare `package Bugzilla;`
(`contrib/Bugzilla.pm` shadows the root `Bugzilla.pm`); the resolver picks
the wrong one, breaking the singleton's type inference and exports.

**Why it needs design.** "Which file owns `package Foo`?" has no
ground-truth in static analysis — at runtime `@INC` order decides, and the
LSP has no single `@INC`. A heuristic is unavoidable, but it must be
*principled and stable* (rule #10: not "is this path `contrib/`"). The
choice interacts with B4 (a shadowed package exports the wrong `@EXPORT`)
and with workspace-vs-@INC priority (CLAUDE.md: documents → workspace_index
→ module_index already encodes a priority; duplicates *within* a tier are
the gap).

**Options.**
1. **Path-distance / role ranking.** Prefer the file whose path best matches
   the package name (`Bugzilla::Foo` → `lib/Bugzilla/Foo.pm`), then prefer
   `lib/` over `t/`, `contrib/`, `xt/`, `examples/` via a *role* ranking
   (test/contrib/example dirs deprioritized) — but encoded as a ranked
   `FileRole`, not an inline `if path.contains("contrib")` at the resolution
   site. The role is computed once at index time and carried on the entry.
2. Honor a real `@INC` / `.perl-lsp` config order when present — correct but
   requires config most projects won't have.

**Recommendation.** Option 1 with the rank as a typed `FileRole` on the
indexed entry (so the resolver asks the entry "are you canonical?" and the
entry answers — no path-string branch in the resolver). Make `@INC`/config
(option 2) an override when available. Decide this *before* B4: B4 may be
H1 in disguise (the shadow exports the wrong surface), so the duplicate
resolution must land first to test B4 cleanly.

---

## B4 — cross-file `@EXPORT` bare-`use` not suppressed at scale (Bugzilla ~899)

**Problem.** `use Bugzilla::Util;` (bare, no import list) should pull the
module's `@EXPORT`; instead every exported function flags. ~899 FPs in
Bugzilla — *surprising*, because GATE-5 (the landed `export_ok` bare-`use`
work) was supposed to cover this.

**Why it needs design.** It's not obviously its own bug — it's likely a
*symptom* of one of the items above and needs investigation to attribute
before any fix:
- **H1?** If the resolver picks `contrib/Bugzilla.pm` over the real one, it
  reads the wrong `@EXPORT` and suppresses nothing. Bugzilla is exactly the
  project where H1 reproduces — strong candidate.
- **Workspace-exporter resolution at scale?** GATE-5 may resolve `@EXPORT`
  for OPEN/indexed-as-dep modules but not for sibling workspace files at
  Bugzilla's size (hundreds of intra-project `use`s).
- **B6 warm-cache regression?** The "exported by X" attribution is lost on
  the warm/cached path (enrichment not persisted in the cache blob). If QA
  ran warm, the suppression that works cold is gone — making B4 a
  manifestation of B6, not a new defect.

**Why it needs design (not code).** The fix is different for each cause
(canonical-file selection vs. workspace-tier exporter lookup vs. cache-blob
persistence), and shipping one without attributing risks masking the others.

**Recommendation.** **Investigate first, in order:** (1) dump which file the
resolver picked for `package Bugzilla::Util` (H1); (2) re-run cold vs. warm
and diff the FP count (B6); (3) confirm whether `@EXPORT` is resolved for
workspace-tier files at all. Then fix the attributed cause. Most likely
chain: H1 lands → re-test → residual is B6 (persist enrichment in the cache
blob, bump `EXTRACT_VERSION`). Do not write a B4-specific suppression — it
would paper over whichever of H1/B6 is the real defect.

---

## B-tag — `%EXPORT_TAGS` membership (incl. `Readonly::Hash`/`Readonly::Array`)

**Problem.** A consumer `use M qw(:sometag)` imports the names that `M`'s
`%EXPORT_TAGS{sometag}` lists, and a *named* `use M qw(foo)` of one of those
same names also resolves. Today the tag form doesn't expand: the engine
never reads `%EXPORT_TAGS`, so a name imported via tag is invisible to import
resolution. Perl::Critic isolates this cleanly — in `Perl/Critic/Utils.pm`,
`hashify` is both an `@EXPORT_OK` name (named import → resolves) *and* a
member of `%EXPORT_TAGS{data_conversion}`. Importing `hashify` by name works;
the *same* `hashify` reached via `:data_conversion` does not, and flags 41×.
The `%EXPORT_TAGS` there is built with `Readonly::Hash our %EXPORT_TAGS =>
(...)`, so even the plain-hash read wouldn't suffice — the assignment is
wrapped in a `Readonly::Hash` call.

**Why it resists a local fix.** Two layers: (a) the builder doesn't populate
the export surface from `%EXPORT_TAGS` at all (only `@EXPORT` / `@EXPORT_OK`
feed `export` / `export_ok` / `export_lookup`), so `exports_name("hashify")`
is `false` even though `hashify` is a real, exported sub; (b) the
`Readonly::Hash %EXPORT_TAGS => (...)` and `Readonly::Array @EXPORT_OK =>
(...)` forms hide the table *inside a function call*, so a naïve
"`%EXPORT_TAGS = (...)`-assignment" reader misses them. This sits squarely on
the **exporters-are-core's-job** boundary (`exporters-core-vs-byo` memory
note): tag tables are common CPAN mechanics, not house style, so the rule
belongs in core's export-surface model — not a per-module plugin. It is the
**contained-ish sub-fix** of the larger B2/B3 tag/rename/re-export system: a
reader that (1) extracts `%EXPORT_TAGS` membership (plain *and* `Readonly`-
wrapped) into the module's export surface and (2) expands a consumer's
`:tag` selector against it. It does NOT need the full selector evaluator
(renames, regex, negation) to retire the Readonly-`%EXPORT_TAGS` FP cluster.

**Options.**
1. **Read `%EXPORT_TAGS` into the export surface (both forms).** In `build()`
   (rule #1), recognize `%EXPORT_TAGS = (tag => [names...], ...)` *and*
   `Readonly::Hash ... %EXPORT_TAGS => (...)` / `Readonly::Array ...
   @EXPORT_OK => (...)` — the Readonly wrapper is just a call whose last
   arg is the table literal. Record `tag → name-set`, and fold every tagged
   name into `export_ok` (so `exports_name` and `find_exporters` answer
   `true`). Then a consumer's `:tag` selector resolves the same way a bare
   name does today. Bounded, composes with B2/B3 (it *is* the producer-
   surface stage's `%EXPORT_TAGS` input).
2. Special-case Readonly at the consumer's `use` site — rejected (rule #10:
   branches on the producer's pragma; Const::Fast, manual `our` forms miss).

**Recommendation.** Option 1, scoped to the producer surface only. Build the
`tag → names` map and fold tagged names into the export set; defer the full
consumer selector evaluator (renames/regex/negation) to B2/B3. The
`Readonly`-wrapped read is the load-bearing nuance — handle the call-wrapped
assignment as just "the table is the trailing args of the call," not a
Readonly-specific branch, so Const::Fast and friends ride the same path.

---

## MAIN-1 — `main::` aggregation across `require` of package-less scripts

**Problem.** Legacy CGI (AWStats) `require`s package-less `.pm`/plugin files
into the running script; with no `package` statement, every sub in every
such file lands in `main::`. The host calls plugin subs and the plugins call
host subs — all in `main` at runtime — but each file is analyzed in
isolation, so cross-file `main::` symbols never unify. ~270 FPs in both
directions (host→plugin and plugin→host) in `awstats.pl` and its
`require "$pluginpath"` plugins.

**Why it resists a local fix.** Cross-file resolution keys on a *named*
package (`package_parents`, the module index's module→file map, the reverse
index). `main` is the implicit, unnamed package: there's no `package main;`
to anchor a module name, and many unrelated scripts each define their own
`main::` subs, so naïvely unifying all `main` symbols workspace-wide would
cross-link genuinely unrelated files (every `t/*.t` has its own `main`). The
real edge is **`require`-induced**: file A `require`s file B, so B's `main`
subs are visible in A. That's a *file-level dependency edge* the engine
doesn't model — distinct from `@ISA` (it's not inheritance) and from
`use`-import (no export list; everything in `main` is just visible). Modeling
it wrong (union all `main`) is worse than the FP.

**Options.**
1. **Model the `require`-dependency edge.** When a file statically `require`s
   another (literal path, or a resolvable `$var` whose value is a constant
   path), add a directed edge A→B; resolve unqualified calls in A against
   B's `main::` subs along that edge (bounded, seen-set). Only files actually
   reachable via a require edge unify — unrelated `main`s stay separate.
   The hard part is the dynamic `require "$pluginpath"` (path from config);
   degrade silently when the path isn't statically knowable.
2. **`.perl-lsp` manifest of require roots** — let the project declare which
   files aggregate into the script's `main`. Correct, but config most legacy
   projects won't write.
3. Leave deferred — honest; MAIN-1 stays a legacy-CGI-specific standing FP.

**Recommendation.** Option 1 *if* legacy-CGI support is in scope, gated on
statically-resolvable require paths (the AWStats `require "$file"` where
`$file` traces to a constant). Otherwise option 3 — this is a narrow,
legacy-CGI-specific shape (modern code uses packages), and the dynamic-path
require defeats static analysis anyway. Judgment call on whether the corpus
weight justifies the new dependency-edge axis.

---

## NARROW-1 — `Params::Util::_INSTANCE($x, 'Class')` type-narrowing guard

**Problem.** `if ( _INSTANCE($x, 'IO::All::File') ) { $x->binmode }`
(Email::Stuffer:573,673,679) does not narrow `$x` to the asserted class
inside the guarded block, so `$x->`-method calls flag. `Params::Util`'s
`_INSTANCE($thing, $class)` returns `$thing` iff it isa `$class` — a standard
runtime type assertion the engine doesn't recognize.

**Why it resists a local fix.** This is a **type-narrowing guard**: a
predicate whose truth in a conditional implies a tighter type for one of its
arguments along the then-branch. The engine has no narrowing axis — types are
assigned at binding sites, not refined by control flow. Special-casing
`_INSTANCE` by name is forbidden (rule #10): `blessed($x) && $x->isa('C')`,
`ref($x) eq 'C'`, `_INSTANCEDOES`, `reftype` all want the same treatment;
a name-allowlist enumerates a set that's always incomplete. The property
"this call, when true, narrows arg N to the class named by arg M" must live
on the *call/function descriptor*, not in the consumer.

**Options.**
1. **Narrowing-predicate descriptor + flow-scoped witness.** A function (core
   for `Params::Util`, plugin for house predicates) declares "truthy result
   narrows arg `N` to the class named by arg `M`" (or to a fixed class). The
   builder, seeing the guard in a conditional, pushes a Variable witness for
   the narrowed var **scoped to the then-branch span** (the bag already does
   temporal/positional scoping — `FrameworkAwareTypeFold` skips witnesses
   past the query point; a branch-span scope is the spatial analogue). The
   consumer asks the bag for the type at the use point and gets the narrowed
   class. This is a genuine new inference axis (flow-sensitive narrowing),
   shared by the whole guard family.
2. Recognize only the literal-class form (`_INSTANCE($x, 'Foo')`) as a
   single special pass — rejected; it's the rule #10 name/shape branch and
   doesn't compose with `isa`/`blessed`/`reftype`.

**Recommendation.** Option 1, as the general narrowing axis, with the
predicate→narrowing rule carried on a descriptor (core knows `Params::Util`;
plugins declare house guards) and the result published as a branch-span-
scoped witness. Sequence it with the A4 / boundary-#4 value-flow work — both
are "refine a value's type from evidence the binding site didn't carry," and
the branch-span witness scope is reusable for `if (ref $x eq 'Foo')` too.

---

## NAV — navigation/diagnostic resolution divergence (root-cause)

**The Round-1 headline.** goto-def / references / hover are measurably less
reliable than diagnostics and *diverge from them* — every Round-1 project hit
it. This is the unlanded `resolve_symbol` cursor→target unification (CLAUDE.md:
"planned but **not landed**" — handlers resolve the target via
`FileAnalysis::rename_kind_at`, then map `RenameKind`→`TargetRef` inline,
*separately* from `find_definition` and from the diagnostic path). This
section root-causes three observed failures so the design discussion starts
from mechanism, not symptom. All reproes are read-only, against the staged
corpus, EXTRACT_VERSION 44, parser 1.0.3. **CLI line/col are 0-based;
reported locations are 1-based.**

The structural finding: **"is this name/method resolvable, and to what?" is
answered by at least four different, non-unified code paths reading three
different data sources** — goto-def's `find_definition` (`file_analysis.rs`),
the references/rename `rename_kind_at`→`TargetRef`→`refs_to` chain
(`file_analysis.rs` + `resolve.rs`), the unresolved-function diagnostic
(`symbols.rs::collect_diagnostics`), and workspace-symbol (the raw symbol
table / `reverse_index`). They disagree because they consult different
sources (`export_lookup` vs `export`/`export_ok` vs `reverse_index` vs
per-call invocant inference) with different fallbacks. The three failures
below are each one slice of that disagreement.

### (a) Confident WRONG jump — `$self->{key}->method` lands on the package decl

**Repro.** `Email::Stuffer.pm`, the `$self->{email}->header_str_set(...)`
chain (1-based line 314): `perl-lsp --definition <root> .../Stuffer.pm 313 17`
→ `…/Stuffer.pm:3:9`, the **`package Email::Stuffer;` declaration** (the
package *name* starts at col 9). The actual `_assert_addr_list_ok` call one
line up resolves correctly (`312 9` → `:296:5`), so this is shape-specific,
not an off-by-one. Minimal reproduction:
```perl
package Foo;
sub new { bless { email => undef }, shift }
sub to { my $self = shift; $self->{email}->totallyunknownmethod(1); }
```
goto-def on `totallyunknownmethod` → `:1:9` (`package Foo`). Contrast: the
same method on a *plain*-hash-extracted or genuinely-untyped invocant
(`my $x = $h{email}; $x->m`, `my $y = external(); $y->m`) correctly returns
"No definition found." The trigger is specifically the `$self->{…}` chain.

**Root cause.** `FileAnalysis::method_call_invocant_class` (file_analysis.rs
~4657) resolves the invocant `$self->{email}` to `$self`'s class — the
enclosing package `Foo` — because the hash-element deref is dropped and the
base `$self`'s class wins (the A4 boundary, but here it produces a *wrong
positive* class, not just `HashRef`). With `class_name = Some("Foo")`,
`find_definition`'s `MethodCall` arm (file_analysis.rs ~3801) calls
`resolve_method_in_ancestors("Foo", "header_str_set")` → not found, `Foo` has
no parents → falls to the **`find_package_or_class(cn)` fallback (~3823)**,
which returns the package declaration span. So "I knew the class but couldn't
find the method on it" degrades to a confident jump to that class's `package`
line — *worse than a miss*.

**Bug vs. unification.** Two defects compose: (1) the A4 over-typing
(`$self->{key}` inheriting `$self`'s class) is the upstream cause and is the
boundary-#4 / A4 design item; (2) the `find_package_or_class` fallback at
~3823 is a **contained bug** independent of unification — "method not found on
a known class" should return `None` (let the cross-file adapter try, then
honestly miss), never jump to the package decl. The CLAUDE.md MethodCall arm
already returns `None` for the `class_name.is_none()` and `CrossFile` cases
precisely to avoid harmful jumps; the "known class, method-not-found,
no-parents" case is the one that still jumps. **Proposed fix direction:**
drop the `find_package_or_class` fallback from the method-not-found path (keep
it only when the cursor is genuinely on a bare class name, i.e. a `PackageRef`
ref, which already has its own arm at ~3845). This is a small, contained
change; the A4 over-typing is the separate design item that also needs to
land for the *correct* jump.

### (b) References undercount — method refs gated on per-call invocant inference

**Repro.** Method `references` results differ between the un-enriched
workspace build and the enriched fresh build of the *same* file. Minimal:
```perl
package Widget;
sub frobnicate { 1 }
sub run { my $self = shift;
  my $w = $ENV{X} ? external() : undef;   # $w untyped
  $w->frobnicate; $w->frobnicate; $self->frobnicate; }
```
`--references` on the `frobnicate` def returns the def + the `$self->`
call but **drops both `$w->frobnicate` sites** (the enriched copy yields 2,
the workspace copy yields 4 — the duplicate result set in the raw output is
the divergence made visible). AWStats `Format_Number` (172 call sites → 6)
is the same undercount at scale, compounded by REF-1 (see below).

**Root cause.** The references path (`cli_references`/LSP references →
`rename_kind_at`→`TargetRef::from_rename_kind`→`refs_to`) matches a
`MethodCall` ref to a `Method{class}` target in `resolve.rs::refs_to`'s
`matches_kind` block (~390–424) **only when
`analysis.method_call_invocant_class(r)` returns
`Some(class)` equal to (or on the rename-chain of) the target scope**. A
call site whose invocant class fails to infer (`$w` untyped) returns `None`
→ no match → the call silently drops from the result set. So method
references are gated on the *same* per-call-site invocant inference that
goto-def depends on; wherever inference is lossy (untyped vars, hash-extracted
invocants, cross-file types not yet enriched in this build), the call falls
out. The enriched-vs-workspace divergence is exactly this: enrichment changes
which invocants infer, so the two builds of one file disagree on the ref set.

**Bug vs. unification.** This is the **unification gap**, not a contained bug.
There is no second, inference-independent way to ask "does this `->frobnicate`
call refer to `Widget::frobnicate`?" — the answer is always routed through
invocant-class inference, which has no honest "unknown ⇒ include
conservatively" mode without re-introducing the `new`-floods-everything
problem (a name-only match over-collects). The principled fix is the
`resolve_symbol` unification: a cursor→target step that records, per
call-site ref, *which symbol it was resolved against at build time* (a
`resolves_to`-style edge for MethodCall, the way `Variable` refs already
carry `resolves_to`), so references read a stored edge instead of
re-deriving the class at query time. Then a call site that resolved at build
time stays matched regardless of query-time inference flakiness, and
genuinely unresolved sites are honestly excluded. **Proposed direction:**
extend the build pipeline's PostFold invocant-fill (it already computes
`invocant_class` on `MethodCall` refs) to also stamp the resolved *target
symbol/class* as a stored edge, and have `refs_to` match on that edge rather
than re-calling `method_call_invocant_class`. (Orthogonal driver **REF-1**:
calls embedded in expressions — `print "…".Format_Number($x)."…"` — emit no
`FunctionCall`/`MethodCall` ref at all, so they can't match by any mechanism;
that's a builder ref-emission gap, rule #7, and is the bulk of the AWStats
172→6 undercount. Fix it in the builder independently.)

### (c) Diagnostic resolves but goto-def/hover don't — divergent export surfaces

**Repro.** `Perl/Critic/Utils.pm` exports `hashify` two ways: it's an
`@EXPORT_OK` name **and** a member of `%EXPORT_TAGS{data_conversion}` (built
via `Readonly::Hash`). In `Policy.pm`, `hashify` is imported via the
`:data_conversion` tag and called at 1-based line 354. `--definition` →
**"No definition found"**; `--hover` → nothing; `--workspace-symbol hashify`
→ **finds it** (Sub, line 317). A *bare* `use Perl::Critic::Utils;` consumer
calling `hashify(…)` *also* fails goto-def — confirming it's not the tag
selector at the consumer but the **producer's export surface** missing the
name. The named-import sibling `interpolate` (literally in the qw list)
resolves correctly (goto-def → the `use` statement, then the .pm).

**Root cause.** Three different "is this name exported / resolvable" sources,
none unified:
- **goto-def** (`symbols.rs::resolve_imported_function` ~2126) gates on
  `cached.analysis.exports_name(name)` → `export_lookup`. `hashify` is
  **absent** from `export_lookup` because the builder populates the export
  surface from `@EXPORT`/`@EXPORT_OK` only, and `hashify`'s membership comes
  via `Readonly::Hash %EXPORT_TAGS` (the B-tag gap). So goto-def can't bind
  the call to the def.
- **the unresolved-function diagnostic** (`symbols.rs::collect_diagnostics`)
  has an extra fallback — `module_index.find_exporters(name)` and the
  `reverse_index` — so it doesn't surface `hashify` as an actionable "exported
  by X" hint, and at warning level the channel stays clean. The diagnostic's
  *non-flagging* and goto-def's *non-resolution* look contradictory but are
  two different code paths reading different sources.
- **workspace-symbol** walks the raw symbol table / `reverse_index`, which
  covers **every named sub** (not just exports), so it finds `hashify`
  regardless of the export surface.

So the same name is "found" (workspace-symbol), "not an error" (diagnostic),
and "no definition" (goto-def) simultaneously — the canonical divergence.

**Bug vs. unification.** Two layers, both needed: (1) the **export-surface
gap** (B-tag: `%EXPORT_TAGS` incl. `Readonly`-wrapped not folded into
`export_lookup`/`export_ok`) is a contained-ish builder/producer fix — land
B-tag and `exports_name("hashify")` becomes `true`, which *directly* fixes
this goto-def case (no nav change needed). (2) Even with the surface fixed,
the deeper issue is that goto-def, diagnostics, and workspace-symbol each
pick their own resolvability source with their own fallback ladder — the
`resolve_symbol` unification would make all three consult one
cursor→target resolver over one export-surface model, so they can never
disagree again. **Proposed direction:** fix B-tag first (retires this specific
FP + the matching goto-def miss with a bounded producer-side change), then,
as the unification lands, route `resolve_imported_function`,
`collect_diagnostics`'s import check, and the workspace-symbol path through
the *same* export-surface query so resolvability is single-sourced.

### NAV conclusions (for the design discussion)

- **(a) is a contained bug riding a design item.** The harmful jump comes from
  the `find_package_or_class` fallback on the method-not-found path
  (file_analysis.rs ~3823) — remove it (return `None`) and the confident
  wrong jump becomes an honest miss *today*, independent of unification. The
  *correct* jump additionally needs A4 (stop `$self->{key}` inheriting
  `$self`'s class).
- **(b) is the unification.** Method references are structurally gated on
  query-time invocant inference; the fix is a stored resolved-target edge on
  `MethodCall` refs (PostFold already has the invocant class in hand), read by
  `refs_to` instead of re-derived. REF-1 (missing ref emission for
  expression-embedded calls) is a separate builder fix that must also land.
- **(c) is one contained producer fix + the unification.** B-tag (fold
  `%EXPORT_TAGS`, incl. `Readonly`-wrapped, into the export surface) fixes the
  immediate goto-def miss; single-sourcing the resolvability query across
  goto-def / diagnostics / workspace-symbol is the unification that prevents
  the class of divergence recurring.

The throughline: **navigation re-derives resolution at query time from lossy
inference, while diagnostics and workspace-symbol consult coarser but more
complete indexes.** `resolve_symbol` should make every cursor→target consumer
read one resolved edge / one export surface, so "found by one feature, missed
by another" stops being possible. (a)'s fallback removal and (c)'s B-tag fold
are bankable now; (b) and the single-sourcing are the unification's core.
