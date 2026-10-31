//! Typed view over the tree-sitter-perl CST.
//!
//! The sanctioned tree consumers (`builder.rs`, `cursor_context.rs`, plugin
//! emit hooks) speak to tree-sitter through this module instead of raw
//! `child_by_field_name` / `kind() == "..."` pokes. The point is that each
//! grammar trap is encoded exactly once:
//!
//! - `=>` is a comma that autoquotes its LHS — pair walking
//!   ([`pair_nodes`]) is separator-agnostic by construction, so the
//!   plain-comma spelling can't be silently dropped.
//! - a plain comma list is flat (tree-sitter-perl ≥1.1.4), but EXPLICIT parens
//!   still nest a sub-list (`a => (b => c)` → `a, (b, c)`) — [`flatten_list`]
//!   splices the wrapper so a linear scan sees every pair.
//! - call arguments may arrive as a bare node or a `list_expression` /
//!   `parenthesized_expression` — [`call_args`] always yields the flat
//!   positional sequence.
//!
//! Typed wrappers are declared with [`typed_node!`]; accessors return what
//! the grammar *means*. Name-level Perl conventions (constructor names,
//! conventional invocants) live in `conventions.rs` — this module re-exports
//! them so tree-side callers have one import.

use tree_sitter::Node;

use crate::model::conventions::Sigil;

mod syntax;
pub(crate) use syntax::{NodeFacts, NodeId, Structure, SyntaxForest};
use crate::model::file_analysis::Span;

pub(crate) use crate::model::conventions::is_conventional_invocant_name;

/// Ergonomic accessors every tree consumer wants; `use crate::cst::NodeExt`
/// instead of re-spelling `utf8_text(..).ok()` chains.
pub(crate) trait NodeExt<'a>: Sized {
    /// The node's source text. `None` only on invalid UTF-8.
    fn text(&self, src: &'a [u8]) -> Option<&'a str>;
    /// Text of a named field's child.
    fn field_text(&self, field: &str, src: &'a [u8]) -> Option<&'a str>;
    /// The node's span, in `FileAnalysis` coordinates.
    fn span(&self) -> Span;
    /// Named children, by index (no `TreeCursor` borrow gymnastics).
    fn named(&self) -> NamedChildren<'a>;
    /// Named children minus trivia ([`is_trivia`]): what a positional read
    /// ("the first argument", "the last statement", "the k-th pair") means.
    fn elements(&self) -> Elements<'a>;
}

impl<'a> NodeExt<'a> for Node<'a> {
    fn text(&self, src: &'a [u8]) -> Option<&'a str> {
        self.utf8_text(src).ok()
    }
    fn field_text(&self, field: &str, src: &'a [u8]) -> Option<&'a str> {
        self.child_by_field_name(field).and_then(|n| n.text(src))
    }
    fn span(&self) -> Span {
        node_to_span(*self)
    }
    fn named(&self) -> NamedChildren<'a> {
        NamedChildren { node: *self, idx: 0 }
    }
    fn elements(&self) -> Elements<'a> {
        Elements(self.named())
    }
}

/// The replacement of an `s///e` (or `/ee`, which evals the result once
/// more): Perl compiles it as code, the grammar reads it as a string.
pub(crate) fn eval_replacement<'a>(node: Node<'a>, src: &[u8]) -> Option<Node<'a>> {
    if node.kind() != "substitution_regexp" {
        return None;
    }
    if !node.child_by_field_name("modifiers")?.text(src)?.contains('e') {
        return None;
    }
    node.elements().find(|c| c.kind() == "replacement")
}

pub(crate) fn node_to_span(node: Node) -> Span {
    Span {
        start: node.start_position(),
        end: node.end_position(),
    }
}

/// The called name of a call node: the `method` field of a method call,
/// the `function` field of either function-call shape.
pub(crate) fn extract_call_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "method_call_expression" => node.field_text("method", source).map(str::to_string),
        "function_call_expression" | "ambiguous_function_call_expression" => {
            node.field_text("function", source).map(str::to_string)
        }
        _ => None,
    }
}

/// Narrow `node`'s span to the bare tail after the last `::` in `text` (rule
/// #7): for a qualified name (`Foo::Bar::baz`) the renamable / highlightable
/// token is `baz`, not the whole path — so rename rewrites only the tail and
/// the qualifier survives, while the ref's `target_name` keeps the full path.
/// No `::` → the node's own span. FQ identifiers are single-line tokens, so
/// the tail column is `start.column + byte_offset_of_tail`.
pub(crate) fn fq_tail_span(node: Node, text: &str) -> Span {
    match text.rfind("::") {
        Some(idx) => {
            let s = node.start_position();
            Span {
                start: tree_sitter::Point { row: s.row, column: s.column + idx + 2 },
                end: node.end_position(),
            }
        }
        None => node_to_span(node),
    }
}

pub(crate) struct NamedChildren<'a> {
    node: Node<'a>,
    idx: usize,
}

impl<'a> Iterator for NamedChildren<'a> {
    type Item = Node<'a>;
    fn next(&mut self) -> Option<Node<'a>> {
        while self.idx < self.node.named_child_count() {
            let i = self.idx;
            self.idx += 1;
            if let Some(c) = self.node.named_child(i) {
                return Some(c);
            }
        }
        None
    }
}

/// [`NodeExt::elements`].
pub(crate) struct Elements<'a>(NamedChildren<'a>);

impl<'a> Iterator for Elements<'a> {
    type Item = Node<'a>;
    fn next(&mut self) -> Option<Node<'a>> {
        self.0.by_ref().find(|c| !is_trivia(*c))
    }
}

/// Declare a zero-copy typed wrapper for one grammar node kind. Generated
/// surface: `cast(node)` (kind-checked), `node()`, and one
/// `Option<Node>`-returning accessor per listed field.
macro_rules! typed_node {
    ($(#[$meta:meta])* $name:ident($kind:literal) { $($(#[$fmeta:meta])* $field:ident),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy)]
        pub(crate) struct $name<'a>(Node<'a>);

        #[allow(dead_code)]
        impl<'a> $name<'a> {
            pub(crate) fn cast(node: Node<'a>) -> Option<Self> {
                (node.kind() == $kind).then_some(Self(node))
            }
            pub(crate) fn node(&self) -> Node<'a> {
                self.0
            }
            $(
                $(#[$fmeta])*
                pub(crate) fn $field(&self) -> Option<Node<'a>> {
                    self.0.child_by_field_name(stringify!($field))
                }
            )*
        }
    };
}

