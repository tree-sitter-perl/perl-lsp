//! Sentinel reparse for member-access cursor context — the pack-language
//! member-completion seam (`docs/adr/cursor-context-completion.md`).
//!
//! A member of the reparse family (`cpp_reparse.rs`, `reparse.rs`): a
//! **source edit + reparse + span remap**. The others fix a parse
//! corrupted by a *declaration* (a macro, a prototype). This one fixes a
//! parse corrupted by *incompleteness*: at the instant a user triggers
//! completion the buffer reads `box.` / `box->` / `obj.` — a member access
//! with no member, so tree-sitter produces an ERROR and the receiver is no
//! longer reachable through the typed `field_expression` / `attribute`
//! shape the rest of the engine speaks.
//!
//! The fix is the same shape as expansion: splice a placeholder identifier
//! (`__CURSOR__`) at the cursor so the access becomes syntactically
//! complete, reparse, locate the placeholder, and take its member node's
//! receiver. This module does only step (a) — detect member access +
//! identify the receiver; the backend feeds the receiver span to
//! `expr_type_at_span` + `complete_members_for_class` for (b)/(c).
//!
//! Coordinate remap is trivial here, and that is the whole appeal: the
//! splice lands AT the cursor, strictly AFTER the receiver, so every
//! receiver byte offset is identical in patched and original source — the
//! one anchor the other reparse siblings must carry, this one gets free.
//! Language config is a two-field table (rule #10): the member-access node
//! kinds and the don't-splice-here set are the only facts that vary.

use crate::model::file_analysis::MemberShape;
use crate::model::file_analysis::{
    expected_member_op, CrossFileLookup, FileAnalysis, InferredType, Span,
};
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

/// The placeholder spliced at the cursor to complete a dangling member
/// access. Chosen to be a legal identifier in every C-family / Python
/// grammar and vanishingly unlikely to collide with real source.
pub const SENTINEL: &str = "__CURSOR__";

/// Byte offset of a `Point` in `src` (Point.column is a byte offset
/// within its row). The inverse of `byte_to_point`.
pub fn point_to_byte(src: &str, point: Point) -> usize {
    let (mut row, mut col) = (0usize, 0usize);
    for (i, ch) in src.char_indices() {
        if row == point.row && col == point.column {
            return i;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += ch.len_utf8();
        }
    }
    src.len()
}

/// The receiver of a dangling member access, recovered by sentinel
/// re-parse. Test-only probe surface: production consumes
/// `member_completion_ctx_incremental`; these wrappers exercise the shared
/// sentinel mechanics (`find_sentinel` / `climb_to_member` / `cursor_in_skip`)
/// with a directly assertable result.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct Receiver {
    /// Receiver source text (`box`, `obj`, `a.b`, `make()` ...).
    pub text: String,
    /// Receiver span in ORIGINAL (unpatched) byte coordinates. Equal to
    /// the patched coordinates because the splice is at/after `end`.
    pub start: usize,
    pub end: usize,
    /// The access operator the sentinel completed: `->` (true) vs `.`.
    pub arrow: bool,
}

/// Splice the sentinel at `cursor` (a byte offset into `src`) and return
/// the patched buffer. The cursor is expected to sit just after a `.` /
/// `->` (possibly with trailing whitespace / a partial member already
/// typed — the caller strips that; here we patch exactly at `cursor`).
pub fn patch(src: &str, cursor: usize) -> String {
    let mut out = String::with_capacity(src.len() + SENTINEL.len());
    out.push_str(&src[..cursor]);
    out.push_str(SENTINEL);
    out.push_str(&src[cursor..]);
    out
}

/// True when `cursor` lands inside a string/char/comment in the ORIGINAL
/// parse — splicing there would be a no-op at best, corruption at worst.
fn cursor_in_skip(orig: &Tree, src: &str, cursor: usize, cfg: &crate::build::query_extract::LangPack) -> bool {
    // Probe one byte back: at `box.` the cursor is past the `.`, but a
    // cursor that is literally inside `"foo`| sits within the string.
    let probe = cursor.saturating_sub(1).min(src.len().saturating_sub(1));
    let Some(node) = orig
        .root_node()
        .descendant_for_byte_range(probe, probe)
    else {
        return false;
    };
    let mut n = Some(node);
    while let Some(x) = n {
        if cfg.skip_kinds.contains(&x.kind()) {
            return true;
        }
        n = x.parent();
    }
    false
}

