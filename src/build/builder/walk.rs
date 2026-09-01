//! The walk driver: CST descent costs heap, not native stack.
//!
//! `visit_node` dispatches one node and *queues* whatever it wants walked
//! next; `drive_walk` pops that queue until it drains. Depth therefore grows
//! a `Vec`, not the call stack — which matters because a stack overflow is a
//! fatal abort no `catch_unwind` can net, so a single deeply-nested generated
//! file would otherwise take the whole server down (see `MAX_CST_DEPTH`).
//!
//! **Tasks pop in reverse push order.** Push the work that must run LAST
//! first. The `*_then` combinators encode that inversion so visitors don't
//! have to: they read top-to-bottom as "walk this subtree, then do that".
//!
//! The recursive descent survives under `cfg(test)` so the two walks can be
//! held against each other: `walk_equivalence_over_repo_fixtures` compares
//! them over every Perl file in the repo on an ordinary test run, and
//! `PERL_LSP_WALK_EQUIV=1` widens that to every file the suite touches.
//! Every primitive below pairs its queueing branch with the exact recursive
//! statement it replaced — that pairing IS the equivalence argument.
//!
//! The comparison is on the serde projection, NOT on bytes: `HashMap`
//! iteration order differs between two builds in one thread, so equal
//! analyses do not serialize equal. See `assert_walks_agree`.

use super::*;

