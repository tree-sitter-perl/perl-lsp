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

(binary_operator
  lhs: (identifier) @def.var.name @flow.target
  ["<-" "="]
  rhs: (_) @flow.source) @def.var @flow.assign

(parameter
  name: (identifier) @def.var.name) @def.var

; ---- scopes ----
(function_definition) @scope

; ---- refs ----
(call
  function: (identifier) @ref.call) @expr.call
(extract_operator
  lhs: (identifier) @ref.var
  rhs: (identifier) @ref.key)
(identifier) @expr.read.var

; ---- imports: library(pkg) / require(pkg) / source("path") ----
; R's imports are CALLS, so the capture says which kind of import the call
; is and the pack maps the ARGUMENT to a module: a sourced path is one
; verbatim, a library name resolves in the installed tree.
(call
  function: (identifier) @import.call.library
  arguments: (arguments
    (argument value: (identifier) @import.arg))
  (#any-of? @import.call.library "library" "require"))
(call
  function: (identifier) @import.call.source
  arguments: (arguments
    (argument value: (string (string_content) @import.arg)))
  (#eq? @import.call.source "source"))

; ---- literals ----
(string) @expr.lit.string
(float) @expr.lit.number
(integer) @expr.lit.number

; ---- keyed shapes: list(a = 1, b = 2) / data.frame(age = ..., ...) ----
; Named arguments of a shape constructor are the keys; which callees
; construct a $-accessible value is the pattern's own `#any-of?`, and the
; driver groups @shape.key by the enclosing @expr.shape span.
(call
  function: (identifier) @shape.ctor
  arguments: (arguments
    (argument
      name: (identifier) @shape.key))
  (#any-of? @shape.ctor "list" "data.frame" "tibble")) @expr.shape