/// Patch a sentinel at `cursor`, re-parse, and return the receiver of the
/// member access the sentinel completed. `None` when the cursor is not at a
/// member access (or sits in a string/comment).
///
/// `parser` must already be set to the target language; `cfg` selects
/// the per-language node vocabulary.
#[cfg(test)]
pub fn receiver_at(
    parser: &mut Parser,
    cfg: &crate::build::query_extract::LangPack,
    src: &str,
    cursor: usize,
) -> Option<Receiver> {
    let orig = parser.parse(src, None)?;
    if cursor_in_skip(&orig, src, cursor, cfg) {
        return None;
    }
    let patched = patch(src, cursor);
    let tree = parser.parse(&patched, None)?;
    let node = find_sentinel(tree.root_node(), &patched, cursor)?;
    let member = climb_to_member(node, cfg)?;
    receiver_of(member, &patched, cursor)
}

/// Find the freshly-spliced sentinel identifier node. It begins exactly
/// at `cursor` and its text is `SENTINEL`. We descend to the smallest
/// node covering the sentinel byte range and accept it (or its first
/// matching ancestor) whose text equals the sentinel.
fn find_sentinel<'a>(root: Node<'a>, patched: &str, cursor: usize) -> Option<Node<'a>> {
    let end = cursor + SENTINEL.len();
    let mut node = root.descendant_for_byte_range(cursor, end)?;
    // Descend to the leaf token carrying the sentinel. Completing IN PLACE of
    // an existing `->member` (`o->|op_type`, the common "edit a member" case)
    // patches to `o->__CURSOR__op_type`, so the sentinel is MERGED into the
    // identifier — accept the leaf that CONTAINS it, not only an exact
    // `__CURSOR__` token (which only happens at a bare trailing `->`).
    while node.child_count() > 0 {
        let mut cur = node.walk();
        let child = node
            .children(&mut cur)
            .find(|c| c.start_byte() <= cursor && c.end_byte() >= end);
        match child {
            Some(c) => node = c,
            None => return None,
        }
    }
    node.utf8_text(patched.as_bytes())
        .ok()
        .filter(|t| t.contains(SENTINEL))
        .map(|_| node)
}

/// Walk up from the sentinel to the member-access node that owns it.
fn climb_to_member<'a>(node: Node<'a>, cfg: &crate::build::query_extract::LangPack) -> Option<Node<'a>> {
    let mut n = node;
    for _ in 0..6 {
        let parent = n.parent()?;
        if cfg.member_kinds.contains(&parent.kind()) {
            return Some(parent);
        }
        n = parent;
    }
    None
}

/// Extract the receiver (first named child) of a member-access node and
/// map its span back to original coordinates. Since the receiver lies
/// entirely before `cursor`, patched and original offsets coincide.
#[cfg(test)]
fn receiver_of(member: Node, patched: &str, cursor: usize) -> Option<Receiver> {
    let receiver = member.named_child(0)?;
    // Defensive: the receiver must end at or before the splice site.
    if receiver.end_byte() > cursor {
        return None;
    }
    let arrow = member_uses_arrow(member, patched);
    Some(Receiver {
        text: receiver.utf8_text(patched.as_bytes()).ok()?.to_string(),
        start: receiver.start_byte(),
        end: receiver.end_byte(),
        arrow,
    })
}

/// The `.`/`->` operator token of a member-access node (the anonymous
/// child between the two named children). It lies entirely before the
/// splice site, so its span is identical in patched and original source.
fn operator_token<'a>(member: Node<'a>, patched: &str) -> Option<Node<'a>> {
    let mut cur = member.walk();
    let found = member.children(&mut cur).find(|c| {
        !c.is_named() && matches!(c.utf8_text(patched.as_bytes()), Ok(".") | Ok("->"))
    });
    found
}

/// Detect `->` vs `.` by scanning the member node's anonymous children.
#[cfg(test)]
fn member_uses_arrow(member: Node, patched: &str) -> bool {
    operator_token(member, patched).map(|t| t.utf8_text(patched.as_bytes()) == Ok("->")).unwrap_or(false)
}

