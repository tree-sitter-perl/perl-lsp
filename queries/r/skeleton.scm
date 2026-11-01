; R language pack. Same vocabulary, same driver, same engine.
;
; R's bones fit the engine unusually well: `list(a=1)` /
; `data.frame(age=...)` ARE keyed shapes (HashWithKeys), `df$age` IS a
; key access, S3 methods are name conventions (`print.myclass`), and
; `source("util.R")` hands cross-file resolution a literal path.

; ---- defs ----
; `f <- function(...)` — a sub. The general var pattern below also
; matches; the driver's def-dedup prefers the more specific kind.
(binary_operator
  lhs: (identifier) @def.sub.name
  ["<-" "="]
  rhs: (function_definition)) @def.sub

; `name = function(...)` as a list()/R6Class() member — the R OOP idiom.
; These are `argument` nodes, not `binary_operator`, so the pattern above
; misses them (the scout found 195 such method defs lost across tidyverse).
(argument
  name: (identifier) @def.sub.name
  value: (function_definition)) @def.sub @scope

; A variable belongs to the function it is assigned in: the first write
; declares it, and every later write and every read in the body is that
; one variable.
(binary_operator
  lhs: (identifier) @def.assigned.name @flow.target
  ["<-" "="]
  rhs: (_) @flow.source) @def.assigned @flow.assign

(parameter
  name: (identifier) @def.var.name) @def.var

; ---- scopes ----
(function_definition) @scope.sub

; ---- refs ----
(call
  function: (identifier) @ref.call) @expr.call
(extract_operator
  lhs: (identifier) @ref.var
  rhs: (identifier) @ref.key)
(identifier) @expr.read.var

; ---- imports: library(pkg) / require(pkg) / source("path") ----
; R's imports are CALLS. The callees that import are the predicate's; the
; argument names the module as written, a bare package name or a quoted
; path alike.
(call
  function: (identifier) @_import
  arguments: (arguments
    (argument value: (identifier) @import.arg))
  (#any-of? @_import "library" "require" "source"))
(call
  function: (identifier) @_import
  arguments: (arguments
    (argument value: (string (string_content) @import.arg)))
  (#any-of? @_import "library" "require" "source"))

; ---- literals ----
; Parentheses hold the value they wrap.
((parenthesized_expression (_) @expr.wrapper.inner) @expr.wrapper
 (#not-is-kind? @expr.wrapper.inner comment))
(string) @expr.lit.string
(float) @expr.lit.number
(integer) @expr.lit.number

; ---- keyed shapes: list(a = 1, b = 2) / data.frame(age = ..., ...) ----
; Named arguments of a shape constructor are the keys. Which callees
; construct a $-accessible value is the predicate's; the driver groups
; @shape.key by the enclosing @expr.shape span.
(call
  function: (identifier) @_ctor
  arguments: (arguments
    (argument
      name: (identifier) @shape.key))
  (#any-of? @_ctor "list" "data.frame" "tibble")) @expr.shape