typed_node! {
    /// `$obj->method(args)` / `$obj->$dyn(...)` / `Class->Pkg::method`.
    MethodCall("method_call_expression") {
        invocant,
        method,
        arguments,
    }
}

typed_node! {
    /// `name(args)` — the parenthesized call form.
    FunctionCall("function_call_expression") {
        function,
        arguments,
    }
}

typed_node! {
    /// `name args` — no parens; also `bless { ... }`.
    AmbiguousFunctionCall("ambiguous_function_call_expression") {
        function,
        arguments,
    }
}

/// What an `assignment_expression`'s operator does to the value, grouped by
/// the result it produces. Every compound operator is `LHS = LHS op RHS`, so
/// the group decides both the new value of the target and the value of the
/// expression itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssignOp {
    /// `=` — the value is the RHS.
    Plain,
    /// `||=` / `//=` — the old value when true/defined, else the RHS.
    Fallback,
    /// `&&=` — the RHS when the old value is true, else the (false) old value.
    AndThen,
    /// `.=` — string append.
    Append,
    /// `x=` — string repetition.
    Repeat,
    /// Arithmetic and shift ops — a number.
    Numeric,
    /// `|=` / `&=` / `^=` — a number, unless both operands are strings, when
    /// Perl does the op per character and the result is a string.
    Bitwise,
}

impl AssignOp {
    /// Whether the value can be the RHS itself, so facts about the RHS (its
    /// type, the call it came from) are facts about the target.
    pub(crate) fn carries_rhs(self) -> bool {
        matches!(self, AssignOp::Plain | AssignOp::Fallback | AssignOp::AndThen)
    }
}

/// Where an assignment's write lands: after the whole expression, so every
/// read inside it — the RHS of `$n = $n->parent`, the implicit read of
/// `$x ||= …` — sees the value being replaced. A declaration is the
/// exception: its RHS can't name the variable it declares, and the facts a
/// declaration must not kill (a parameter assertion) sit on its binding
/// token, so it writes there.
pub(crate) fn assignment_write_point(assign: Node) -> tree_sitter::Point {
    match assign.child_by_field_name("left") {
        Some(left) if left.kind() == "variable_declaration" => left.start_position(),
        _ => assign.end_position(),
    }
}

/// The operator of an `assignment_expression`. `None` for any other kind, or
/// an operator this table doesn't know.
pub(crate) fn assign_op(node: Node) -> Option<AssignOp> {
    if node.kind() != "assignment_expression" {
        return None;
    }
    Some(match node.child_by_field_name("operator")?.kind() {
        "=" => AssignOp::Plain,
        "||=" | "//=" => AssignOp::Fallback,
        "&&=" => AssignOp::AndThen,
        ".=" => AssignOp::Append,
        "x=" => AssignOp::Repeat,
        "+=" | "-=" | "*=" | "/=" | "%=" | "**=" | "<<=" | ">>=" => AssignOp::Numeric,
        "|=" | "&=" | "^=" => AssignOp::Bitwise,
        _ => return None,
    })
}

/// The shape of a literal, whatever grammar kind spells it. Readers match on
/// the shape, so a spelling the grammar splits across kinds (a list with or
/// without parens, a single- or double-quoted string) is one arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiteralShape {
    /// `qw(a b)`.
    Qw,
    /// `'a'` / `"a"`.
    Str,
    /// `a, b` or `(a, b)` / `(a)`. The empty list `()` is a `stub_expression`,
    /// which callers that want it name.
    List,
    /// `[a, b]`.
    ArrayRef,
    /// `{a => 1}`.
    HashRef,
}

/// `None` for anything that isn't a literal.
pub(crate) fn literal_shape(node: Node) -> Option<LiteralShape> {
    Some(match node.kind() {
        "quoted_word_list" => LiteralShape::Qw,
        "string_literal" | "interpolated_string_literal" => LiteralShape::Str,
        "list_expression" | "parenthesized_expression" => LiteralShape::List,
        "anonymous_array_expression" => LiteralShape::ArrayRef,
        "anonymous_hash_expression" => LiteralShape::HashRef,
        _ => return None,
    })
}

/// A list literal ([`LiteralShape::List`]).
pub(crate) fn is_list_literal(node: Node) -> bool {
    literal_shape(node) == Some(LiteralShape::List)
}

/// A call expression's arguments as a flat positional sequence. The
/// `arguments` field may be a bare single node or a `list_expression` /
/// `parenthesized_expression` wrapper — callers never see the difference.
pub(crate) fn call_args<'a>(call_node: Node<'a>) -> Vec<Node<'a>> {
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    if is_list_literal(args) {
        args.elements().collect()
    } else {
        vec![args]
    }
}

/// Flatten a pair-list container's children into one token stream, splicing
/// EVERY nested `list_expression` / `parenthesized_expression` inline. A plain
/// comma list is already flat, but explicit parens (`a => (b => c)`) nest a
/// sub-list — and that's pure *grouping* in Perl:
/// a bare list always flattens into its surroundings (`key => (a, b)` IS
/// `key, a, b`; the grouped-value spelling is the ref `[a, b]`). So we descend
/// nested groups wherever they appear, yielding the flat sibling sequence a
/// single linear scan can pair over. Ref literals (`[...]`, `{...}`) are NOT
/// groups and stay whole. Comments and POD are skipped ([`is_trivia`]).
pub(crate) fn flatten_list<'a>(list: Node<'a>, out: &mut Vec<Node<'a>>) {
    let count = list.child_count();
    for i in 0..count {
        let Some(child) = list.child(i) else { continue };
        if is_list_literal(child) {
            flatten_list(child, out);
        } else if !is_trivia(child) {
            out.push(child);
        }
    }
}

/// The named elements of a grouping container — a `parenthesized_expression`
/// or `list_expression` — with nested groups spliced inline: [`flatten_list`]
/// minus the punctuation. `($a, $b)` is `parenthesized_expression` over a
/// `list_expression`, `($a)` is `parenthesized_expression` over the scalar,
/// and a bare `$a, $b` is a `list_expression`; all three yield their scalars.
/// Callers gate on the container kind — a non-group node yields its own
/// children.
pub(crate) fn list_elements<'a>(list: Node<'a>) -> Vec<Node<'a>> {
    let mut flat = Vec::new();
    flatten_list(list, &mut flat);
    flat.into_iter().filter(|n| n.is_named()).collect()
}