/// Incremental variant: reuse a cached `old` tree of the unpatched
/// buffer. The splice is a pure insertion at `cursor`, so the
/// `InputEdit` is exact and tree-sitter re-parses only the damaged
/// region around the cursor — the cost completion actually pays per
/// keystroke once the document tree is already in hand (it always is:
/// `document.rs` keeps it). Same recovery, a fraction of the work.
#[cfg(test)]
pub fn receiver_at_incremental(
    parser: &mut Parser,
    cfg: &crate::build::query_extract::LangPack,
    src: &str,
    old: &Tree,
    cursor: usize,
) -> Option<Receiver> {
    if cursor_in_skip(old, src, cursor, cfg) {
        return None;
    }
    let patched = patch(src, cursor);
    let mut edited = old.clone();
    let pos = byte_to_point(src, cursor);
    edited.edit(&InputEdit {
        start_byte: cursor,
        old_end_byte: cursor,
        new_end_byte: cursor + SENTINEL.len(),
        start_position: pos,
        old_end_position: pos,
        new_end_position: Point::new(pos.row, pos.column + SENTINEL.len()),
    });
    let tree = parser.parse(&patched, Some(&edited))?;
    let node = find_sentinel(tree.root_node(), &patched, cursor)?;
    let member = climb_to_member(node, cfg)?;
    receiver_of(member, &patched, cursor)
}

/// The completion context at a dangling member access: the receiver's type
/// (for listing members) plus, when the typed operator disagrees with the
/// receiver's pointer depth, the single-level `.`↔`->` fix to swap it. One
/// reparse serves both — completion pays it once per keystroke.
///
/// `op_fix = Some((span, text))` means "replace the operator token at `span`
/// with `text`". `None` when the operator is already correct, the receiver
/// isn't a simple variable, or the depth is DEEP (`Box**` → `(*pp)->`, an
/// expression wrap we don't auto-apply — members still complete, show-only).
pub struct MemberCompletionCtx {
    pub receiver_type: Option<InferredType>,
    pub op_fix: Option<(Span, String)>,
    /// The operator actually typed (`.` vs `->`), regardless of whether
    /// `op_fix` corrects it. `Slot::Member.op`'s pack-side answer
    /// (`docs/adr/cursor-slots.md`).
    pub op: crate::model::file_analysis::MemberOp,
    /// A SCOPED access (`::` — no `.`/`->` token on the member node): the
    /// class's constants and static members are what completes there.
    pub scoped: bool,
}

pub fn member_completion_ctx_incremental(
    parser: &mut Parser,
    cfg: &crate::build::query_extract::LangPack,
    src: &str,
    old: &Tree,
    cursor: usize,
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
) -> Option<MemberCompletionCtx> {
    if cursor_in_skip(old, src, cursor, cfg) {
        return None;
    }
    let patched = patch(src, cursor);
    let mut edited = old.clone();
    let pos = byte_to_point(src, cursor);
    edited.edit(&InputEdit {
        start_byte: cursor,
        old_end_byte: cursor,
        new_end_byte: cursor + SENTINEL.len(),
        start_position: pos,
        old_end_position: pos,
        new_end_position: Point::new(pos.row, pos.column + SENTINEL.len()),
    });
    let tree = parser.parse(&patched, Some(&edited))?;
    let node = find_sentinel(tree.root_node(), &patched, cursor)?;
    let member = climb_to_member(node, cfg)?;
    let receiver = member.named_child(0)?;
    // Downstream projects `class_name()` without an index, so the
    // exact-spelling-vs-primary dispatch call (a spec class exists for
    // `formatter<int>`) is made HERE, while the index is in hand.
    // A receiver spelled as the language's receiver keyword (`this`,
    // `$this`) has no typeable value node — it IS the enclosing class,
    // read off the cursor's scope chain: self-access member completion
    // (privates included, inheritance-aware) instead of a scope dump.
    let receiver_type = resolve_node_type(receiver, cfg, &patched, analysis, module_index)
        .or_else(|| {
            let txt = receiver.utf8_text(patched.as_bytes()).ok()?;
            // `self::` / `static::` name the enclosing class the same way
            // `$this->` does.
            if !cfg.receiver_names.contains(&txt) && !cfg.self_class_tokens.contains(&txt) {
                return None;
            }
            let sc = analysis.scope_at(byte_to_point(src, cursor))?;
            analysis
                .enclosing_class_for_scope(sc)
                .map(crate::model::file_analysis::InferredType::ClassName)
        })
        .or_else(|| {
            // A bare class token (`Foo::`, `App\Foo::`) IS the class it
            // spells, leaf-keyed like every class identity.
            if !cfg.class_token_kinds.contains(&receiver.kind()) {
                return None;
            }
            let txt = receiver.utf8_text(patched.as_bytes()).ok()?;
            let leaf = txt.rsplit('\\').next().unwrap_or(txt);
            (!leaf.is_empty()).then(|| crate::model::file_analysis::InferredType::ClassName(leaf.to_string()))
        })
        .map(|t| analysis.refine_instance_dispatch(t, module_index));
    let op_fix = operator_fix(member, receiver, &patched, analysis, cfg);
    let op = typed_member_op(member, &patched);
    let scoped = operator_token(member, &patched).is_none();
    Some(MemberCompletionCtx { receiver_type, op_fix, op, scoped })
}

