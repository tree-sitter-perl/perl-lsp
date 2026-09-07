; Perl value-flow capture pack — the assignment SHAPES, declarative.
;
; Run inside build() by `mint_flow_edges_via_query` (NOT the spike extractor),
; so FlowEdges carry the BUILDER's scope. Capture vocabulary:
;
;   @flow.lhs      a `my`/`local`/`our` declaration LHS (single OR list), or a
;                  bare `($x, $y)` reassignment group — the minter iterates
;                  its slots (positional for a list)
;   @flow.target   a bare scalar LHS (reassignment) — a single Whole target
;   @flow.source   the value expression the LHS receives
;
; The minter reuses `lhs_list_targets`/`list_element_nodes` for the positional
; pairing — the shape is declared here, the pairing logic is shared. A
; parenthesized RHS (`= (1, 2)`) arrives as the `right:` field's
; `parenthesized_expression`; `list_element_nodes` unwraps it.

; `my $x = EXPR` / `my @a = EXPR` / `my ($a, $b) = EXPR`
(assignment_expression
  left: (variable_declaration) @flow.lhs
  right: (_) @flow.source)

; bare reassignment: `$x = EXPR`
(assignment_expression
  left: (scalar) @flow.target
  right: (_) @flow.source)

; bare LIST reassignment `($x, $y) = EXPR` (no `my` — the LHS is a
; parenthesized group, not a variable_declaration). Each slot rebinds: the
; cutoff + positional typing.
(assignment_expression
  left: (parenthesized_expression) @flow.lhs
  right: (_) @flow.source)

; --- binding shapes (no inflowing value) — the rebind coverage the narrowing
; --- cutoff needs, plus a real type where the bind clears the var. ---

; bare `my $x;` / `my ($x,$y);` — a declaration that is NOT an assignment LHS
; (a direct child of the statement; the `= …` form nests under assignment_expression
; and so won't match here). Clears to undef.
(expression_statement (variable_declaration) @flow.bare)

; bare `local $x;` — clears the scalar to undef for the dynamic scope.
(localization_expression (scalar) @flow.bare)

; `foreach my $x (LIST)` — the loop var rebinds per element (element type TBD).
(for_statement
  variable: (scalar) @flow.loopvar
  list: (_) @flow.source)