/// Peel transparent grouping wrappers — a `parenthesized_expression` or a
/// single-element `list_expression` — down to the expression inside.
/// These are pure grouping in Perl (`($x)` IS `$x`), so descending changes
/// no semantics; a call/deref/element-access does, and is left alone.
/// Loops, so nested `(($x))` collapse.
pub(crate) fn peel_groups(mut node: Node) -> Node {
    while is_list_literal(node) {
        let mut named = node.elements();
        match (named.next(), named.next()) {
            (Some(inner), None) => node = inner,
            _ => break, // empty, or a real multi-element list
        }
    }
    node
}

/// What a `refgen_expression` (`\X`) takes a reference to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefgenOperand {
    /// `\&name`, `\&$v`, `\&{..}`.
    Code,
    Hash,
    Array,
    /// `\$x`, `\(...)`, `\"str"`: nothing the lattice models.
    Other,
}

/// `\my @z` names the declared variable, not the declaration.
pub(crate) fn refgen_operand(refgen: Node) -> Option<RefgenOperand> {
    let operand = refgen.elements().next()?;
    let operand = if operand.kind() == "variable_declaration" {
        operand.child_by_field_name("variable")?
    } else {
        operand
    };
    Some(match operand.kind() {
        "function" => RefgenOperand::Code,
        "hash" => RefgenOperand::Hash,
        "array" => RefgenOperand::Array,
        _ => RefgenOperand::Other,
    })
}

/// The container deref around `elem`: `@{$h->{k}}` / `%{$h->{k}}` (a
/// container whose varname is a block holding `elem`) and `$h->{k}->@*` /
/// `$h->{k}->%*`. `None` when `elem` is not the whole deref operand.
pub(crate) fn deref_container_of<'t>(syn: &SyntaxForest<'t>, elem: Node<'t>) -> Option<Node<'t>> {
    let mut up = syn.ancestors(elem);
    let p = up.next()?;
    if matches!(p.kind(), "array_deref_expression" | "hash_deref_expression") {
        return Some(p);
    }
    if p.kind() != "expression_statement" {
        return None;
    }
    up.next().filter(|b| b.kind() == "block")?;
    up.next().filter(|v| v.kind() == "varname")?;
    up.next().filter(|c| matches!(c.kind(), "array" | "hash"))
}

/// The assignment whose target list holds `node` as an element:
/// `($h->{k}, $x) = …`. A direct target (`$h->{k} = …`) is not in a list.
pub(crate) fn list_assignment_target_of<'t>(syn: &SyntaxForest<'t>, node: Node<'t>) -> Option<Node<'t>> {
    let mut up = syn.ancestors(node).peekable();
    let mut group = up.next().filter(|g| is_list_literal(*g))?;
    while let Some(outer) = up.next_if(|g| is_list_literal(*g)) {
        group = outer;
    }
    let assign = up.next().filter(|a| a.kind() == "assignment_expression")?;
    (assign.child_by_field_name("left")? == group).then_some(assign)
}

/// `meth Class(ARGS)` / `meth Class ARGS` / `meth Class` — the spelling Perl
/// may read as the indirect-object method call `Class->meth(ARGS)`. The
/// grammar parses it as a call to `meth` whose argument is a call to (or a
/// bareword naming) `Class`. Shape only: whether Perl reads it as a method
/// call depends on which names are subs and packages at that point, which
/// the caller decides.
#[derive(Clone, Copy)]
pub(crate) struct IndirectObjectCall<'a> {
    pub method: Node<'a>,
    pub class: Node<'a>,
    /// The call's arguments, when it has any.
    pub arguments: Option<Node<'a>>,
}

pub(crate) fn indirect_object_call<'a>(node: Node<'a>, src: &[u8]) -> Option<IndirectObjectCall<'a>> {
    use crate::model::conventions::{is_bareword_class_name, PERL_SPELLINGS};
    if node.kind() != "ambiguous_function_call_expression" {
        return None;
    }
    let method = node.child_by_field_name("function")?;
    let inner = node.child_by_field_name("arguments")?;
    let (class, arguments) = match inner.kind() {
        "bareword" => (inner, None),
        "function_call_expression" | "ambiguous_function_call_expression" => {
            (inner.child_by_field_name("function")?, inner.child_by_field_name("arguments"))
        }
        _ => return None,
    };
    // The method token is one plain identifier; a `&name`, a deref or a
    // qualified name is not.
    let method_text = method.utf8_text(src).ok()?;
    if !is_bareword_class_name(method_text, &PERL_SPELLINGS)
        || crate::model::file_analysis::split_qualified(method_text, &PERL_SPELLINGS).0.is_some()
    {
        return None;
    }
    if !is_bareword_class_name(class.utf8_text(src).ok()?, &PERL_SPELLINGS) {
        return None;
    }
    Some(IndirectObjectCall { method, class, arguments })
}

/// First named child of `node` (each group-peeled) whose kind satisfies
/// `pred` — the "pull the single typed operand out of a call" primitive.
/// The kind-filter callback keeps it reusable across callers.
pub(crate) fn first_named_child_where<'a>(
    node: Node<'a>,
    pred: impl Fn(&str) -> bool,
) -> Option<Node<'a>> {
    node.named().map(peel_groups).find(|c| pred(c.kind()))
}


/// position, never matched. This is THE pairing primitive — gating pair
/// walking on the `fat_comma` node silently drops the plain-comma spelling
/// (`{ 'GAMMA', 3 }` is identical to `{ GAMMA => 3 }`).
pub(crate) fn pair_nodes_in<'a>(children: &[Node<'a>]) -> Vec<(Node<'a>, Node<'a>)> {
    let mut out = Vec::new();
    let count = children.len();
    let mut i = 0;
    while i < count {
        let k_node = children[i];
        i += 1;
        if !is_list_element(k_node) {
            continue;
        }
        // Skip the comma / fat comma to the next named node (the value).
        let val = loop {
            match children.get(i) {
                Some(c) if is_list_element(*c) => break Some(*c),
                Some(_) => i += 1,
                None => break None,
            }
        };
        let Some(val) = val else { break };
        i += 1; // step past the value so the next key isn't read off it
        out.push((k_node, val));
    }
    out
}

/// A node the grammar declares an extra (Perl: comment, POD, heredoc body):
/// it can sit between any two tokens, so it is never an element of the list,
/// argument or block around it. Counting one shifts every later position.
pub(crate) fn is_trivia(node: Node) -> bool {
    node.is_extra()
}

/// The `n`-th element of `node` ([`NodeExt::elements`]).
pub(crate) fn nth_element(node: Node, n: usize) -> Option<Node> {
    node.elements().nth(n)
}