/// The domain-comparison completion context: the cursor sits after an
/// equality operator (`o->op_type == |`) whose OTHER operand is a
/// domain-typed field. Returns the field's DOMAIN as `ClassName(enum)` so
/// completion ranks that enum's members first (`docs/adr/cursor-slots.md`
/// — `Slot::expected_type`'s reserved comparison semantics). `None` when
/// the cursor isn't at such a comparison, the field operand doesn't
/// resolve, or the field carries no recovered domain.
///
/// Shares the member probe's splice/reparse: the sentinel completes the
/// dangling value operand, so the comparison becomes syntactically whole
/// and the field slot (which lies entirely before the cursor) types
/// exactly as it does in finished source.
pub fn domain_compare_ctx_incremental(
    parser: &mut Parser,
    cfg: &crate::build::query_extract::LangPack,
    src: &str,
    old: &Tree,
    cursor: usize,
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
) -> Option<InferredType> {
    if cfg.domain_compare_kinds.is_empty() || cfg.domain_compare_ops.is_empty() {
        return None;
    }
    if cursor_in_skip(old, src, cursor, cfg) {
        return None;
    }
    let patched = patch(src, cursor);
    let mut edited = old.clone();
    let pos = byte_to_point(src, cursor);
    edited.edit(&InputEdit {
        start_byte: cursor,
        old_end_byte: cursor,
        new_end_byte: cursor + SENTINEL.len(),
        start_position: pos,
        old_end_position: pos,
        new_end_position: Point::new(pos.row, pos.column + SENTINEL.len()),
    });
    let tree = parser.parse(&patched, Some(&edited))?;
    let node = find_sentinel(tree.root_node(), &patched, cursor)?;
    let cmp = climb_to_domain_compare(node, cfg, &patched)?;
    let slot = domain_slot_operand(cmp, cfg, cursor)?;
    // The field slot is `recv OP field` — recover the receiver's class and
    // the field name, then ask usage what enum this field is used AS.
    let base = slot.named_child(0)?;
    let field = slot.named_child(slot.named_child_count() - 1)?;
    let base_ty = resolve_node_type(base, cfg, &patched, analysis, module_index)?;
    let class = base_ty.class_name()?;
    let field_name = field.utf8_text(patched.as_bytes()).ok()?;
    let dom = analysis.field_domain(class, field_name, module_index)?;
    Some(InferredType::ClassName(dom.domain))
}

/// Climb from the sentinel to the enclosing equality comparison — a
/// `domain_compare_kinds` node whose operator is one of the pack's
/// `domain_compare_ops`. The operator gate keeps `<`/`+`/arithmetic
/// binaries from opening the slot.
fn climb_to_domain_compare<'a>(
    node: Node<'a>,
    cfg: &crate::build::query_extract::LangPack,
    patched: &str,
) -> Option<Node<'a>> {
    let mut n = node;
    for _ in 0..6 {
        let parent = n.parent()?;
        if cfg.domain_compare_kinds.contains(&parent.kind())
            && comparison_uses_domain_op(parent, cfg, patched)
        {
            return Some(parent);
        }
        n = parent;
    }
    None
}

/// True when the comparison's operator token is a domain-comparison
/// operator. The operator is an anonymous child between the operands.
fn comparison_uses_domain_op(
    cmp: Node,
    cfg: &crate::build::query_extract::LangPack,
    patched: &str,
) -> bool {
    (0..cmp.child_count()).filter_map(|i| cmp.child(i)).any(|ch| {
        !ch.is_named()
            && ch
                .utf8_text(patched.as_bytes())
                .map(|t| cfg.domain_compare_ops.contains(&t))
                .unwrap_or(false)
    })
}

