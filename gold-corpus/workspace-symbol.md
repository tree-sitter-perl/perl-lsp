# workspace-symbol

Generated from `fixtures/workspace-symbol.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl workspace-symbol`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| ws-moo-pkg | simple | oo-isa | query: `Moo` | all: {"col":"8","file":"Moo.pm","kind":"Package","line":"0","name":"Moo"} | gold | Moo Package Moo.pm:0:8 among many matches |
| ws-uri-pkg | simple | oo-isa | query: `URI` | all: {"col":"8","file":"URI.pm","kind":"Package","line":"0","name":"URI"} | gold | URI Package URI.pm:0:8 |
| ws-uri-newabs | simple | classic | query: `new_abs` | all: {"col":"4","file":"URI.pm","kind":"Sub","line":"86","name":"new_abs"} | gold | 3 matches: URI.pm:86:4, WithBase.pm:25:4, file.pm:46:4 |
| ws-cat-pkg | simple | oo-isa | query: `Catalyst::Controller` | all: {"col":"8","file":"Controller.pm","kind":"Package","line":"0","name":"Catalyst::Controller"} | gold | Catalyst::Controller Package Controller.pm:0:8 |
| ws-cat-forward | tricky | classic | query: `forward` | all: {"col":"4","file":"Catalyst.pm","kind":"Sub","line":"488","name":"forward"} | gold | forward Sub Catalyst.pm:488:4 among many |
| ws-cat-toapp | tricky | codegen | query: `to_app` | all: {"col":"0","file":"Catalyst.pm","kind":"Sub","line":"3557","name":"to_app"} | gold | to_app Sub Catalyst.pm:3557:0 (typeglob alias) |
| ws-dt-mon | tricky | codegen | `x86_64-linux/DateTime.pm` q=`mon` | all: {"col":"0","file":"DateTime.pm","kind":"Sub","line":"797","name":"mon"} | gold | mon Sub DateTime.pm:797:0 (glob alias *mon = \&month;) |
| ws-dt-fromepoch | tricky | classic | `x86_64-linux/DateTime.pm` q=`from_epoch` | all: {"col":"8","file":"DateTime.pm","kind":"Sub","line":"485","name":"from_epoch"} | gold | from_epoch Sub DateTime.pm:485:8 (4-space indent reflected in col) |
| ws-moo-accessor | tricky | moo | query: `namespace` | all: {"col":"4","file":"Action.pm","kind":"Method","line":"29","name":"namespace"}; {"col":"4","file":"Action.pm","kind":"HashKeyDef","line":"29","name":"namespace"} | gold | Method namespace @ Action.pm:29:4 (x2) + HashKeyDef @ 29:4 |
| ws-moose-accessor | tricky | moose | query: `part` | all: {"col":"4","file":"ActionContainer.pm","kind":"Method","line":"20","name":"part"}; {"col":"4","file":"ActionContainer.pm","kind":"HashKeyDef","line":"20","name":"part"} | gold | Method part @ ActionContainer.pm:20:4 (x2) + HashKeyDef @ same loc |
| ws-subexp-build | simple | exporter | query: `build_exporter` | all: {"col":"4","file":"Exporter.pm","kind":"Sub","line":"703","name":"build_exporter"} | gold | build_exporter Sub Exporter.pm:703:4 (1 match) |
| ws-exptiny-import | tricky | exporter | query: `import` | all: {"col":"4","file":"Tiny.pm","kind":"Sub","line":"53","name":"import"} | gold | import Sub Tiny.pm:53:4 among many |
| ws-tt-coerce | tricky | type-inference | query: `coerce` | all: {"col":"4","file":"Tiny.pm","kind":"Sub","line":"1088","name":"coerce"} | gold | coerce Sub Tiny.pm:1088:4 among many |
| ws-cat-fuzzy | tricky | oo-isa | query: `Controll` | all: {"col":"8","file":"Controller.pm","kind":"Package","line":"0","name":"Catalyst::Controller"} | gold | Controll -> Catalyst::Controller Package Controller.pm:0:8 |

## Dropped (non-lib, absent from installed tree)

- ws-jsonpp-const — JSON::PP is not installed in the cpm substrate (snapshot resolves JSON via JSON::MaybeXS -> Cpanel::JSON::XS; no JSON/PP.pm under local/lib/perl5). `--emit workspace-symbol P_ASCII` returns []. The use-constant codegen target (P_ASCII Sub) has no installed file to anchor to.
