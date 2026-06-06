# signature-help

Generated from `fixtures/signature-help.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl signature-help`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| sig-typetiny-failed-check-p0 | tricky | type-inference | `Type/Tiny.pm:962:23` | all: _failed_check($name, $value, %attrs); active param: 0 ($name) | gold | * _failed_check($name, $value, %attrs) active param: 0 ($name) |
| sig-typetiny-failed-check-p1 | tricky | type-inference | `Type/Tiny.pm:962:32` | all: _failed_check($name, $value, %attrs); active param: 1 ($value) | gold | * _failed_check($name, $value, %attrs) active param: 1 ($value) |
| sig-stuffer-addr-list-fatcomma-p0 | simple | classic | `Email/Stuffer.pm:312:30` | all: _assert_addr_list_ok($header, $allow_empty, $list); active param: 0 ($header) | gold | * _assert_addr_list_ok($header, $allow_empty, $list) active param: 0 ($header) |
| sig-stuffer-addr-list-fatcomma-p2 | tricky | classic | `Email/Stuffer.pm:312:41` | all: _assert_addr_list_ok($header, $allow_empty, $list); active param: 2 ($list) | gold | * _assert_addr_list_ok($header, $allow_empty, $list) active param: 2 ($list) |
| sig-uri-new-class-p0 | simple | oo-isa | `URI.pm:233:24` | all: new($uri; active param: 0 ($uri | gold | * new($uri: String, $scheme) active param: 0 ($uri: String) |
| sig-uri-new-class-p1 | tricky | oo-isa | `URI.pm:89:29` | all: new($uri; active param: 1 ($scheme) | gold | * new($uri: String, $scheme) active param: 1 ($scheme) |
| sig-stuffer-create-crossfile | tricky | fq | `Email/Stuffer.pm:215:37` | all: create(%args); active param: 0 (%args) | gold | * create(%args) active param: 0 (%args) |
| sig-gld-norm-imply-function | simple | classic | `Getopt/Long/Descriptive.pm:656:25` | all: _norm_imply($what); active param: 0 ($what) | gold | * _norm_imply($what) active param: 0 ($what) |
| sig-uri-check-path-function-noinvocant | tricky | classic | `URI/_generic.pm:58:13` | all: _check_path($path, $pre); active param: 0 ($path) | xfail | * _check_path($pre: String) active param: 0 ($pre: String) |
| sig-typetiny-new-dynamic-args | tricky | type-inference | `Type/Tiny.pm:1246:27` | all: new(); active param: 0 () | provisional | * new() active param: 0 () |
| sig-typetiny-check-shift-style | simple | type-inference | `Type/Tiny.pm:902:30` | all: check(); active param: 0 () | provisional | * check() active param: 0 () |
| sig-datetime-normalize-nanoseconds-indexarg | simple | classic | `x86_64-linux/DateTime.pm:226:8` | all: _normalize_nanoseconds(); active param: 0 () | provisional | * _normalize_nanoseconds() active param: 0 () |

## Dropped (non-lib, absent from installed tree)

_None._