/// The comparison operand that is a member access (`o->op_type`) — the
/// domain-typed field slot, as opposed to the value operand holding the
/// spliced sentinel. The cursor-containment guard is belt-and-suspenders:
/// the value operand parses as a bare identifier, never a member access.
fn domain_slot_operand<'a>(
    cmp: Node<'a>,
    cfg: &crate::build::query_extract::LangPack,
    cursor: usize,
) -> Option<Node<'a>> {
    (0..cmp.named_child_count())
        .filter_map(|i| cmp.named_child(i))
        .find(|n| {
            cfg.member_kinds.contains(&n.kind())
                && !(n.start_byte() <= cursor && cursor < n.end_byte())
        })
}

/// The operator token actually written at a member access — `.` unless
/// the anonymous child between the two named children spells `->`.
fn typed_member_op(member: Node, patched: &str) -> crate::model::file_analysis::MemberOp {
    match operator_token(member, patched).and_then(|t| t.utf8_text(patched.as_bytes()).ok()) {
        Some("->") => crate::model::file_analysis::MemberOp::Arrow,
        _ => crate::model::file_analysis::MemberOp::Dot,
    }
}

/// The operator correction for a member access whose receiver is a simple
/// variable. Drives entirely off the receiver's `deref_stack` (rule #10):
/// the depth picks the expected operator, and we offer the swap only when
/// it differs from what was typed AND a single token expresses it.
fn operator_fix(
    member: Node,
    receiver: Node,
    patched: &str,
    analysis: &FileAnalysis,
    cfg: &crate::build::query_extract::LangPack,
) -> Option<(Span, String)> {
    if !cfg.simple_var_kinds.contains(&receiver.kind()) {
        return None; // only simple-variable receivers carry a resolvable stack
    }
    let name = receiver.utf8_text(patched.as_bytes()).ok()?;
    let stack = analysis.var_deref_stack_at(name, receiver.start_position())?;
    let expected = expected_member_op(stack)?; // None = DEEP → show-only
    let op = operator_token(member, patched)?;
    let typed_arrow = op.utf8_text(patched.as_bytes()) == Ok("->");
    let expected_arrow = expected == crate::model::file_analysis::MemberOp::Arrow;
    if typed_arrow == expected_arrow {
        return None; // already correct
    }
    let span = Span { start: op.start_position(), end: op.end_position() };
    Some((span, expected.as_str().to_string()))
}

