//! `bless` semantics: promotion of a plain ref to an instance
//! (`bless $ref, $class` → `ClassName`), class-argument resolution, and
//! the receiver-polymorphic constructor idiom (`bless X, ref $x || $x`).

use super::*;

impl<'a> Builder<'a> {
    pub(super) fn is_bless_call(&self, node: Node<'a>) -> bool {
        let kind = node.kind();
        if kind == "function_call_expression" || kind == "ambiguous_function_call_expression" {
            if let Some(func) = node.child_by_field_name("function") {
                return func.utf8_text(self.source).ok() == Some("bless");
            }
        }
        false
    }

    /// `bless $ref, $class` promotes `$ref`'s type from its hashref/
    /// arrayref rep to `ClassName($class)` — after the bless the value IS
    /// an instance, so `$self->method` resolves. We push a `ClassName` TC
    /// on the first arg scoped at the bless statement; the temporal fold
    /// makes it win for queries past this point. Honest-miss when the class
    /// isn't statically determinable (a computed `$class` that doesn't
    /// resolve to a class name) — no TC, the value keeps its rep type.
    ///
    /// Class resolution for the 2nd arg: `$class`/`shift`/`$_[0]` →
    /// enclosing class via the bag's FirstParam projection; `__PACKAGE__`
    /// → current package; a string/bareword literal → its text; a missing
    /// 2nd arg (`bless {}`) → current package (Perl's one-arg bless blesses
    /// into the caller's package).
    pub(super) fn visit_bless_call(&mut self, node: Node<'a>) {
        if let Some(p) = self.current_package.clone() {
            self.blessing_packages.insert(p);
        }
        let args = self.extract_call_args(node);
        let obj = match args.first() {
            Some(n) => *n,
            None => return,
        };
        // Receiver-polymorphic statement bless — the Bugzilla::Object idiom
        // (`bless($object, $class) if $object; return $object;`). The class
        // is the CALL SITE's receiver, not a name resolvable here, so a
        // concrete TC can't carry it: push the deferred witness on the
        // VARIABLE (see `push_receiver_bless_witness`). The concrete TC
        // below still fires when the class ALSO resolves statically
        // (`$class` from `shift` → enclosing class via FirstParam): that
        // concrete witness is the in-body baseline; the deferred one wins
        // at real call sites via reducer order.
        if obj.kind() == "scalar" {
            if let Ok(text) = obj.utf8_text(self.source) {
                let text = text.to_string();
                self.push_receiver_bless_witness(&text, node);
            }
        }
        let class = match args.get(1) {
            Some(class_node) => match self.bless_class_of(*class_node) {
                Some(c) => c,
                None => return, // honest miss — class undeterminable
            },
            // One-arg `bless {}` blesses into the current package.
            None => match self.current_package.clone() {
                Some(p) => p,
                None => return,
            },
        };
        self.push_var_type_constraint(obj, node, InferredType::ClassName(class));
    }