/// The last element of `node` ([`NodeExt::elements`]).
pub(crate) fn last_element(node: Node) -> Option<Node> {
    node.elements().last()
}

/// Every child of `node`, punctuation included, minus trivia: the token
/// stream a separator-aware scan walks.
pub(crate) fn children_without_trivia(node: Node) -> Vec<Node> {
    (0..node.child_count())
        .filter_map(|i| node.child(i))
        .filter(|c| !is_trivia(*c))
        .collect()
}

fn is_list_element(node: Node) -> bool {
    node.is_named() && !is_trivia(node)
}

/// The list a `{…}` / `[…]` literal holds, in either spelling: `{ a => 1 }`
/// or `{ (a => 1) }`. The literal itself when it holds one bare element.
/// Callers walk this rather than the literal's own children, which also
/// carry its braces and any comment inside them.
pub(crate) fn literal_body(literal: Node) -> Node {
    if is_list_literal(literal) {
        return literal;
    }
    literal.named().find(|c| is_list_literal(*c)).unwrap_or(literal)
}

/// Pair-walk a container node: a bare `list_expression` /
/// `parenthesized_expression`, or an `anonymous_hash_expression` (walked
/// through [`literal_body`]). Composition of [`flatten_list`] +
/// [`pair_nodes_in`].
pub(crate) fn pair_nodes<'a>(container: Node<'a>) -> Vec<(Node<'a>, Node<'a>)> {
    let list = if container.kind() == "anonymous_hash_expression" {
        literal_body(container)
    } else {
        container
    };
    let mut children = Vec::new();
    flatten_list(list, &mut children);
    pair_nodes_in(&children)
}

/// The `varname` child of a variable node (`scalar` / `array` / `hash` /
/// `container_variable`). For nontrivial derefs (`${$h{k}}`) the child is a
/// `block`, not a `varname` — this returns `None` there, so a `Some` is
/// always a simple identifier.
pub(crate) fn varname_child<'a>(node: Node<'a>) -> Option<Node<'a>> {
    node.named().find(|c| c.kind() == "varname")
}

/// `no Module LIST`: a `use_statement` led by the `no` keyword (the grammar
/// has no `no_statement` kind).
pub(crate) fn is_negated_use(node: Node) -> bool {
    (0..node.child_count()).filter_map(|i| node.child(i)).any(|c| !c.is_named() && c.kind() == "no")
}

/// The name token of a braced variable spelling (`${name}`, `@{ name }`) —
/// the `varname` child, braces excluded. `None` for the plain spelling and
/// for derefs (`${$ref}`, `@{[ ... ]}`).
pub(crate) fn braced_varname<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let vn = varname_child(node)?;
    let braced = (0..node.child_count()).filter_map(|i| node.child(i)).any(|c| c.kind() == "{");
    (braced && vn.named_child_count() == 0).then_some(vn)
}

/// Canonical sigiled spelling of a variable node: `${ sner }` → `$sner`
/// (the grammar's `varname` child already excludes the braces). `None`
/// for deref spellings (`${$ref}` — the varname child wraps a `block`,
/// not a bare identifier) and non-variable nodes.
pub(crate) fn canonical_var_name<'a>(node: Node<'a>, src: &'a [u8]) -> Option<String> {
    let sigil = match node.kind() {
        "scalar" => '$',
        "array" => '@',
        "hash" => '%',
        _ => return None,
    };
    let vn = varname_child(node)?;
    if vn.named_child_count() > 0 {
        return None;
    }
    Some(format!("{}{}", sigil, vn.text(src)?))
}

/// Canonical variable name for a container access: `$foo[0]` reads `@foo`,
/// `$foo{k}` reads `%foo`, `@foo{...}` slices `%foo`. Resolves the access
/// node + its parent's shape to the *declared* variable's sigil + bare name.
/// `None` = not a container-element access (caller keeps the raw text).
pub(crate) fn canonical_container_name<'a>(
    syn: &SyntaxForest<'a>,
    node: Node<'a>,
    src: &'a [u8],
) -> Option<String> {
    if deref_operand(node).is_some() {
        return None;
    }
    let bare = varname_child(node)?.text(src)?;
    Some(format!("{}{}", container_sigil(syn, node)?.char(), bare))
}

/// The sigil of the aggregate a container access reads — `@` for `$a[0]`,
/// `@a[..]`, `%a[..]`, `$#a`; `%` for `$h{k}`, `@h{..}`, `%h{..}`. The
/// access node's own sigil never says it: a slice is always `@`.
pub(crate) fn container_sigil<'t>(syn: &SyntaxForest<'t>, node: Node<'t>) -> Option<Sigil> {
    if node.kind() == "arraylen" {
        return Some(Sigil::Array);
    }
    let parent = syn.parent(node)?;
    match parent.kind() {
        "array_element_expression" => Some(Sigil::Array),
        "hash_element_expression" => Some(Sigil::Hash),
        "slice_expression" | "keyval_expression" => {
            if parent.child_by_field_name("array").is_some_and(|c| c == node) {
                Some(Sigil::Array)
            } else if parent.child_by_field_name("hash").is_some_and(|c| c == node) {
                Some(Sigil::Hash)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// What a deref node dereferences its operand as: `${..}` / `$$x` a
/// scalar, `@{..}` / `@$x` an array, `%{..}` / `%$x` a hash, an element,
/// slice or `$#` access the aggregate it reads, `&{..}` / `&$x` code,
/// `*{..}` a glob.
pub(crate) fn deref_sigil<'t>(syn: &SyntaxForest<'t>, node: Node<'t>) -> Option<Sigil> {
    match node.kind() {
        "scalar" => Some(Sigil::Scalar),
        "array" => Some(Sigil::Array),
        "hash" => Some(Sigil::Hash),
        "function" | "code_deref_expression" => Some(Sigil::Code),
        "glob" | "glob_deref_expression" => Some(Sigil::Glob),
        "container_variable" | "slice_container_variable" | "keyval_container_variable" | "arraylen" => {
            container_sigil(syn, node)
        }
        _ => None,
    }
}

/// What a deref spelling dereferences: the `block` of `@{EXPR}` /
/// `${EXPR}[0]` / `$#{EXPR}` / `&{EXPR}`, or the inner `scalar` of `@$x` /
/// `$$x[0]` / `$#$x` / `&$x`. `None` for a named variable — its `varname`
/// is a bare leaf.
pub(crate) fn deref_operand<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let holder = match node.kind() {
        "code_deref_expression" | "glob_deref_expression" => node,
        _ => varname_child(node)?,
    };
    holder.named().find(|c| matches!(c.kind(), "block" | "scalar"))
}

/// The expression a deref dereferences: the inner scalar of `@$x`, or the
/// lone expression of `@{EXPR}`. `None` for a named variable or a block
/// that is more than one expression.
pub(crate) fn deref_operand_expr<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let operand = deref_operand(node)?;
    if operand.kind() != "block" {
        return Some(operand);
    }
    let mut stmts = operand.elements();
    let stmt = stmts.next().filter(|s| s.kind() == "expression_statement")?;
    let mut exprs = stmt.elements();
    let expr = exprs.next()?;
    (stmts.next().is_none() && exprs.next().is_none()).then_some(expr)
}