/// Type a receiver node. A member-access node (`field_expression` /
/// `attribute`) is field-on-class — recurse the base, look the field up on
/// its class; anything else (an identifier, a call) resolves by its exact
/// span through the bag (`expr_type_at_span`).
fn resolve_node_type(
    node: Node,
    cfg: &crate::build::query_extract::LangPack,
    src: &str,
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
) -> Option<InferredType> {
    // a member ACCESS `recv.field` — the member's value on the receiver
    // (`member_value_type`: dispatch ladder + receiver-threaded method
    // return, falling back to the field's declared type with template
    // params substituted).
    if cfg.member_kinds.contains(&node.kind()) {
        let base = node.named_child(0)?;
        let field = node.named_child(node.named_child_count() - 1)?;
        let base_ty = resolve_node_type(base, cfg, src, analysis, module_index)?;
        let field_name = field.utf8_text(src.as_bytes()).ok()?;
        // a member READ: the value shape (a strict pack never answers it
        // with a same-named method)
        return analysis.member_value_type(&base_ty, field_name, module_index, None, MemberShape::Value);
    }
    // a method CALL `recv.method(...)` — the method's return on the
    // receiver's class, resolved through PackageSymbol (inheritance +
    // cross-file flow through the same chase, no special-casing). The
    // receiver's full value threads through so a param-shaped return
    // (`T get()`) substitutes the instance's args.
    if cfg.call_kinds.contains(&node.kind()) {
        let func = node.child_by_field_name("function")?;
        if cfg.member_kinds.contains(&func.kind()) {
            let recv = func.named_child(0)?;
            let method = func.named_child(func.named_child_count() - 1)?;
            let recv_ty = resolve_node_type(recv, cfg, src, analysis, module_index)?;
            let method_name = method.utf8_text(src.as_bytes()).ok()?;
            return analysis.member_value_type(&recv_ty, method_name, module_index, None, MemberShape::Callable);
        }
        // A plain call (`make_widget()`, a ctor-on-temporary `Box()`) —
        // `function` isn't member-shaped, so there's no receiver to recurse
        // onto. Falls through to the exact-span lookup below, which chases
        // the call's own return through the bag's call-root arm.
    }
    // Transparent wrappers — `(expr)`, `*p`, `&obj` — denote the same class
    // as their operand (pointer-/reference-ness dropped). Peel and recurse so
    // `(*p).m` / `(&o)->m` reach the members `p->m` does.
    if cfg.recv_peel.wrappers.iter().any(|(k, _)| *k == node.kind()) {
        return resolve_node_type(node.named_child(0)?, cfg, src, analysis, module_index);
    }
    // Implicit-`this` member receiver: a bare identifier (`iter_->`, `mem_->`,
    // `options_.`) with NO local declaration IS `this->name` where the pack
    // elides the receiver (`implicit_this_members`). Resolve it on the
    // enclosing class — `member_value_type` runs the SAME dispatch ladder +
    // cross-file field lookup member access uses, so a field declared in the
    // class's header (an out-of-line method body reads it cross-file) resolves
    // exactly like a same-file one. Taken AHEAD of the span/bag path on
    // purpose: a member reassignment (`prog_ = f(...*2/3)`) leaves phantom
    // "local" flow witnesses on the name (here typing it `Numeric` off a buried
    // literal) that would otherwise shadow the true member type. A genuine
    // local/param keeps its flow-narrowed value (it has a Variable symbol, so
    // this branch is skipped). Gated on the pack capability so Python/R
    // (mandatory receiver) never treat a bare name as a member.
    if cfg.implicit_this_members && cfg.simple_var_kinds.contains(&node.kind()) {
        if let Ok(name) = node.utf8_text(src.as_bytes()) {
            if !analysis.has_local_variable_at(name, node.start_position()) {
                if let Some(t) =
                    analysis.implicit_receiver_class_at(node.start_position()).and_then(|class| {
                        analysis.member_value_type(
                            &InferredType::ClassName(class),
                            name,
                            module_index,
                            None,
                            MemberShape::Value,
                        )
                    })
                {
                    return Some(t);
                }
            }
        }
    }
    let span = Span { start: node.start_position(), end: node.end_position() };
    analysis.expr_type_at_span(span, module_index)
}

fn byte_to_point(src: &str, byte: usize) -> Point {
    let mut row = 0;
    let mut col = 0;
    for (i, ch) in src.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += ch.len_utf8();
        }
    }
    Point::new(row, col)
}

#[cfg(test)]
#[path = "cursor_sentinel_tests.rs"]
mod tests;

/// A call site the cursor sits in: the callee token and the active argument.
#[derive(Debug, Clone, PartialEq)]
pub struct PackCallSite {
    /// The callee token (a member call's method name, a function call's
    /// last name segment, a `new` expression's class name) in original
    /// coordinates.
    pub callee: Span,
    pub active_param: usize,
}

/// The innermost pack-declared call expression whose argument list holds
/// the cursor (`docs/adr/cursor-slots.md`'s ArgPosition for pack languages).
/// Walks the ORIGINAL tree — no sentinel splice: the arguments are what
/// the user has typed so far, and a cursor right after `(` or `,` is
/// inside the list by construction.
pub fn call_at(tree: &Tree, cfg: &crate::build::query_extract::LangPack, src: &str, cursor: usize) -> Option<PackCallSite> {
    if cfg.call_shapes.is_empty() {
        return None;
    }
    let root = tree.root_node();
    let at = cursor.min(src.len());
    let mut node = root.descendant_for_byte_range(at.saturating_sub(1), at)?;
    loop {
        if let Some(shape) = cfg.call_shapes.iter().find(|c| c.kind == node.kind()) {
            if let Some(args) = node.child_by_field_name(shape.args_field) {
                if args.start_byte() < cursor && cursor <= args.end_byte() {
                    let callee = if shape.callee_field.is_empty() {
                        // `new Foo(...)`: the class token is the first
                        // named child that is not the argument list.
                        (0..node.named_child_count())
                            .filter_map(|i| node.named_child(i))
                            .find(|c| c.id() != args.id())
                    } else {
                        node.child_by_field_name(shape.callee_field)
                    }?;
                    let tok = last_name_token(callee);
                    let mut active = 0usize;
                    for i in 0..args.named_child_count() {
                        let Some(a) = args.named_child(i) else { continue };
                        if !cfg.arg_kind.is_empty() && a.kind() != cfg.arg_kind {
                            continue;
                        }
                        if a.start_byte() <= cursor && cursor <= a.end_byte() {
                            break;
                        }
                        if a.end_byte() < cursor {
                            active += 1;
                        }
                    }
                    return Some(PackCallSite {
                        callee: Span { start: tok.start_position(), end: tok.end_position() },
                        active_param: active,
                    });
                }
            }
        }
        node = node.parent()?;
    }
}

