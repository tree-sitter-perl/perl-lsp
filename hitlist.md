# cpp-lsp is completely useless right now

## perl5
### toke.c

- line 148 of toke.c gives NO references to the macro, even tho it has many uses
  - macros look like they aren't treated like actual symbols, while semantically they are
    of sorts, at least enum-ey ones like that

- gd does nothing on #include lines
  - even worse - if you gd on the .h in the quotes, it takes you to line 12173, where
    ther's a random var named `h`
  - includes should be treated like perl imports in their UX

### op.c

- line 180 - no references found, even tho it has callers thru its blessed macro
- op_type from *op_p shows NO type info, just that it's a field
  - gd on it takes you to the def of OP, which is just a container for another macro;
    you'd probably want to model that cleaner and show where it's composed and also
    defined
  - `op_p->` by itself on a line gives no smart completion (falls back to global); it's possible that it's a syntax
    error the sentinel doesn't fix?
    - `*op_p->` DOES give completion
    - so does `op_p. == 5` w/ your cursor on the dot
  - the "you need to peel" diagnostic is not firing
- on line 185, no element of the OP enum is offered as a completion
- on line 2817, gd on fix_optchain takes you to the intermediate macro, and you have to
  then navigate to its inner def
  - makes it seem like function like wrapper macros should be more transparent, similar to
    the references issue from above

## fmt

### src/format.cc

- the outline is - COMPLETELY USELESS, it shows a handful of random variables which are
  arguments to templates (lines 19 and 21)

### include/fmt/format.h

- line 1161 - gr returns no referenses to the struct, even tho it seems to be used a few
  times w/in several lines
  - this is asymmetric - gd on line 1167 does jump back

- macros that are clearly markers (define w/ no expansion) show up in the outline
