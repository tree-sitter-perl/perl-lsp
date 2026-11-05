// mojo-events: first-class Mojo::EventEmitter wire-ups / emit sites.
//
//   * ->on / ->once / ->subscribe   emit a Handler symbol for the event
//     name (stacked — each wire-up is its own registration) AND a
//     DispatchCall ref so `references` on the name reaches here.
//   * ->emit / ->unsubscribe / ->has_subscribers / ->catch  emit a
//     DispatchCall ref only. Cursor on the event name → goto-def jumps
//     to the nearest Handler, references shows every wire-up + emit.
//
// The Handler/DispatchCall types are part of the core file-analysis
// model (not HashKey-overloaded), so every downstream LSP feature
// (hover, sig help, ref, rename, workspace symbol) composes without
// any mojo-events-specific branching.
//
// The verb set and the arg shape live in the `patterns()` query
// (docs/prompt-plugin-queries.md); the core only computes the
// projections this pattern declares, and only for actual matches.
// `on_match` owns the semantics — no verb if-chain, no positional
// arg digging.

fn id() { "mojo-events" }

fn api() { 1 }

fn triggers() {
    [
        #{ ClassIsa: "Mojo::EventEmitter" },
        // Event wire-ups also happen inside Mojo plugins subscribing to an
        // emitter they hold (`$minion->on(worker => ...)` in a plugin's
        // `register`). The plugin package isn't itself an EventEmitter, so
        // this trigger opens the door there. `resolve_event` does NOT gate on
        // receiver class — it takes the receiver's class as the handler's
        // owner (falling back to the match-site package), the same
        // no-allowlist stance as minion: the `on`/`emit`/`subscribe` verb +
        // a string first arg is the signal. A stray `$x->on('e', sub)` in
        // such a file thus registers an owner-scoped handler; harmless
        // (owner won't match an unrelated `emit`).
        #{ ClassIsa: "Mojolicious::Plugin" },
    ]
}

fn patterns() {
    [
        #{
            name: "event_call",
            // A single-arg call carries the literal directly under
            // `arguments:`; multi-arg calls wrap a `list_expression` —
            // hence the alternation. The bare-arg alternatives
            // enumerate name-shaped kinds only, so `->catch(sub {…})`
            // stays unmatched (same decline as the old hook's
            // string_value check).
            query: `
(method_call_expression
  invocant: (_) @recv
  method: (_) @verb
  (#any-of? @verb "on" "once" "subscribe"
                  "emit" "unsubscribe" "has_subscribers" "catch")
  arguments: [
    (list_expression . (_) @event . (_)? @callback)
    (string_literal) @event
    (interpolated_string_literal) @event
    (scalar) @event
    (bareword) @event
  ]
) @call
`,
            projections: #{
                recv: ["ty"],
                event: ["str", "content_span"],
                callback: ["sub_params"],
            },
            expect: [
                #{ src: "$e->on('ready', sub { my ($s) = @_; });", matches: 1 },
                #{ src: "$e->emit('ready');", matches: 1 },
                #{ src: "$e->emit(ready => 1);", matches: 1 },
                #{ src: "$e->off('x');", matches: 0 },
            ],
        },
    ]
}

// Shared resolver: name + owner class + tight span around the name
// argument. Returns () when the call isn't eligible (dynamic name,
// no owner class, etc.).
fn resolve_event(m) {
    let ev = m.captures.event;
    if ev == () { return (); }

    let name = ev.str;
    if name == () { return (); }

    let owner_class = ();
    let rtype = m.captures.recv.ty;
    if rtype != () && rtype.contains("ClassName") {
        owner_class = rtype.ClassName;
    }
    if owner_class == () {
        // `package` is a Rhai reserved word — bracket access required.
        owner_class = m["package"];
    }
    if owner_class == () { return (); }

    // Prefer the string-content span (inside the quotes) so rename rewrites
    // just `connect`, not `'connect'` (a bareword replacement would strip the
    // quotes). A const-folded name (`$evt`) has no content span; its capture
    // span is the variable token — `refs_to` flags that site non-rewritable so
    // rename skips it (no literal to rewrite) while references still sees it.
    let nspan = ev.content_span;
    if nspan == () { nspan = ev.span; }

    #{
        name: name,
        owner: #{ Class: owner_class },
        name_span: nspan,
    }
}

fn on_match(pattern, m) {
    let out = [];
    let verb = m.captures.verb.text;

    let is_register = verb == "on" || verb == "once" || verb == "subscribe";

    let ev = resolve_event(m);
    if ev == () { return out; }

    // Every call-site is a reference — emit and unsubscribe sites as well
    // as wire-ups. References on the handler name return all of them.
    out += #{
        DispatchCall: #{
            name: ev.name,
            dispatcher: verb,
            owner: ev.owner,
            span: ev.name_span,
            var_text: m.captures.recv.text,
        }
    };

    // Registering verbs stamp a Handler def. Handler params come from
    // the handler sub's signature (the `sub_params` projection on the
    // callback capture — sig syntax or `my (...) = @_`).
    if is_register {
        let params = [];
        let cb = m.captures.callback;
        if cb != () {
            // Mojo::EventEmitter passes the emitter as the callback's
            // first positional. The param list is otherwise verbatim
            // from the handler sub; `as_invocant_params` flags just
            // position 0 so sig help drops it at display time.
            params = as_invocant_params(cb.sub_params);
        }

        out += #{
            Handler: #{
                name: ev.name,
                owner: ev.owner,
                dispatchers: ["emit"],
                params: params,
                // Extent covers the whole `on(name => sub {…})` so the outline
                // node encloses the handler body — editor sticky-scroll /
                // breadcrumb pin the event while the cursor is inside it.
                span: m.span,
                selection_span: ev.name_span,
            }
        };

        // Register the event into a namespace per emitter class. Bridges
        // to the emitter class so workspace-wide "events on EmitterX"
        // lookups walk `for_each_entity_bridged_to(EmitterX, ...)` and
        // find the Handler entities. One namespace per emitter class per
        // file; multiple on/once/subscribe sites accumulate entities.
        out += #{
            PluginNamespace: #{
                id: "mojo-events:" + ev.owner.Class,
                kind: "events",
                bridges: [ #{ Class: ev.owner.Class } ],
                entity_names: [ev.name],
                decl_span: ev.name_span,
            }
        };

        // Type the callback's first positional as the emitter class
        // so the user can ->-complete on it inside the handler body.
        // `ev.owner` is a Class map — project back to the string.
        if params.len() >= 1 && cb != () {
            out += #{
                VarType: #{
                    variable: params[0].name,
                    at: cb.span,
                    inferred_type: #{ ClassName: ev.owner.Class },
                }
            };
        }
    }

    out
}