/// The token a callee node names: itself when it is a leaf, else its last
/// named leaf (`A\B\f` → `f`, `$this->m` → `m`).
/// One argument of a pack call site: its span and text, and the shapes
/// that end positional matching (a named argument, a spread, a callable
/// placeholder).
pub struct PackArg {
    pub span: Span,
    pub text: String,
    pub named: bool,
    pub spread: bool,
}

/// A pack call site with its arguments in source order.
pub struct PackCallArgs {
    pub callee: Span,
    pub args: Vec<PackArg>,
}

/// Every pack-declared call expression whose callee token sits on one of
/// `rows`, with its arguments — the same shapes `call_at` reads at a
/// cursor, walked over a range for the hint lanes.
pub fn calls_in_rows(
    tree: &Tree,
    cfg: &crate::build::query_extract::LangPack,
    src: &str,
    rows: std::ops::RangeInclusive<usize>,
) -> Vec<PackCallArgs> {
    let mut out = Vec::new();
    if cfg.call_shapes.is_empty() {
        return out;
    }
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.end_position().row < *rows.start() || node.start_position().row > *rows.end() {
            continue;
        }
        if let Some(shape) = cfg.call_shapes.iter().find(|c| c.kind == node.kind()) {
            if let Some(args) = node.child_by_field_name(shape.args_field) {
                let callee = if shape.callee_field.is_empty() {
                    (0..node.named_child_count())
                        .filter_map(|i| node.named_child(i))
                        .find(|c| c.id() != args.id())
                } else {
                    node.child_by_field_name(shape.callee_field)
                };
                if let Some(callee) = callee {
                    let tok = last_name_token(callee);
                    if rows.contains(&tok.start_position().row) {
                        let ends_positional = |n: Node<'_>| {
                            (!cfg.spread_arg_kind.is_empty() && n.kind() == cfg.spread_arg_kind)
                                || (!cfg.callable_placeholder_kind.is_empty()
                                    && n.kind() == cfg.callable_placeholder_kind)
                        };
                        let mut list = Vec::new();
                        for i in 0..args.named_child_count() {
                            let Some(a) = args.named_child(i) else { continue };
                            if !cfg.arg_kind.is_empty() && a.kind() != cfg.arg_kind {
                                continue;
                            }
                            let named = !cfg.named_arg_field.is_empty()
                                && a.child_by_field_name(cfg.named_arg_field).is_some();
                            let spread = ends_positional(a) || a.named_child(0).is_some_and(ends_positional);
                            list.push(PackArg {
                                span: Span { start: a.start_position(), end: a.end_position() },
                                text: src.get(a.start_byte()..a.end_byte()).unwrap_or("").to_string(),
                                named,
                                spread,
                            });
                        }
                        out.push(PackCallArgs {
                            callee: Span { start: tok.start_position(), end: tok.end_position() },
                            args: list,
                        });
                    }
                }
            }
        }
        for i in (0..node.named_child_count()).rev() {
            if let Some(c) = node.named_child(i) {
                stack.push(c);
            }
        }
    }
    out.sort_by_key(|c| (c.callee.start.row, c.callee.start.column));
    out
}

fn last_name_token(n: Node<'_>) -> Node<'_> {
    let mut cur = n;
    while cur.named_child_count() > 0 {
        let Some(last) = (0..cur.named_child_count()).rev().filter_map(|i| cur.named_child(i)).next() else { break };
        cur = last;
    }
    cur
}