/// Canonical access-path key for a place subject — a chain of constant
/// hash/array projections bottoming out at a plain scalar root
/// (`$self->{handler}`, `$self->{a}{b}`, `$self->[0]`). Returns
/// `(key, root)`: `key` is the place's source text (the spelling the
/// guard and the use share, so a witness keyed on it matches the
/// use-site invocant), `root` the base scalar (for the flow-narrowing
/// invalidation scan). Dynamic keys and accessor projections return
/// `None` — not a stable place identity.
pub(crate) fn canonical_place_path<'a>(
    syn: &SyntaxForest<'a>,
    node: Node<'a>,
    src: &'a [u8],
) -> Option<(String, String)> {
    let base = match node.kind() {
        "hash_element_expression" => {
            if !is_stable_subscript(node.child_by_field_name("key")?, src) {
                return None;
            }
            node.named_child(0)?
        }
        "array_element_expression" => {
            if !is_stable_subscript(node.child_by_field_name("index")?, src) {
                return None;
            }
            node.named_child(0)?
        }
        _ => return None,
    };
    // The root is a plain scalar (arrow form `$self->{a}`), the named
    // container itself (direct form `$h{a}` / `$h[0]`, whose base is a
    // `container_variable` resolving to `%h` / `@h`), or a nested constant
    // projection (`$self->{a}{b}`, `$h{a}{b}`).
    let root = match base.kind() {
        "scalar" => canonical_var_name(base, src)?,
        "container_variable" => canonical_container_name(syn, base, src)?,
        "hash_element_expression" | "array_element_expression" => {
            canonical_place_path(syn, base, src)?.1
        }
        _ => return None,
    };
    let key = node.text(src)?.to_string();
    Some((key, root))
}

/// A place subscript is stable when it pins one slot deterministically:
/// a constant key/index (`{foo}`, `[0]`) or a **plain scalar** (`{$k}`,
/// `[$i]`) whose identity is the variable spelling. A scalar key keeps
/// the place stable only while that scalar is unchanged — the narrower
/// tracks it via `place_dynamic_key_vars` and truncates on reassignment.
/// Deref / computed keys (`{$h{x}}`, `{compute()}`, `{$obj->m}`) are not
/// stable identities and disqualify the place.
fn is_stable_subscript(sub: Node, src: &[u8]) -> bool {
    match sub.kind() {
        "autoquoted_bareword" | "string_literal" | "number" => true,
        "scalar" => canonical_var_name(sub, src).is_some(),
        _ => false,
    }
}

/// What a place path's identity hangs on, read off its chain once.
pub(crate) struct PlaceDeps {
    /// The plain-scalar key/index variables along the path
    /// (`$self->{$k}{$j}` → `[$k, $j]`): reassigning one breaks the place's
    /// identity, so its narrowing truncates there.
    pub key_vars: Vec<String>,
    /// The keys of the path's proper element prefixes (`$self->{a}` of
    /// `$self->{a}{b}`): an opaque use of one can disturb the slot.
    pub prefixes: Vec<String>,
}

/// The dependencies of a place that passed `canonical_place_path` (every
/// subscript is stable, so every prefix is a place of its own).
pub(crate) fn place_deps<'a>(node: Node<'a>, src: &'a [u8]) -> PlaceDeps {
    let mut deps = PlaceDeps { key_vars: Vec::new(), prefixes: Vec::new() };
    let mut cur = node;
    loop {
        let sub = match cur.kind() {
            "hash_element_expression" => cur.child_by_field_name("key"),
            "array_element_expression" => cur.child_by_field_name("index"),
            _ => break,
        };
        if cur != node {
            if let Some(text) = cur.text(src) {
                deps.prefixes.push(text.to_string());
            }
        }
        if let Some(sub) = sub {
            if sub.kind() == "scalar" {
                if let Some(v) = canonical_var_name(sub, src) {
                    deps.key_vars.push(v);
                }
            }
        }
        match cur.named_child(0) {
            Some(base) => cur = base,
            None => break,
        }
    }
    deps
}

/// Per-word `(text, span)` pairs of a `quoted_word_list`, multi-line
/// aware (rows/cols tracked through internal whitespace).
pub(crate) fn qw_word_spans(qw_node: Node, src: &[u8], results: &mut Vec<(String, Span)>) {
    for j in 0..qw_node.named_child_count() {
        let Some(sc) = qw_node.named_child(j) else { continue };
        if sc.kind() != "string_content" {
            continue;
        }
        let Ok(text) = sc.utf8_text(src) else { continue };
        let sc_start = sc.start_position();
        let bytes = text.as_bytes();
        let mut row = sc_start.row;
        let mut col = sc_start.column;
        let mut i = 0;
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    row += 1;
                    col = 0;
                } else {
                    col += 1;
                }
                i += 1;
            }
            let word_start = tree_sitter::Point { row, column: col };
            let word_begin = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                col += 1;
                i += 1;
            }
            if i > word_begin {
                results.push((
                    text[word_begin..i].to_string(),
                    Span { start: word_start, end: tree_sitter::Point { row, column: col } },
                ));
            }
        }
    }
}

/// The `string_content` text of a quoted node — quote-flavor-agnostic.
pub(crate) fn string_content_text(node: Node, src: &[u8]) -> Option<String> {
    for i in 0..node.named_child_count() {
        if let Some(content) = node.named_child(i) {
            if content.kind() == "string_content" {
                return content.utf8_text(src).ok().map(|s| s.to_string());
            }
        }
    }
    None
}