    /// If `bless_node` is a bless whose class argument denotes the RECEIVER
    /// (`bless X, $class` / `ref $x || $x` — `bless_class_is_receiver`),
    /// push the deferred `ReturnExpr::ReceiverOr(enclosing)` witness on
    /// `var_name` — the receiver-polymorphic ctor's variable half. A
    /// concrete TC can't carry "the call site's class"; the deferred
    /// payload rides the variable, the return-arm chase reaches it through
    /// `Edge(Variable)` with the caller's receiver threaded
    /// (`query_variable_with_visited`), and `ReturnExprReducer` substitutes
    /// at each call site — so an inherited `$class->new` types to the
    /// subclass it was called on, cross-file included. Temporal: the
    /// witness carries the bless span; pre-bless queries keep the rep type.
    /// Returns whether a witness was pushed.
    pub(super) fn push_receiver_bless_witness(&mut self, var_name: &str, bless_node: Node<'a>) -> bool {
        if !self.is_bless_call(bless_node) {
            return false;
        }
        let args = self.extract_call_args(bless_node);
        let Some(class_node) = args.get(1) else { return false };
        if !self.bless_class_is_receiver(*class_node) {
            return false;
        }
        let re = match self.current_package.clone() {
            Some(pkg) => {
                crate::model::witnesses::ReturnExpr::ReceiverOr(InferredType::ClassName(pkg))
            }
            None => crate::model::witnesses::ReturnExpr::Receiver,
        };
        self.bag.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable {
                name: var_name.to_string(),
                scope: self.current_scope(),
            },
            source: crate::model::witnesses::WitnessSource::Builder("bless_receiver".into()),
            payload: crate::model::witnesses::WitnessPayload::ReturnExpr(re),
            span: node_to_span(bless_node),
        });
        true
    }

    /// Does this `bless`-class argument denote the RECEIVER (the call-site
    /// invocant) rather than a fixed class? Matches the receiver-polymorphic
    /// constructor idiom: a bare invocant (`shift` / `$self` / `$class` /
    /// `$_[0]`), `ref <invocant>`, or `<a> || <b>` / `<a> // <b>` where a side
    /// denotes the receiver (`ref $class || $class`). A string / bareword /
    /// `__PACKAGE__` is NOT the receiver — it blesses into a fixed class. When
    /// true the bless returns `ReturnExpr::ReceiverOr`, so an inherited ctor /
    /// `SUPER::new` chain types to whatever subclass it was called on.
    pub(super) fn bless_class_is_receiver(&self, node: Node<'a>) -> bool {
        if self.is_shift_call(node) {
            return true;
        }
        match node.kind() {
            "scalar" => crate::cst::is_conventional_invocant_scalar(node, self.source),
            "array_element_expression" => self.is_positional_receiver(node),
            // `ref <invocant>`
            "func1op_call_expression" => {
                node.child(0).and_then(|c| c.utf8_text(self.source).ok()) == Some("ref")
                    && node
                        .named_child(0)
                        .map_or(false, |op| self.bless_class_is_receiver(op))
            }
            // `<a> || <b>` / `<a> // <b>` — receiver if either side is.
            "binary_expression" => {
                matches!(self.get_operator_text(node).as_deref(), Some("||") | Some("//"))
                    && (0..node.named_child_count())
                        .filter_map(|i| node.named_child(i))
                        .any(|c| self.bless_class_is_receiver(c))
            }
            _ => false,
        }
    }

    /// Resolve a `bless`'s class argument to a class name. String/bareword
    /// literals read directly; everything else routes through the same
    /// invocant-class resolver used for method receivers (so `$class` from
    /// `shift`, `__PACKAGE__`, etc. resolve identically).
    pub(super) fn bless_class_of(&self, class_node: Node<'a>) -> Option<String> {
        // `__PACKAGE__` parses as a func0op call here (not a bareword), so
        // `invocant_type_at_node`'s bareword arm doesn't catch it — resolve
        // it to the enclosing package directly.
        if class_node.utf8_text(self.source).ok().is_some_and(crate::model::conventions::is_current_package_token) {
            return self.package_for_node(class_node);
        }
        // `bless $r, ref $x` (the clone idiom) blesses into `$x`'s class: the
        // 2nd arg `ref EXPR` yields EXPR's class *name*, so the bless target is
        // EXPR's resolved class. `ref EXPR` as a general value is a String, so
        // this unwrap lives here (the class slot) rather than in the generic
        // invocant resolver.
        if class_node.kind() == "func1op_call_expression"
            && class_node.child(0).and_then(|c| c.utf8_text(self.source).ok()) == Some("ref")
        {
            return class_node
                .named_child(0)
                .and_then(|operand| self.resolve_invocant_class_tree(operand));
        }
        if let Some(s) = self.literal_arg_string(class_node) {
            if !s.is_empty() {
                return Some(s);
            }
        }
        self.resolve_invocant_class_tree(class_node)
    }
}
