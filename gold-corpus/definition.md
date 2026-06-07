# definition

Generated from `fixtures/definition.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl definition`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| def-01-crossfile-import-call | tricky | exporter | `Type/Registry.pm:13:23` | all: Tiny.pm:430: | gold | Tiny.pm:430:1 |
| def-02-import-list-member | tricky | exporter | `Type/Registry.pm:74:12` | all: Tiny.pm:430: | gold | Tiny.pm:430:1 |
| def-03-samefile-method | simple | oo-isa | `Type/Tiny.pm:414:9` | all: Tiny.pm:525: | gold | Tiny.pm:525:5 |
| def-04-multihop-isa-method | tricky | oo-isa | `LWP/RobotUA.pm:64:23` | all: MemberMixin.pm:5: | gold | MemberMixin.pm:5:1 |
| def-05-use-parent-target | simple | oo-isa | `LWP/RobotUA.pm:2:14` | all: UserAgent.pm:1: | gold | UserAgent.pm:1:1 |
| def-06-onehop-isa-method | tricky | oo-isa | `LWP/UserAgent.pm:711:36` | all: MemberMixin.pm:5: | gold | MemberMixin.pm:5:1 |
| def-07-export-member-to-sub | simple | exporter | `LWP/Simple.pm:8:17` | all: Simple.pm:32: | gold | Simple.pm:32:5 |
| def-08-moose-has-accessor | tricky | moose | `Dist/Zilla.pm:182:29` | all: Zilla.pm:82: | gold | Zilla.pm:82:5 |
| def-09-with-role-target | simple | moose | `Dist/Zilla.pm:4:6` | all: ConfigDumper.pm:1: | gold | ConfigDumper.pm:1:1 |
| def-10-use-constant | simple | constants | `URI.pm:16:18` | all: URI.pm:9:14 | gold | URI.pm:9:14 |
| def-11-crossfile-imported-func-call | tricky | exporter | `URI/_query.pm:95:10` | all: Escape.pm:216: | gold | Escape.pm:216:1 |
| def-12-fq-variable | tricky | fq | `URI.pm:116:33` | all: URI.pm:27: | gold | URI.pm:27:5 |
| def-13-fq-sub-call | tricky | fq | `URI/otpauth.pm:47:39` | all: Escape.pm:216: | gold | Escape.pm:216:1 |
| def-14-classic-lexical-var | simple | classic | `LWP/MemberMixin.pm:9:11` | all: MemberMixin.pm:8:8 | gold | MemberMixin.pm:8:8 |
| def-15-classmethod-crossfile | tricky | oo-isa | `LWP/RobotUA.pm:46:31` | all: UserAgent.pm:23: | gold | UserAgent.pm:23:1 |
| def-16-codegen-type-function | tricky | codegen | `Type/Tiny.pm:414:56` | all: Standard.pm:215: / none: Standard.pm:1:1 | xfail | Standard.pm:1:1 |

## Dropped (non-lib, absent from installed tree)

_None._