/// The literal content of a quoted node ONLY when it's a plain,
/// non-interpolated, single-fragment literal — the shape a class-name /
/// reftype guard (`isa('Foo')`, `ref($x) eq 'HASH'`) needs. Anything
/// interpolated (`"$cls"`) or empty yields `None`, so an interpolated
/// argument never reads as a literal class name. Stricter sibling of
/// `string_content_text`.
pub(crate) fn plain_string_literal_text(node: Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string_literal" | "interpolated_string_literal") {
        return None;
    }
    let mut content: Option<String> = None;
    for i in 0..node.named_child_count() {
        let c = node.named_child(i)?;
        // Interpolation nests INSIDE the `string_content` (`"$cls"` →
        // string_content > scalar), so a plain literal is a single
        // `string_content` with no children of its own.
        if c.kind() != "string_content" || content.is_some() || c.named_child_count() > 0 {
            return None;
        }
        content = c.utf8_text(src).ok().map(|s| s.to_string());
    }
    content.filter(|s| !s.is_empty())
}

/// Span of the `string_content` inside a quoted node (selection span);
/// the whole node when there is none (empty string).
pub(crate) fn string_content_span(node: Node) -> Span {
    for i in 0..node.named_child_count() {
        if let Some(content) = node.named_child(i) {
            if content.kind() == "string_content" {
                return node_to_span(content);
            }
        }
    }
    node_to_span(node)
}

/// One arm of an `if`/`unless` statement chain.
#[derive(Clone, Copy)]
pub(crate) struct CondArm<'a> {
    /// `None` for the trailing `else`.
    pub cond: Option<Node<'a>>,
    pub block: Node<'a>,
    /// The truth value `cond` must take for `block` to run: `false` only
    /// for the leading arm of an `unless`. An `else` arm carries `true`.
    pub enter: bool,
}

/// A `conditional_statement`'s arms in order: the leading `if`/`unless`,
/// each `elsif`, then the `else`. The `elsif`/`else` nodes nest inside one
/// another, so the chain is the walk down them.
pub(crate) fn conditional_arms(stmt: Node<'_>) -> Vec<CondArm<'_>> {
    let mut arms = Vec::new();
    let (Some(cond), Some(block)) =
        (stmt.child_by_field_name("condition"), stmt.child_by_field_name("block"))
    else {
        return arms;
    };
    let enter = stmt.child(0).is_none_or(|k| k.kind() != "unless");
    arms.push(CondArm { cond: Some(cond), block, enter });
    let mut tail = stmt;
    while let Some(next) = tail.elements().find(|c| matches!(c.kind(), "elsif" | "else")) {
        let Some(block) = next.child_by_field_name("block") else { break };
        let cond = next.child_by_field_name("condition");
        arms.push(CondArm { cond, block, enter: true });
        if cond.is_none() {
            break;
        }
        tail = next;
    }
    arms
}

/// The template node a `map` callback contributes. `map EXPR, LIST` is
/// the callback itself; `map { ... } LIST` (the block form — at least
/// as idiomatic) yields the block's tail expression, since Perl
/// returns the last evaluated expression. A tail that is a lexical
/// scalar bound exactly once by a plain `my $x = EXPR;` in the same
/// block chases to that EXPR (`map { my $n = "Role::$_"; $n }`) — the
/// same one-binding rule as the `my $m = 'process'` constant fold,
/// applied block-locally where the builder's constant table can't
/// reach (the template still holds an unresolved `$_`). A tail that's
/// conditional, re-assigned, or not an expression statement is an
/// honest miss: no fold beats a wrong parent.
fn map_callback_template<'a>(cb: Node<'a>, src: &[u8]) -> Option<Node<'a>> {
    if cb.kind() != "block" {
        return Some(cb);
    }
    let stmts: Vec<Node> = cb.elements().collect();
    if stmts.is_empty() || !stmts.iter().all(|s| s.kind() == "expression_statement") {
        return None;
    }
    let tail = stmts.last()?.named_child(0)?;
    if tail.kind() != "scalar" {
        return Some(tail);
    }
    let name = tail.utf8_text(src).ok()?;
    let mut init: Option<Node> = None;
    for s in &stmts[..stmts.len() - 1] {
        let Some(expr) = s.named_child(0) else { continue };
        if expr.kind() != "assignment_expression" {
            continue;
        }
        let Some(left) = expr.child_by_field_name("left") else { continue };
        let declares = left.kind() == "variable_declaration"
            && left
                .named()
                .any(|c| c.kind() == "scalar" && c.utf8_text(src).ok() == Some(name));
        if declares {
            if init.is_some() {
                return None; // two bindings — not a static value
            }
            // Structural RHS pick (last named child that isn't the
            // left side) rather than the `right:` field — field
            // presence has drifted across grammar versions; structure
            // hasn't.
            let mut rhs = None;
            for i in 0..expr.named_child_count() {
                if let Some(c) = expr.named_child(i) {
                    if c.id() != left.id() {
                        rhs = Some(c);
                    }
                }
            }
            init = Some(rhs?);
        } else if left.utf8_text(src).ok() == Some(name) {
            return None; // re-assigned after (or instead of) the decl
        }
    }
    init
}

/// Strings of a list-ish node — qw(), paren/comma lists, arrayrefs,
/// bare string literals — with per-word spans, INCLUDING the case
/// where `node` itself is the leaf (a parenless call's `arguments` IS
/// the qw/string node). Non-literal elements (bareword constants,
/// `@list` variables) go through `fold`: syntax lives here, constant
/// resolution stays with the caller that has the state for it.
/// `map "PREFIX$_", LIST` — the string-template map over a literal
/// list is statically foldable: each produced string is the template
/// with `$_` replaced by the list element, and its span is the
/// ELEMENT's own span, so per-name refs (role parents, package lists)
/// land on the word that produced them. Exactly one `$_` in the
/// template and `map` only (`grep` filters, it doesn't transform);
/// anything fancier is an honest miss. The crm idiom that motivated
/// it: `with map "GenericCo::Sheets::Roles::$_", qw/CSV DB/;`.
fn map_built_strings(
    node: Node,
    src: &[u8],
    fold: &mut dyn FnMut(Node) -> Vec<(String, Span)>,
) -> Vec<(String, Span)> {
    let Some(kw) = node.child(0) else { return vec![] };
    if kw.utf8_text(src).ok() != Some("map") {
        return vec![];
    }
    let Some(cb) = node.child_by_field_name("callback") else { return vec![] };
    let Some(cb) = map_callback_template(cb, src) else { return vec![] };
    if cb.kind() != "interpolated_string_literal" {
        return vec![];
    }
    let Some(content) = cb.named().find(|c| c.kind() == "string_content") else {
        return vec![];
    };
    let Ok(ctext) = content.utf8_text(src) else { return vec![] };
    let subs: Vec<Node> = content.named().filter(|c| c.kind() == "scalar").collect();
    let [topic] = subs.as_slice() else { return vec![] };
    if topic.utf8_text(src).ok() != Some("$_") {
        return vec![];
    }
    let pre_end = topic.start_byte() - content.start_byte();
    let post_start = topic.end_byte() - content.start_byte();
    let (pre, post) = (&ctext[..pre_end], &ctext[post_start..]);
    let Some(list) = node.child_by_field_name("list") else { return vec![] };
    string_list(list, src, fold)
        .into_iter()
        .map(|(t, sp)| (format!("{pre}{t}{post}"), sp))
        .collect()
}