/// One unit of pending walk work.
pub(super) enum WalkTask<'a> {
    /// Dispatch a node through `visit_node`.
    Visit(Node<'a>),
    /// Work a visitor deferred until its subtree finished. Captures owned
    /// data only (`Node` is `Copy`) — never a borrow of the builder.
    Then(Box<dyn FnOnce(&mut Builder<'a>) + 'a>),
}

/// True when the env gate asks for the pre-worklist recursive descent.
/// Read once per build, not per node.
#[cfg(test)]
#[cfg(test)]
pub(super) fn recursive_walk_requested() -> bool {
    std::env::var_os("PERL_LSP_RECURSIVE_WALK").is_some_and(|v| v == "1")
}

thread_local! {
    /// Per-thread walk-mode override for the equivalence harness, which
    /// builds the same tree both ways and compares the serialized result.
    /// A thread-local (not the env gate) so the two builds can run in one
    /// process without racing every other test thread.
    static FORCED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn recursive_walk_forced() -> Option<bool> {
    FORCED.with(|f| f.get())
}

/// Run `f` with the walk mode pinned. Restores the previous setting after.
#[cfg(test)]
pub(crate) fn with_walk_mode<R>(recursive: bool, f: impl FnOnce() -> R) -> R {
    let prev = FORCED.with(|c| c.replace(Some(recursive)));
    let out = f();
    FORCED.with(|c| c.set(prev));
    out
}

impl<'a> Builder<'a> {
    /// Walk `root`'s subtree to completion.
    pub(super) fn drive_walk(&mut self, root: Node<'a>) {
        self.queue_children(root);
        while let Some(task) = self.walk_stack.pop() {
            match task {
                WalkTask::Visit(node) => self.visit_node(node),
                WalkTask::Then(work) => work(self),
            }
        }
    }

    // ---- Queueing primitives ----

    /// Walk `node`'s children, in source order.
    pub(super) fn queue_children(&mut self, node: Node<'a>) {
        #[cfg(test)]
        if self.recursive_walk {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    self.visit_node(child);
                }
            }
            return;
        }
        // Reversed: the driver pops from the end, so child 0 must go on last.
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                self.walk_stack.push(WalkTask::Visit(child));
            }
        }
    }

    /// Walk one node.
    pub(super) fn queue_node(&mut self, node: Node<'a>) {
        #[cfg(test)]
        if self.recursive_walk {
            self.visit_node(node);
            return;
        }
        self.walk_stack.push(WalkTask::Visit(node));
    }

    /// Run `work` when the walk reaches this point.
    ///
    /// Raw primitive: it pushes, so it runs AFTER everything queued after it.
    /// Reach for `queue_children_then` / `queue_node_then` unless the shape is
    /// genuinely descend-work-descend (`visit_assignment`).
    pub(super) fn queue_then(&mut self, work: impl FnOnce(&mut Builder<'a>) + 'a) {
        #[cfg(test)]
        if self.recursive_walk {
            work(self);
            return;
        }
        self.walk_stack.push(WalkTask::Then(Box::new(work)));
    }

    /// Run `steps` in order, each to completion before the next starts.
    ///
    /// For a visitor that dispatches several children from a loop: collect
    /// the steps in source order and hand them over, rather than queueing
    /// inside the loop (which would need reversing, and would then read
    /// backwards in both modes).
    pub(super) fn queue_sequence(&mut self, steps: Vec<Box<dyn FnOnce(&mut Builder<'a>) + 'a>>) {
        #[cfg(test)]
        if self.recursive_walk {
            for step in steps {
                step(self);
            }
            return;
        }
        for step in steps.into_iter().rev() {
            self.walk_stack.push(WalkTask::Then(step));
        }
    }

    // ---- Combinators ----

    /// Walk `node`'s children, then run `work`.
    ///
    /// The reason this exists: a visitor that closed a scope, restored a
    /// package, or published a witness *after* `visit_children` returned
    /// cannot just call `queue_children` and fall through — the children
    /// have not run yet at that point.
    pub(super) fn queue_children_then(
        &mut self,
        node: Node<'a>,
        work: impl FnOnce(&mut Builder<'a>) + 'a,
    ) {
        #[cfg(test)]
        if self.recursive_walk {
            self.queue_children(node);
            work(self);
            return;
        }
        self.queue_then(work);
        self.queue_children(node);
    }

    /// Walk `node`, then run `work`.
    pub(super) fn queue_node_then(
        &mut self,
        node: Node<'a>,
        work: impl FnOnce(&mut Builder<'a>) + 'a,
    ) {
        #[cfg(test)]
        if self.recursive_walk {
            self.queue_node(node);
            work(self);
            return;
        }
        self.queue_then(work);
        self.queue_node(node);
    }
}

/// Deepest tree the equivalence check will compare.
///
/// The check runs the recursive descent, so it inherits that descent's limit —
/// past roughly 400 levels a debug build overflows a 2 MiB stack, which is a
/// process abort, not a test failure. Files deeper than this are compared by
/// `deep_file_gets_a_real_analysis` instead, which is the test that only the
/// iterative walk can pass at all. 256 is comfortably under the debug ceiling
/// and well above every source in the suite (the deepest real Perl in 138,806
/// CPAN files is 247 levels).
#[cfg(test)]
pub(super) const MAX_COMPARABLE_DEPTH: usize = 256;

/// `PERL_LSP_WALK_EQUIV=1` — build every file both ways and compare.
#[cfg(test)]
pub(super) fn equivalence_check_enabled() -> bool {
    std::env::var_os("PERL_LSP_WALK_EQUIV").is_some_and(|v| v == "1")
}

/// Panic unless the iterative and recursive walks produced the same
/// `FileAnalysis`, naming the first field that differs.
///
/// Compares the serde projection rather than bincode bytes: `serde_json`
/// renders maps through a `BTreeMap`, so `HashMap` iteration order — which
/// differs between two builds in one thread, since each map gets its own
/// `RandomState` seed — cannot manufacture a false difference. Sequence
/// order, which is what emission order actually means, is preserved exactly.
#[cfg(test)]
pub(super) fn assert_walks_agree(
    iterative: &FileAnalysis,
    build_recursive: impl FnOnce() -> FileAnalysis,
) {
    assert_analyses_agree("walk", iterative, build_recursive)
}

/// The generic form: `label` names which pair of paths is being compared, so
/// a second equivalence net (pattern-dispatch combined vs per-spec) reuses
/// this comparator instead of growing a parallel one that can drift from it.
#[cfg(test)]
pub(super) fn assert_analyses_agree(
    label: &str,
    iterative: &FileAnalysis,
    build_recursive: impl FnOnce() -> FileAnalysis,
) {
    let recursive = build_recursive();
    let mut a = serde_json::to_value(iterative).expect("FileAnalysis serializes");
    let mut b = serde_json::to_value(&recursive).expect("FileAnalysis serializes");
    canonicalize_sets(&mut a);
    canonicalize_sets(&mut b);
    if a == b {
        return;
    }
    let mut path = String::new();
    first_difference(&a, &b, &mut path);
    // Also show the enclosing sequence element: a bare leaf ("scope: 3 vs 4")
    // rarely says which pass emitted the row, but the element around it
    // carries the witness `source` tag that does.
    let elem = path.rfind(']').map(|i| &path[..=i]).unwrap_or(&path[..]).to_string();
    panic!(
        "{label} divergence: the two paths disagree at `{}`\n\
         first: {}\n second: {}\n\
         enclosing `{}`\n  first: {}\n  second: {}",
        path,
        truncate(&at_path(&a, &path)),
        truncate(&at_path(&b, &path)),
        elem,
        truncate(&at_path(&a, &elem)),
        truncate(&at_path(&b, &elem)),
    );
}

/// `HashSet` fields reachable from `FileAnalysis`. They serialize as JSON
/// arrays in iteration order, which differs between two builds in one thread
/// (every map gets its own `RandomState` seed) — so comparing them as
/// sequences reports a difference that does not exist. Sorted before the
/// compare; every OTHER array keeps its order, because sequence order is
/// exactly what this check is for.
///
/// Listed by name rather than detected structurally: an array of scalars is
/// indistinguishable from a set once serialized, and order genuinely matters
/// for some of them (`package_parents` values carry MRO). A new `HashSet`
/// field on the model belongs here — the check fails loudly and points at it
/// if one is missed.
#[cfg(test)]
const SET_FIELDS: &[&str] = &[
    "column_keyed_verbs",
    "contract_symbols",
    "export_lookup",
    "framework_imports",
    "reassigned_scalars",
];

/// Sort the arrays named in `SET_FIELDS`, everywhere they appear.
#[cfg(test)]
fn canonicalize_sets(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if SET_FIELDS.contains(&k.as_str()) {
                    if let Value::Array(items) = child {
                        items.sort_by_key(|i| i.to_string());
                        continue;
                    }
                }
                canonicalize_sets(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize_sets),
        _ => {}
    }
}

/// Descend into the first mismatching child, recording the JSON path.
#[cfg(test)]
fn first_difference(a: &serde_json::Value, b: &serde_json::Value, path: &mut String) {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                match y.get(k) {
                    Some(vb) if vb == va => continue,
                    Some(vb) => {
                        path.push('.');
                        path.push_str(k);
                        return first_difference(va, vb, path);
                    }
                    None => {
                        path.push('.');
                        path.push_str(k);
                        return;
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, va) in x.iter().enumerate() {
                match y.get(i) {
                    Some(vb) if vb == va => continue,
                    Some(vb) => {
                        path.push_str(&format!("[{}]", i));
                        return first_difference(va, vb, path);
                    }
                    None => {
                        path.push_str(&format!("[{}] (missing on the right)", i));
                        return;
                    }
                }
            }
            if y.len() != x.len() {
                path.push_str(&format!(" (len {} vs {})", x.len(), y.len()));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
fn at_path(v: &serde_json::Value, path: &str) -> serde_json::Value {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        let (name, idxs) = match seg.find('[') {
            Some(i) => (&seg[..i], &seg[i..]),
            None => (seg, ""),
        };
        if !name.is_empty() {
            match cur.get(name) {
                Some(next) => cur = next,
                None => return cur.clone(),
            }
        }
        for part in idxs.split(']').filter(|s| !s.is_empty()) {
            if let Ok(i) = part.trim_start_matches('[').trim().parse::<usize>() {
                match cur.get(i) {
                    Some(next) => cur = next,
                    None => return cur.clone(),
                }
            }
        }
    }
    cur.clone()
}

#[cfg(test)]
fn truncate(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 400 {
        format!("{}… ({} bytes)", &s[..400], s.len())
    } else {
        s
    }
}
