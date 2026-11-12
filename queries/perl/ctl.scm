; Perl control structure, in the `@ctl.*` vocabulary every language's
; document speaks (`docs/adr/cfg-tier.md`, `query_extract::vocab::CtlCap`).
; Read by `build::builder::control`, never stored.
;
;   @ctl.branch / @ctl.loop   the construct; `@ctl.loop.once` for a body
;                             that runs before the first test
;   @ctl.cond                 the condition its arms are taken on
;   @ctl.then / @ctl.else     what runs where the condition holds / fails
;   @ctl.loop.body            what a loop runs while its test holds
;   @ctl.loop.step            what runs after each pass, before the next test
;   @ctl.leave.<kind>         a statement that leaves its block
;
; Every arm pattern carries its construct, so arms group by construct node.
; A condition, a loop list and a `for` initialiser run whenever their
; construct does, so they are no arm.

; ---- if / unless, elsif, else ----
; `unless` runs its block where the condition fails, and its `elsif` /
; `else` where it holds.
(conditional_statement "if" condition: (_) @ctl.cond block: (_) @ctl.then) @ctl.branch
(conditional_statement "if" condition: (_) @ctl.cond [(elsif) @ctl.else (else block: (_) @ctl.else)]) @ctl.branch
(conditional_statement "unless" condition: (_) @ctl.cond block: (_) @ctl.else) @ctl.branch
(conditional_statement "unless" condition: (_) @ctl.cond [(elsif) @ctl.then (else block: (_) @ctl.then)]) @ctl.branch
(elsif condition: (_) @ctl.cond block: (_) @ctl.then) @ctl.branch
(elsif condition: (_) @ctl.cond [(elsif) @ctl.else (else block: (_) @ctl.else)]) @ctl.branch

; ---- statement modifiers ----
(postfix_conditional_expression . (_) @ctl.then "if" condition: (_) @ctl.cond) @ctl.branch
(postfix_conditional_expression . (_) @ctl.else "unless" condition: (_) @ctl.cond) @ctl.branch

; ---- ?: and the short-circuits ----
; `//`'s right runs where its left is undefined: the else of a definedness
; test. `xor` evaluates both sides, so it decides nothing.
(conditional_expression condition: (_) @ctl.cond consequent: (_) @ctl.then) @ctl.branch
(conditional_expression condition: (_) @ctl.cond alternative: (_) @ctl.else) @ctl.branch
(lowprec_logical_expression left: (_) @ctl.cond "and" right: (_) @ctl.then) @ctl.branch
(lowprec_logical_expression left: (_) @ctl.cond "or" right: (_) @ctl.else) @ctl.branch
(binary_expression left: (_) @ctl.cond "&&" right: (_) @ctl.then) @ctl.branch
(binary_expression left: (_) @ctl.cond ["||" "//"] right: (_) @ctl.else) @ctl.branch

; ---- loops ----
; TODO(cfg-loop-polarity): an `until` body runs where its condition fails;
; nothing reads a loop condition's polarity yet.
(loop_statement condition: (_) @ctl.cond block: (_) @ctl.loop.body) @ctl.loop
(loop_statement continue: (_) @ctl.loop.step) @ctl.loop
(cstyle_for_statement block: (_) @ctl.loop.body) @ctl.loop
(cstyle_for_statement condition: (_) @ctl.cond) @ctl.loop
(cstyle_for_statement iterator: (_) @ctl.loop.step) @ctl.loop
(for_statement block: (_) @ctl.loop.body) @ctl.loop
(postfix_loop_expression (do_expression) @ctl.loop.body condition: (_) @ctl.cond) @ctl.loop.once
(postfix_loop_expression . (_) @ctl.loop.body condition: (_) @ctl.cond) @ctl.loop
(postfix_for_expression . (_) @ctl.loop.body list: (_)) @ctl.loop

; ---- leaving ----
; `die` and `exit` are calls; how a call ends is the effect lane's
; (`model/builtins.rs`). `redo` restarts the body without the test, a back
; edge like `next`'s.
(return_expression) @ctl.leave.return
(loopex_expression "last") @ctl.leave.break
(loopex_expression ["next" "redo"]) @ctl.leave.continue
(goto_expression) @ctl.leave.jump
