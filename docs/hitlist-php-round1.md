# PHP dogfood round 1 — hitlist

Corpora: monolog, guzzle (agent 1), WordPress core, laravel/framework
(agent 2). Binary: release all-langs at the pack-spike commit (predates
the fluent/narrowing fixes). ~90 probes, no crashes, no ERROR-node
misparses anywhere (PHP 8.1 syntax included) — the grammar tier is
solid; every finding is resolution-side.

Position conventions verified: positional args are 0-based line/byte
col; `--at f:l:c` is 1-based editor coords.

## H1 (TOP) — cross-file resolution dark: pack visibility is IncludeClosure

`--references` on `esc_attr` finds 7/1281 (the 7 same-file ones,
exactly); `have_posts` 0/24; `Logger::addRecord` 10/16 (all same-file);
`Client::request` misses all 51 cross-file calls. Same-file counts are
grep-exact, cross-file is exactly zero → a visibility filter, not an
index gap (the index reported 1900 files indexed).

Root cause: `VisibilityAxis::for_origin` returns `IncludeClosure` for
EVERY pack language. PHP has no include closure (the pack declares
`include_path_tokens: false`), so the closure is empty and every
cross-file candidate is invisible. The axis must be derived from the
pack's linkage fact: include-path languages (C/C++) get IncludeClosure;
name-keyed packs get Transparent until a real SearchPath (composer
PSR-4) lands.
STATUS: LANDED — `for_origin` takes the pack's `include_path_tokens`
via the new `PackVisibility` routing fact.

## H2 (TOP) — `new ClassName()` types only when the class is same-file

`$q = new WP_Query($args); $q->have_posts()` — gd/hover dark one line
after an unambiguous ctor (agent 2, 5c); `--type-at` on
`$client = new Client()` → nothing (agent 1, F3); completion on the
result → garbage (F4). Root cause: pack call-site value resolution
(`callee_sid`) is file-local by design (the no-name-case-guess rule for
C macros). But `new X()` is STRUCTURAL — the syntax says X is a class —
so the pack may honestly mint `Expr(span) → Edge(TypeName(X))`, which
falls back to `ClassName(X)` with zero index knowledge.
STATUS: LANDED — `@expr.ctor` capture; object-creation sites emit the
TypeName edge.

## H3 (TOP) — `$this->prop` / `$obj->prop` invisible to gd/hover/references

Method calls on the same receiver resolve; property accesses don't
(agent 1 F1: 10/10 uses of `$this->handlers` missed; agent 2 F3: 59
WP_Query fields dark). The member ref IS minted (the tool echoes the
token).
Root cause found in repro: the pack declared the field as `$name`
(sigil-ful, Perl style) but PHP accesses it as `->name` — the name join
could never match, and the class-content gate rightly reads sigils as
Perl shapes.
STATUS: LANDED — fields key on the inner sigil-less name token
(promoted ctor params mint both the member and the `$`-spelled local).
gd/hover/references on properties all verified on the repro.

## H4 (TOP) — hover leaks Perl type vocabulary (`HashRef`, `Numeric`, `Bool`)

`int $x` hovers as `Numeric`, `array $h` as `HashRef` (agent 1 F2).
The display projection of `InferredType` is Perl-flavored everywhere.
STATUS: LANDED — the pack declares a `type_display` vocabulary
(`LangPack` → `PackFacts`), and `FileAnalysis::render_type` /
`display_type_of` are the one type-label projection every human surface
routes through (hover, member hover, inlay, signature, completion
detail, `--type-at`). Perl's empty map is a pass-through by
construction. EXTRACT_VERSION bumped for the PackFacts shape.

## H5 (HIGH) — completion omits inherited + trait members

`$this->` inside a subclass shows only same-file methods (agent 1 F4b).
Parents are recorded (extends/implements/use all land as parent edges);
the ancestor walk dies at the file boundary — same routing fact as H1.
STATUS: partially rides H1 (cross-file ancestor lookup unblocks once
the pack index answers); full verification deferred to round 2.

## H6 (HIGH) — member completion on an untypeable receiver dumps ~200 globals

The member slot should answer empty (or receiver-scoped nothing) when
the receiver doesn't type; instead the identifier-soup fallback fires
(agent 1 F4). Two fixes landed that shrink the blast radius: typed
receivers now complete members (H2's ctor typing), and PHP's flat
method-call nodes joined `member_kinds` so mid-token `->ma|p` climbs to
the member slot at all. The honest-empty gate for a still-untypeable
receiver is round-2 work with the cursor-slot taxonomy.

## H7 (TOP) — duplicate global functions: confidently wrong single answer

WordPress `noop.php` re-declares 19 core functions as empty stubs;
gd/hover on `esc_attr`/`is_admin` teleport to the stub with full
confidence (agent 2 F6). The honest answer for a multi-provider name is
ALL candidates (LSP definitions may be plural), or rank real-over-stub
by reference mass. Round-2 slice: audit `definitions()`'s candidate
collapse for pack languages.

## H8 (MEDIUM) — class constants outline as Method

`public const VALUES = [...]` → kind Method (agent 1 F6). The pack maps
`constant` → SymKind::Sub (Perl parity), and a Sub inside a class
package renders Method.
STATUS: LANDED — constants now outline as constants.

## H9 (LOW) — `--type-at` takes no root; passing one gives a raw OS error

Every other cursor verb takes `<root>` first (agent 1 F7). Round-2 CLI
polish: accept the root form or print the usage line.

## Non-findings worth keeping

outline exact vs grep on a 5161-line legacy class; zero ERROR nodes
across WP + laravel + monolog + guzzle probes incl. enums, attributes,
heredocs, first-class callables; extends/static-call/use-import gd all
correct; `--implementations` on an interface finds the implementor;
no panics anywhere.
