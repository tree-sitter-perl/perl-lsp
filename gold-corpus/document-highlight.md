# document-highlight

Generated from `fixtures/document-highlight.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl document-highlight`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| dh-uri-impclass | simple | classic-lexical-scalar | `URI.pm:64:8` | all: URI.pm:65:8	READ; URI.pm:70:7	WRITE; URI.pm:77:5	WRITE; URI.pm:80:6	WRITE; URI.pm:83:12	READ | gold | URI.pm:65:8 READ; 70:7 WRITE; 77:5 WRITE; 80:6 WRITE; 83:12 READ |
| dh-uri-str-init | tricky | classic-lexical-scalar-interp | `URI.pm:97:9` | all: URI.pm:98:8	READ; URI.pm:100:5	WRITE; URI.pm:100:33	READ; URI.pm:101:5	WRITE; URI.pm:101:21	READ; URI.pm:101:34	READ; URI.pm:103:23	READ | gold | 98:8 READ; 100:5 WRITE; 100:33 READ; 101:5 WRITE; 101:21 READ; 101:34 READ; 103:23 READ |
| dh-uri-scheme-shadow | tricky | classic-lexical-shadow | `URI.pm:55:21` | all: URI.pm:56:22	READ; URI.pm:67:2	WRITE; URI.pm:70:23	READ; URI.pm:71:6	WRITE; URI.pm:71:16	READ; URI.pm:73:9	READ; URI.pm:73:20	READ; URI.pm:74:6	WRITE; URI.pm:77:31	READ; URI.pm:83:35	READ / none: URI.pm:8:; URI.pm:9: | gold | 10 occurrences 56:22..83:35; package $scheme_re not merged |
| dh-uri-uri-subst | tricky | classic-lexical-subst | `URI.pm:55:17` | all: URI.pm:56:16	READ; URI.pm:58:5	WRITE; URI.pm:58:21	READ; URI.pm:58:30	READ; URI.pm:60:5	READ; URI.pm:61:5	READ; URI.pm:62:5	READ; URI.pm:63:5	READ; URI.pm:66:9	READ; URI.pm:83:29	READ | provisional | $uri occurrence set 10/10; s/// at 60-63 classified READ |
| dh-uri-implements-hashkey | tricky | classic-package-hash | `URI.pm:12:3` | all: URI.pm:13:4	READ; URI.pm:162:19	READ; URI.pm:164:9	WRITE; URI.pm:168:14	READ; URI.pm:196:5	WRITE / none: URI.pm:435: | gold | 13:4 READ; 162:19 READ; 164:9 WRITE; 168:14 READ; 196:5 WRITE |
| dh-datetime-normalize_seconds-sub | simple | classic-sub-name | `x86_64-linux/DateTime.pm:394:8` | all: DateTime.pm:343:24	READ; DateTime.pm:363:24	READ; DateTime.pm:395:5	READ; DateTime.pm:1925:16	READ | gold | 343:24 READ; 363:24 READ; 395:5 READ; 1925:16 READ |
| dh-moo-target-import-shadow | tricky | moo-lexical-shadow | `Moo.pm:37:5` | all: Moo.pm:38:6	READ; Moo.pm:40:52	READ; Moo.pm:49:25	READ; Moo.pm:50:22	READ | gold | 38:6 READ; 40:52 READ; 49:25 READ; 50:22 READ (4 sites only) |
| dh-plack-env-scoped | simple | classic-lexical-scalar-scope | `Plack/Request.pm:206:8` | all: Request.pm:207:8	READ; Request.pm:209:16	READ; Request.pm:211:10	READ; Request.pm:211:33	READ; Request.pm:211:69	READ; Request.pm:212:10	READ | gold | 207:8 READ; 209:16 READ; 211:10 READ; 211:33 READ; 211:69 READ; 212:10 READ |

## Dropped (non-lib, absent from installed tree)

- dh-jsonpp-self-new-shadow: JSON::PP is not in the installed cpm tree (no JSON/PP.pm under local/lib/perl5; only JSON/MaybeXS.pm present) — file absent, cannot port.
- dh-jsonpp-object_to_json-sub: JSON::PP is not in the installed cpm tree (no JSON/PP.pm under local/lib/perl5) — file absent, cannot port.