pub(crate) fn string_list(
    node: Node,
    src: &[u8],
    fold: &mut dyn FnMut(Node) -> Vec<(String, Span)>,
) -> Vec<(String, Span)> {
    string_list_with_residue(node, src, fold).0
}

/// `string_list`, additionally reporting RESIDUE: `true` when at least
/// one list item did not fold to literal strings — a runtime-generated
/// element (`with ReportProxy(type => ...)`, an unresolvable bareword
/// constant, a `map` whose template we can't fold). Callers recording
/// structural facts (parent edges, export lists) use the flag to mark
/// the record incomplete instead of silently dropping the item and
/// presenting a partial list as the whole truth.
pub(crate) fn string_list_with_residue(
    node: Node,
    src: &[u8],
    fold: &mut dyn FnMut(Node) -> Vec<(String, Span)>,
) -> (Vec<(String, Span)>, bool) {
    match literal_shape(node) {
        Some(LiteralShape::Qw) => {
            let mut results = Vec::new();
            qw_word_spans(node, src, &mut results);
            return (results, false);
        }
        Some(LiteralShape::Str) => {
            if let Some(text) = string_content_text(node, src) {
                return (vec![(text, string_content_span(node))], false);
            }
            return (vec![], true);
        }
        Some(_) => {}
        None => match node.kind() {
            "bareword" | "autoquoted_bareword" | "array" => {
                let v = fold(node);
                let residue = v.is_empty();
                return (v, residue);
            }
            "map_grep_expression" => {
                let v = map_built_strings(node, src, fold);
                let residue = v.is_empty();
                return (v, residue);
            }
            _ => {}
        },
    }
    let mut results = Vec::new();
    let mut residue = false;
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match literal_shape(child) {
            Some(LiteralShape::Qw) => qw_word_spans(child, src, &mut results),
            Some(LiteralShape::Str) => {
                if let Some(text) = string_content_text(child, src) {
                    results.push((text, string_content_span(child)));
                } else {
                    residue = true;
                }
            }
            Some(LiteralShape::List | LiteralShape::ArrayRef) => {
                let (v, r) = string_list_with_residue(child, src, fold);
                results.extend(v);
                residue |= r;
            }
            Some(LiteralShape::HashRef) | None => match child.kind() {
                "bareword" | "autoquoted_bareword" | "array" => {
                    let v = fold(child);
                    residue |= v.is_empty();
                    results.extend(v);
                }
                "map_grep_expression" => {
                    let v = map_built_strings(child, src, fold);
                    residue |= v.is_empty();
                    results.extend(v);
                }
                // Separators and parens are anonymous; anything NAMED we
                // didn't fold is a real list item we couldn't read.
                _ => {
                    if is_list_element(child) {
                        residue = true;
                    }
                }
            },
        }
    }
    (results, residue)
}

/// The block (or file) `node` runs in: the extent a fact observed at
/// `node` can hold over when no conditional arm bounds it.
pub(crate) fn enclosing_block<'t>(syn: &SyntaxForest<'t>, node: Node<'t>) -> Option<Node<'t>> {
    syn.ancestors(node).find(|p| matches!(p.kind(), "block" | "source_file"))
}

/// How far [`conditional_arm`] climbs: to the enclosing sub (or file), or
/// only to the enclosing block. A fact that lands on the block's own scope
/// asks `Block` — inside the block, the block has, by construction, run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmBound {
    Sub,
    Block,
}

/// A call on a variable or place whose callee `tag_of` names as reporting
/// what a reference is: which function, and the operand. A call or deref
/// operand (`ref(f($x))`) names nothing a guard could narrow.
pub(crate) fn ref_tag_call<'a>(
    node: Node<'a>,
    src: &[u8],
    tag_of: impl Fn(&str) -> Option<crate::model::file_analysis::RefTagFn>,
) -> Option<(crate::model::file_analysis::RefTagFn, Node<'a>)> {
    let (name, operand) = match node.kind() {
        "func1op_call_expression" => {
            (node.child(0)?.utf8_text(src).ok()?, node.named().find(|c| c.kind() != "comment")?)
        }
        "function_call_expression" | "ambiguous_function_call_expression" => {
            (node.field_text("function", src)?, node.child_by_field_name("arguments")?)
        }
        _ => return None,
    };
    let via = tag_of(name)?;
    let operand = peel_groups(operand);
    matches!(operand.kind(), "scalar" | "hash_element_expression" | "array_element_expression")
        .then_some((via, operand))
}

/// The arm of a conditionally-run construct, between `node` and `bound`,
/// that holds `node`: an `if`/`elsif`/`else` body or a later branch, a loop
/// body, a C-style `for`'s iterator, the statement a postfix
/// `if`/`unless`/`for`/`while` modifies, a ternary branch, the right
/// operand of a short-circuit. A condition, a loop list, a `for`
/// initialiser and a non-short-circuit operand run whenever their
/// construct does, so the climb passes through them. `None` = `node` runs
/// whenever its bound does. Scope-crossing conditionality (a write inside
/// a closure relative to an outer variable's scope) is the caller's check.
pub(crate) fn conditional_arm<'a>(syn: &SyntaxForest<'a>, node: Node<'a>, bound: ArmBound) -> Option<Node<'a>> {
    let mut child = node;
    let mut up = syn.ancestors(node);
    while let Some(p) = up.next() {
        let is_field = |f: &str| p.child_by_field_name(f).is_some_and(|c| c == child);
        let arm = match p.kind() {
            "subroutine_declaration_statement"
            | "method_declaration_statement"
            | "anonymous_subroutine_expression"
            | "source_file" => return None,
            "block" if bound == ArmBound::Block => return None,
            "conditional_statement"
            | "loop_statement"
            | "postfix_conditional_expression"
            | "postfix_loop_expression"
            | "conditional_expression" => !is_field("condition"),
            "cstyle_for_statement" => !is_field("initialiser") && !is_field("condition"),
            "for_statement" | "postfix_for_expression" => !is_field("list") && !is_field("variable"),
            "lowprec_logical_expression" => is_field("right"),
            "binary_expression" => {
                is_field("right")
                    && (0..p.child_count())
                        .filter_map(|i| p.child(i))
                        .any(|c| !c.is_named() && matches!(c.kind(), "&&" | "||" | "//"))
            }
            _ => false,
        };
        if arm {
            return Some(child);
        }
        child = p;
    }
    None
}

