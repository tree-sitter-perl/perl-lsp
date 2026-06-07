# linked-editing

Generated from `fixtures/linked-editing.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl linked-editing`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| le-uri-scheme-local | simple | classic | `URI.pm:151:8` | all: URI.pm:152:8; URI.pm:153:10; URI.pm:153:21; URI.pm:162:31; URI.pm:164:21; URI.pm:196:17 | gold | URI.pm:152:8;153:10;153:21;158:5;158:18;162:31;163:38;164:21;168:26;173:17;195:28;196:17 |
| le-uri-implements-hash | simple | classic | `URI.pm:12:3` | all: URI.pm:13:4; URI.pm:162:19; URI.pm:164:9; URI.pm:168:14; URI.pm:196:5 / none: URI.pm:435 | gold | URI.pm:13:4;162:19;164:9;168:14;196:5 |
| le-uri-scheme-re-our | tricky | classic | `URI.pm:23:5` | all: URI.pm:24:5; URI.pm:66:21; URI.pm:126:29; URI.pm:153:35; URI.pm:273:18 | gold | URI.pm:24:5;66:21;73:35;101:44;126:29;153:35;225:35;232:52;233:29;240:33;242:26;245:33;269:17;273:18 |
| le-uri-implementor-sub-def | tricky | classic | `URI.pm:149:4` | none: URI.pm: | provisional |  |
| le-moo-target-closure | tricky | moo | `Moo.pm:94:11` | all: Moo.pm:95:12; Moo.pm:98:30; Moo.pm:104:41; Moo.pm:122:33; Moo.pm:131:29 | gold | Moo.pm:95:12;98:30;99:37;104:41;105:37;119:37;121:34;122:33;123:39;131:29 |
| le-moo-me-closure | simple | moo | `Moo.pm:94:7` | all: Moo.pm:95:7; Moo.pm:98:7; Moo.pm:99:7; Moo.pm:105:7; Moo.pm:119:9; Moo.pm:121:9; Moo.pm:123:9 / none: Moo.pm:104:7 | gold | Moo.pm:95:7;98:7;99:7;105:7;119:9;121:9;123:9 |
| le-moo-class-is-class | simple | moo | `Moo.pm:81:11` | all: Moo.pm:82:12; Moo.pm:83:18; Moo.pm:83:37 | gold | Moo.pm:82:12;83:18;83:37 |
| le-moo-me-single-occurrence | simple | moo | `Moo.pm:81:7` | none: Moo.pm: | gold |  |
| le-moo-makers-our-fq-and-strings | tricky | moo | `Moo.pm:34:4` | all: Moo.pm:35:5; Moo.pm:57:16; Moo.pm:83:10; Moo.pm:83:29; Moo.pm:268:16 / none: Moo.pm:211; Moo.pm:212 | gold | Moo.pm:35:5;57:16;83:10;83:29;154:24;177:17;178:3;185:11;185:35;203:17;204:3;235:27;268:16 |
| le-dt-p-hash-slices | tricky | type-inference | `x86_64-linux/DateTime.pm:190:7` | all: DateTime.pm:211:13; DateTime.pm:212:11; DateTime.pm:213:44 | xfail | 27 of 30 ranges; the three $p{time_zone} refs at 211/212/213 dropped |
| le-dt-class-new | simple | classic | `x86_64-linux/DateTime.pm:189:7` | all: DateTime.pm:190:8; DateTime.pm:194:16; DateTime.pm:204:22; DateTime.pm:216:30; DateTime.pm:219:11 | gold | DateTime.pm:190:8;194:16;204:22;206:26;216:30;219:11 |
| le-dt-infinity-constant | tricky | constants | `x86_64-linux/DateTime.pm:81:4` | none: DateTime.pm: | provisional |  |

## Dropped (non-lib, absent from installed tree)

_None._
