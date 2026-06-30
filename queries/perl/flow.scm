; Perl value-flow capture pack — the assignment SHAPES, declarative.
;
; Run inside build() by `mint_flow_edges_via_query` (NOT the spike extractor),
; so FlowEdges carry the BUILDER's scope, not a skeleton scope. The capture
; vocabulary is the same the cpp pack speaks:
;
;   @flow.target   a binding that receives a value (the LHS var)
;   @flow.source   the value expression it receives (the RHS)
;
; The driver mints one FlowEdge per (target, source); positional/extraction
; logic lives in the minter, shared with cpp. STRUCTURAL forms only — the
; `right:` field misses a parenthesized RHS (a tree-sitter-perl field quirk),
; so the RHS is matched by node, not field, where it matters.

; bare-RHS declaration: `my $x = EXPR`, `my @a = EXPR`, `my %h = EXPR`
; (a parenthesized list RHS is handled by the list pattern, added next slice).
(assignment_expression
  left: (variable_declaration [(scalar) (array) (hash)] @flow.target)
  right: (_) @flow.source)

; bare reassignment: `$x = EXPR`
(assignment_expression
  left: (scalar) @flow.target
  right: (_) @flow.source)