/// The scalar wrapped by a container node's deref varname (`@$h{…}` —
/// `slice_container_variable > varname > scalar`). `None` for plain
/// (non-deref) containers, whose varname is a leaf.
pub(crate) fn varname_inner_scalar_text<'a>(node: Node<'a>, src: &'a [u8]) -> Option<String> {
    for i in 0..node.named_child_count() {
        let c = node.named_child(i)?;
        if c.kind() != "varname" {
            continue;
        }
        for j in 0..c.named_child_count() {
            let gc = c.named_child(j)?;
            if gc.kind() == "scalar" {
                return gc.text(src).map(|s| s.to_string());
            }
        }
    }
    None
}

/// `$v->{k}` (arrow deref of the scalar's referent) vs `$foo{k}`
/// (element of `%foo` spelled with a `$` sigil): same node kind,
/// different variable — only the arrow form goes through the scalar.
/// The arrow is an anonymous token, so detect it in the source gap
/// between the container and the subscript.
pub(crate) fn element_arrow_deref(element: Node, src: &[u8]) -> bool {
    let Some(container) = element.named_child(0) else { return false };
    let Some(sub) = element
        .child_by_field_name("key")
        .or_else(|| element.child_by_field_name("index"))
    else {
        return false;
    };
    src.get(container.end_byte()..sub.start_byte())
        .is_some_and(|gap| gap.windows(2).any(|w| w == b"->"))
}

/// True when this node is the *container* of an element access —
/// `$c` in `$c->{k}` / `$c->[0]` / `$foo{k}` / `$foo[0]`. The element
/// expressions put the key/index in a named field; the container is the
/// other (first) named child. Element-access bases keep the reference
/// in hand; every other read position (call argument, RHS alias, list,
/// invocant, sigil deref) lets it escape to code that may mutate it.
pub(crate) fn is_element_access_base<'t>(syn: &SyntaxForest<'t>, node: Node<'t>) -> bool {
    let mut up = syn.ancestors(node);
    let Some(parent) = up.next() else { return false };
    match parent.kind() {
        "hash_element_expression" | "array_element_expression" => {
            parent.named_child(0).is_some_and(|c| c == node)
        }
        // Postfix deref slice (`$h->@{…}` / `$h->%{…}` / `$a->@[…]`):
        // the scalar fills the hashref/arrayref field. A slice READ
        // copies values out — it can't mutate the referent.
        "slice_expression" | "keyval_expression" => {
            parent.child_by_field_name("hashref").is_some_and(|c| c == node)
                || parent.child_by_field_name("arrayref").is_some_and(|c| c == node)
        }
        // Sigil-deref slice (`@$h{…}`): the scalar sits under the
        // container's varname. Plain sigil derefs (`%$h`, `$$h`) keep
        // their escape classification — only slice containers are
        // value-copying reads.
        "varname" => up.next().is_some_and(|gp| {
            matches!(
                gp.kind(),
                "slice_container_variable" | "keyval_container_variable"
            )
        }),
        _ => false,
    }
}

/// `Class->new(...)` — a constructor call with a class-shaped invocant.
/// Returns the invocant text (`Foo::Bar` or `__PACKAGE__`); `None` for
/// non-constructor methods and variable/positional invocants, whose class
/// comes from inference. The caller resolves `__PACKAGE__` — the enclosing
/// package is builder state this layer doesn't hold.
pub(crate) fn constructor_invocant<'a>(node: Node<'a>, src: &'a [u8]) -> Option<&'a str> {
    use crate::model::conventions::InvocantText;
    let call = MethodCall::cast(node)?;
    if !call
        .method()?
        .text(src)
        .is_some_and(crate::model::conventions::is_constructor_name)
    {
        return None;
    }
    let inv = call.invocant()?.text(src)?;
    match InvocantText::parse(inv) {
        InvocantText::CurrentPackage => Some(inv),
        // A computed receiver (`(ref $self)->new`) parses to a leading `(`
        // that classifies as Bareword; only a real package name is a class.
        InvocantText::Bareword(b) if crate::model::conventions::is_bareword_class_name(b, &crate::model::conventions::PERL_SPELLINGS) => Some(inv),
        InvocantText::Bareword(_)
        | InvocantText::Scalar(_)
        | InvocantText::NonScalar(_)
        | InvocantText::PositionalReceiver => None,
    }
}

/// The scalar a dynamic method call dispatches through: `$o->$cb(...)`,
/// `$o->${cb}`, `$o->$$cb`. The `method:` field is always a `method` node,
/// wrapping the scalar for these spellings and a bare name otherwise.
pub(crate) fn dynamic_method_scalar(call: Node) -> Option<Node> {
    call.child_by_field_name("method")?.named_child(0).filter(|c| c.kind() == "scalar")
}

/// An argument that flattens into any number of values in list context:
/// an array or hash, a deref of one, or a slice.
pub(crate) fn is_flattening(node: Node) -> bool {
    matches!(
        node.kind(),
        "array"
            | "hash"
            | "array_deref_expression"
            | "hash_deref_expression"
            | "container_variable"
            | "slice_expression"
            | "keyval_expression"
    )
}

/// True when `node` is a `scalar` whose bare varname is a conventional
/// invocant (`$self` / `$class` / `$this` / `$proto`), matching braced
/// spellings (`${self}`) and rejecting derefs (`${$ref}`) by reading the
/// canonical varname child instead of raw text. The node-level half of
/// "is this potentially the receiver" — position-based receiver detection
/// (`$_[0]`, `shift`) stays with the builder, which knows the sub context.
pub(crate) fn is_conventional_invocant_scalar<'a>(node: Node<'a>, src: &'a [u8]) -> bool {
    node.kind() == "scalar"
        && varname_child(node)
            .and_then(|v| v.text(src))
            .is_some_and(is_conventional_invocant_name)
}
