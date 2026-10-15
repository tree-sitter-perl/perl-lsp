#!/usr/bin/env perl
# Gold-corpus harness — structured, exact-assertion regression runner.
#
# Substrate: a hermetic, version-pinned module tree installed by cpm/carton from
# gold-corpus/cpanfile + cpanfile.snapshot into gold-corpus/local. The harness
# points the LSP at local/lib/perl5 (workspace root + PERL5LIB), so every query
# resolves against the SAME pinned versions in dev and CI. Rebuild the substrate:
#     cd gold-corpus && carton install --deployment      # exact, from snapshot
#   (or, faster:  cpm install -L local --resolver snapshot)
#
# Source of truth: gold-corpus/fixtures/*.json. Each row re-runs a CLI query and
# asserts its NORMALIZED output:
#     expect.all  — every string MUST appear
#     expect.none — no string may appear
#     expect.exact_labels — (line-oriented caps, e.g. completion) the output's
#                   non-empty lines, as a SET, must equal exactly this list —
#                   extra entries FAIL. The noise guard containment can't give.
#     expect.max_items — line-count ceiling; more entries FAIL.
#   status:
#     gold        — assertion MUST hold; else FAIL (regression)
#     xfail       — known gap: assertion must currently NOT hold; if it starts
#                   holding → XPASS (gap fixed → promote to gold). Soft failure.
#     provisional — run + report, never fails the suite
#   warm:
#     "xfail"     — this row is KNOWN to pass cold and fail warm. Reported as
#                   warm-xfail; drop the key when the bug is fixed (the run then
#                   reports warm-XPASS until you do).
#   a process abort (exit 134 / signal) is always a hard FAIL (the scanner-overflow class).
#
# The suite runs TWICE against a private, throwaway cache dir: once cold (nothing
# written yet) and once warm (against what the cold pass wrote). A row that
# passes cold and fails warm is warm-FAIL — a fact that survives the first
# analysis but not rehydration, which is invisible to any cold-only run and is
# the state every user is in from their second session onward. `--no-warm` skips
# the second pass for fast iteration; the summary then says so rather than
# printing zeros it did not earn.
#
# Output is normalized before matching: absolute paths reduced to basenames; JSON
# outputs (references/workspace-symbol/outline/rename/diagnostics) decoded and
# re-encoded canonically (sorted keys, compact) so one substring ties
# file+line+kind. The SAME normalize() backs `--emit`, which prints the canonical
# text for one ad-hoc query — author fixtures against `--emit` and they match by
# construction.
#
# Usage:
#   gold-corpus/run.pl [capability ...]                       # run the suite (cold + warm)
#   gold-corpus/run.pl --no-warm [capability ...]             # cold pass only
#   gold-corpus/run.pl --list
#   gold-corpus/run.pl --emit <cap> <file> <line> <col> [newname]
#   gold-corpus/run.pl --emit workspace-symbol <query>
#   gold-corpus/run.pl --emit diagnostics
#   gold-corpus/run.pl --emit [--root <dir>] [--lang cpp] <cap> ...   # a non-Perl row
#   BIN=path CORPUS=<lib> gold-corpus/run.pl definition
# CORPUS defaults to gold-corpus/local/lib/perl5; <file> is a path under it.

use strict;
use warnings;
use FindBin qw($RealBin);
use File::Spec;
use Config;
use JSON::PP;
use POSIX qw(WIFSIGNALED WTERMSIG WIFEXITED WEXITSTATUS);
use File::Temp qw(tempfile);
use Time::HiRes qw(time);
binmode STDOUT, ':utf8';    # responses may carry non-ASCII once decoded

my $bin    = $ENV{BIN}    || File::Spec->rel2abs("$RealBin/../target/release/perl-lsp");
my $corpus = $ENV{CORPUS} || "$RealBin/local/lib/perl5";
my $fxdir  = "$RealBin/fixtures";

die "binary not found: $bin (cargo build --release)\n" unless -x $bin;

# Languages this binary can serve — `perl-lsp --languages` prints e.g.
# "perl-lsp 0.5.3 — languages: perl, cpp". A fixture row tagged with a
# `lang` the binary does NOT carry is SKIPPED, so a default perl-only
# build's gold run stays clean while a `--features cpp` build exercises
# the C++ fixtures from the SAME harness, each language in its own
# `--lang` batch. (Untagged rows are Perl — always run.)
my %served = do {
    my $out = qx{$bin --languages 2>/dev/null};
    # Entries may carry a maturity parenthetical ("cpp (beta)"); the id is
    # what a row's `lang` tag matches, so strip it. DIE rather than default
    # on an unparseable line: falling back to perl-only would silently SKIP
    # every cpp row and still print a green summary — a wrong answer that
    # looks like a pass, which is the one failure this harness must never have.
    my ($list) = $out =~ /languages:\s*(.+?)\s*$/m;
    die "cannot read served languages from `$bin --languages` (got: "
      . ($out =~ s/\s+\z//r) . ")\n" unless defined $list && length $list;
    map { s/\s*\(.*\)\z//r => 1 } split /\s*,\s*/, $list;
};
unless (-d $corpus) {
    die "substrate not found at $corpus.\n"
      . "Build it: cd gold-corpus && carton install   (or cpm install -L local --resolver snapshot)\n";
}
# hermetic @INC: the pinned tree + its arch dir, ahead of anything inherited.
my ($arch) = grep { -d } glob("$corpus/*/auto") ? map { s{/auto$}{}r } glob("$corpus/*/auto") : ();
$ENV{PERL5LIB} = join(':', grep { defined && length } $corpus, $arch, $ENV{PERL5LIB});

# The substrate's arch-specific subdirectory. It is NOT the same string on
# every host: a Debian/Ubuntu threaded perl reports
# `x86_64-linux-gnu-thread-multi` where CI's perl reports `x86_64-linux`.
# Fixtures therefore spell it `{arch}` instead of one host's answer, and this
# substitutes whatever is actually on disk (falling back to this perl's own
# archname when the substrate has no arch dir at all). Hardcoding it cost two
# sessions a full run each: the 22 rows under it simply did not resolve, and
# the harness reported them as `skip` under an otherwise green summary.
my $archname = defined $arch
    ? (File::Spec->splitdir($arch))[-1]
    : $Config{archname};

# Strict residency: a server that would serve absence-as-answer (an evicted
# copy whose blob can't rehydrate, an unaccounted whole-copy pin) PANICS —
# the row scores as a crash (hard fail) instead of quietly-wrong results.
# The net for the intermittent "inputs vanished" cold flake; override with
# PERL_LSP_STRICT_RESIDENCY=0 to reproduce the degraded behavior on purpose.
$ENV{PERL_LSP_STRICT_RESIDENCY} //= '1';

my %CAP = (
    'definition'         => { flag => '--definition',        root => 1, file => 1, lc => 1 },
    'type-definition'    => { flag => '--type-definition',   root => 1, file => 1, lc => 1 },
    'references'         => { flag => '--references',         root => 1, file => 1, lc => 1 },
    'implementations'    => { flag => '--implementations',    root => 1, file => 1, lc => 1 },
    'type-hierarchy'     => { flag => '--type-hierarchy',     root => 1, file => 1, lc => 1 },
    'call-hierarchy'     => { flag => '--call-hierarchy',     root => 1, file => 1, lc => 1 },
    'hover'              => { flag => '--hover',              root => 1, file => 1, lc => 1 },
    'type-at'            => { flag => '--type-at',            root => 0, file => 1, lc => 1 },
    'completion'         => { flag => '--completion',         root => 1, file => 1, lc => 1 },
    'signature-help'     => { flag => '--signature-help',     root => 1, file => 1, lc => 1 },
    'document-highlight' => { flag => '--document-highlight', root => 1, file => 1, lc => 1 },
    'linked-editing'     => { flag => '--linked-editing',     root => 1, file => 1, lc => 1 },
    'semantic-tokens'    => { flag => '--semantic-tokens',    root => 1, file => 1, lc => 0 },
    'document-link'      => { flag => '--document-link',      root => 1, file => 1, lc => 0 },
    'outline'            => { flag => '--outline',            root => 0, file => 1, lc => 0 },
    'workspace-symbol'   => { flag => '--workspace-symbol',   root => 1, qarg => 1 },
    'rename'             => { flag => '--rename',             root => 1, file => 1, lc => 1, rename => 1 },
    'diagnostics'        => { check => 1 },
);
my %JSON_CAP = map { $_ => 1 } qw(references implementations type-hierarchy call-hierarchy workspace-symbol outline rename diagnostics);

sub run_cmd {                       # argv -> (stdout, crashed?)  [stderr dropped]
    my ($argv) = @_;
    my $pid = open(my $fh, '-|') // die "fork: $!";
    if ($pid == 0) { open(STDERR, '>', '/dev/null'); exec @$argv or exit 127; }
    local $/; my $out = <$fh> // ''; close($fh);
    my $st = $?;
    return ($out, (WIFSIGNALED($st) || ($st >> 8) == 134) ? 1 : 0);
}
sub _bn { my $s = shift; $s =~ s{^/.*/([^/]+)$}{$1}; return $s; }       # abs path -> basename
# Reduce absolute source paths to basenames so text-cap assertions are
# portable. Covers Perl + the pack-language extensions (multi-language gold).
sub _text_bn { my $s = shift; $s =~ s{/\S*?/([^/\s":]+\.(?:pm|pl|t|pod|cpp|cc|cxx|c|hpp|hh|h|py|R|r|cmake))}{$1}g; return $s; }
sub normalize {
    my ($cap, $raw) = @_;
    if ($JSON_CAP{$cap}) {
        # $raw is already a decoded character string (the batch wrapper was
        # byte-decoded once); decode_json would croak on any non-ASCII char
        # and silently drop us to the text fallback — where the no-space
        # canonical assertions vacuously pass. Character-mode decode only.
        my $data = eval { JSON::PP->new->decode($raw) };
        return _text_bn($raw) unless defined $data;
        my $walk; $walk = sub {
            my $r = ref $_[0];
            if    ($r eq 'HASH')  { my %h = map { $_ => $walk->($_[0]{$_}) } keys %{$_[0]}; return \%h; }
            elsif ($r eq 'ARRAY') { return [ map { $walk->($_) } @{$_[0]} ]; }
            else { return defined $_[0] ? _bn("$_[0]") : $_[0]; }
        };
        return JSON::PP->new->canonical->encode($walk->($data));
    }
    return _text_bn($raw);
}
# Build the batch request for a fixture row. capability -> the `q` the binary's
# --batch expects; file made absolute under the substrate; ws-symbol carries a
# query string, rename a newname, diagnostics neither.
# A row's workspace root: most rows use the snapshot substrate, but a row may
# carry its own `root` (repo-relative) — e.g. the committed re-export fixture
# tree, which needs its own self-contained workspace.
sub root_for {
    my ($row) = @_;
    return $corpus unless $row->{root};
    return File::Spec->rel2abs("$RealBin/../$row->{root}");
}
# @INC for a given root: the substrate keeps its pinned PERL5LIB (corpus+arch);
# a fixture root resolves against its own lib/. A row may name `incRoots`
# (repo-relative dirs) to put PROVIDERS OUTSIDE ITS WORKSPACE on @INC — the
# only way to exercise the @INC tier honestly, since a twin inside the
# workspace is answered by the workspace relation instead.
sub p5lib_for {
    my ($root, $extra) = @_;
    my $base = $root eq $corpus ? $ENV{PERL5LIB} : "$root/lib:$root";
    return length($extra // '') ? "$extra:$base" : $base;
}
# A row's extra @INC roots, absolute and ':'-joined ('' when it declares none).
sub inc_roots_for {
    my ($row) = @_;
    join ':', map { File::Spec->rel2abs("$RealBin/../$_") } @{ $row->{incRoots} // [] };
}
sub batch_req {
    my ($cap, $spec, $row, $key, $root) = @_;
    $root //= root_for($row);
    my %r = (id => $key, q => $cap);
    if ($spec->{check}) { return \%r; }
    if ($spec->{qarg})  { $r{query} = $row->{query} // ''; return \%r; }
    (my $rel = $row->{file}) =~ s/\{arch\}/$archname/g;
    $r{file} = "$root/$rel";
    $r{line} = $row->{line} // 0;
    $r{col}  = $row->{col}  // 0;
    $r{newname} = $row->{newname} // 'RENAMED' if $spec->{rename};
    return \%r;
}
# Run a batch of requests through ONE --batch process (one startup) against a
# given root, return id -> response. Rows are grouped by root AND language by
# the caller: a process serves one language (`--lang`), so a C++ row in a Perl
# batch is refused, never answered in Perl.
sub run_batch {
    my ($reqs, $root, $p5lib, $rename_scope, $lang) = @_;
    $lang //= 'perl';
    $root  //= $corpus;
    $p5lib //= p5lib_for($root);
    my $jsonl = join("\n", map { encode_json($_) } @$reqs) . "\n";
    # Stage the requests in a temp file and redirect the child's STDIN from it,
    # rather than writing them down a pipe before reading the response. A
    # write-then-read pipe deadlocks once the child's stdout fills the OS pipe
    # buffer (~64KB): the child blocks writing stdout (we're not reading yet)
    # while we block writing stdin (it's not reading). The temp file makes
    # request delivery non-blocking, so we only ever read on the parent side.
    my ($tfh, $tpath) = tempfile('corpus-batch-XXXXXX', TMPDIR => 1, UNLINK => 1);
    print $tfh $jsonl; close $tfh;
    # Child stderr goes to a file, not /dev/null: when the batch aborts, the
    # panic/abort message is the only evidence of WHY, and every unanswered
    # row scores CRASH with no cause attached. Kept only on abnormal exit.
    my ($efh, $epath) = tempfile('corpus-batch-err-XXXXXX', TMPDIR => 1, UNLINK => 1);
    close $efh;
    my @cpu0 = times();           # children utime/stime baseline (slots 2,3)
    my $t0 = time();
    my $pid = open(my $out, '-|');
    die "fork: $!" unless defined $pid;
    if ($pid == 0) {
        open(STDIN, '<', $tpath) or exit 127;
        open(STDERR, '>', $epath);
        $ENV{PERL5LIB} = $p5lib if defined $p5lib;
        if ($rename_scope) { $ENV{PERL_LSP_RENAME_SCOPE} = $rename_scope; }
        else { delete $ENV{PERL_LSP_RENAME_SCOPE}; }
        exec($bin, '--lang', $lang, '--batch', $root) or exit 127;
    }
    # Read line-by-line (the binary flushes per response): each arrival
    # delta is that query's processing cost; the first arrival also
    # carries startup (workspace index + cache warm). Peak RSS is
    # sampled from /proc after each line — the last sample before the
    # child exits is its high-water mark (Linux-only; silently absent
    # elsewhere).
    my %by_id;
    my $met = { wall_s => 0, first_s => undef, per_id => {}, vm_hwm_kb => 0,
                cpu_user_s => 0, cpu_sys_s => 0, root => $root };
    my $prev = $t0;
    while (my $line = <$out>) {
        my $now = time();
        next unless $line =~ /\S/;
        my $r = eval { decode_json($line) } or next;
        next unless defined $r->{id};
        $by_id{$r->{id}} = $r;
        if (defined $met->{first_s}) {
            $met->{per_id}{$r->{id}} = $now - $prev;
        } else {
            # The first arrival is dominated by startup (workspace index
            # + cache warm) — report it as startup, not as the first
            # row's capability cost.
            $met->{first_s} = $now - $t0;
        }
        $prev = $now;
        if (open(my $st, '<', "/proc/$pid/status")) {
            while (my $l = <$st>) {
                $met->{vm_hwm_kb} = $1 if $l =~ /^VmHWM:\s+(\d+)/;
            }
            close($st);
        }
    }
    close($out);
    my $st = $?;
    my @cpu1 = times();
    $met->{wall_s}     = time() - $t0;
    $met->{cpu_user_s} = $cpu1[2] - $cpu0[2];
    $met->{cpu_sys_s}  = $cpu1[3] - $cpu0[3];
    if ($st != 0) {
        $met->{abort} = WIFSIGNALED($st) ? "signal " . WTERMSIG($st)
                      : WIFEXITED($st)   ? "exit "   . WEXITSTATUS($st)
                      :                    "status $st";
        if (open(my $ef, '<', $epath)) {
            my @tail; while (my $l = <$ef>) { push @tail, $l; shift @tail if @tail > 25; }
            close($ef);
            $met->{stderr_tail} = join('', @tail);
        }
    }
    return wantarray ? (\%by_id, $met) : \%by_id;
}

# ---- --emit: canonical output for one ad-hoc query (fixture authoring) ----
if (@ARGV && $ARGV[0] eq '--emit') {
    shift @ARGV;
    my ($emit_root, $emit_lang);
    while (@ARGV && $ARGV[0] =~ /^--(root|lang)$/) {
        my $opt = shift @ARGV;
        if ($opt eq '--lang') { $emit_lang = shift @ARGV; next; }
        $emit_root = shift @ARGV;
        $emit_root = File::Spec->rel2abs("$RealBin/../$emit_root") unless $emit_root =~ m{^/};
    }
    my ($cap, @rest) = @ARGV;
    my $spec = $CAP{$cap} or die "unknown capability: $cap\n";
    # line/col arrive as ARGV strings; numify so encode_json emits JSON numbers
    # (the binary's BatchReq line/col are usize and reject quoted strings).
    my $row = $spec->{check} ? {}
            : $spec->{qarg}  ? { query => $rest[0] }
            :                  { file => $rest[0], line => ($rest[1] // 0) + 0, col => ($rest[2] // 0) + 0, newname => $rest[3] };
    my $req = batch_req($cap, $spec, $row, 'emit', $emit_root);
    my $resp = run_batch([ $req ], $emit_root, $emit_root ? p5lib_for($emit_root) : undef, undef, $emit_lang);
    my $r = $resp->{emit};
    if    (!$r)        { print "<<CRASH: process aborted>>\n"; }
    elsif (!$r->{ok})  { print "<<ERR: $r->{err}>>\n"; }
    else {
        my $norm = normalize($cap, $r->{out});
        print $norm; print "\n" unless $norm =~ /\n\z/;
    }
    exit 0;
}

# ---- load fixtures ----
die "no fixtures dir: $fxdir (generate it — see README)\n" unless -d $fxdir;
my @want = grep { !/^--/ } @ARGV;
my $list_only = grep { $_ eq '--list' } @ARGV;
my @rows;
for my $jf (sort glob("$fxdir/*.json")) {
    open(my $fh, '<', $jf) or die "open $jf: $!";
    local $/; my $j = decode_json(<$fh>); close($fh);
    next if @want && !grep { $_ eq $j->{capability} } @want;
    push @rows, map { { %$_, capability => $j->{capability} } } @{ $j->{rows} || [] };
}
# Drop rows the binary's languages can't serve (e.g. C++ fixtures on a
# perl-only build) — reported as `lang-skip`, never a failure.
my @lang_skip;
@rows = grep {
    !$_->{lang} || $served{$_->{lang}} || do {
        push @lang_skip, "$_->{capability}/$_->{id} (lang=$_->{lang}; binary serves: "
            . join(', ', sort keys %served) . ")";
        0;
    }
} @rows;

if ($list_only) {
    my %c; $c{$_->{capability}}{$_->{status} // 'gold'}++ for @rows;
    printf "%-20s %5s %6s %5s\n", 'capability', 'gold', 'xfail', 'prov';
    printf "%-20s %5d %6d %5d\n", $_, $c{$_}{gold}||0, $c{$_}{xfail}||0, $c{$_}{provisional}||0 for sort keys %c;
    exit 0;
}

# ---- run suite: group rows by workspace root, one --batch per root ----
my %groups; my %meta; my @order; my %gctx;
for my $r (@rows) {
    my $spec = $CAP{$r->{capability}} or next;
    my $key = "$r->{capability}/$r->{id}";
    $meta{$key} = [ $r, $spec ];
    push @order, $key;
    my $root = root_for($r);
    # Rows that pin `renameScope` ("dispatch") run in their own batch with
    # PERL_LSP_RENAME_SCOPE set — the override-fan-out mode is process-wide,
    # so it groups separately from the default (hierarchy) rows for that root.
    my $scope = $r->{renameScope} // '';
    # PERL5LIB is process-wide per batch, so rows differing in @INC group apart.
    my $inc = inc_roots_for($r);
    # One language per process: untagged rows are Perl.
    my $lang = $r->{lang} // 'perl';
    my $gkey = "$root\x00$scope\x00$inc\x00$lang";
    $gctx{$gkey} //= { root => $root, scope => $scope, inc => $inc, lang => $lang };
    push @{ $groups{$gkey} }, batch_req($r->{capability}, $spec, $r, $key, $root);
}

# Both passes run under a cache dir this process owns and throws away, so the
# cold pass is cold because nothing has written to it yet — not because the
# developer happened to clear theirs. The alternative, `--clear-cache`, wipes
# ~/.cache/perl-lsp for every project on the box as a side effect of running
# the suite; this touches nothing outside the tempdir.
# Clearing the real cache instead would be wrong in both directions: bare
# `--clear-cache` wipes every project's cache on the box, and clearing only the
# roots you remembered is worse than clearing none — an uncleared root makes the
# cold pass silently warm, and nothing in the output says which roots were
# reused. A cache dir that did not exist a moment ago cannot be partially cold.
my $cachedir = File::Temp::tempdir('corpus-cache-XXXXXX', TMPDIR => 1, CLEANUP => 1);
$ENV{XDG_CACHE_HOME} = $cachedir;
my %distinct_roots = map { $gctx{$_}{root} => 1 } keys %gctx;
my $n_roots = scalar keys %distinct_roots;

sub run_all_batches {
    my %by; my @mets;
    for my $gkey (sort keys %groups) {
        my ($root, $scope, $inc, $lang) = @{ $gctx{$gkey} }{qw(root scope inc lang)};
        my ($b, $met) = run_batch($groups{$gkey}, $root, p5lib_for($root, $inc), $scope, $lang);
        %by = (%by, %$b);
        push @mets, $met;
    }
    return (\%by, \@mets);
}

# Score one row's response: (ok?, normalized text, skip reason). Shared by both
# passes so a warm verdict is the SAME assertion as the cold one — a second
# copy of this logic is how the two lanes would silently drift apart.
sub score_row {
    my ($r, $rr) = @_;
    return (0, '', 'no response — batch aborted at/before this row') unless $rr;
    # ok:false is a graceful "no result" (e.g. file-not-found, no def); treat as empty output
    my $norm = $rr->{ok} ? normalize($r->{capability}, $rr->{out}) : '';
    return (0, '', $rr->{err})
        if !$rr->{ok} && $rr->{err} && $rr->{err} =~ /^file not found/;
    my $ok = 1;
    for my $s (@{ $r->{expect}{all}  || [] }) { $ok = 0, last if index($norm, $s) < 0; }
    if ($ok) { for my $s (@{ $r->{expect}{none} || [] }) { $ok = 0, last if index($norm, $s) >= 0; } }
    if ($ok && $r->{expect}{exact_labels}) {
        my @got  = sort grep { length } map { my $l = (split /\t/, $_)[0] // ''; $l =~ s/^\s+|\s+$//g; $l } split /\n/, $norm;
        my @want = sort @{ $r->{expect}{exact_labels} };
        $ok = 0 unless @got == @want && join("\0", @got) eq join("\0", @want);
    }
    if ($ok && defined $r->{expect}{max_items}) {
        my @got = grep { length } map { my $l = (split /\t/, $_)[0] // ''; $l =~ s/^\s+|\s+$//g; $l } split /\n/, $norm;
        $ok = 0 if @got > $r->{expect}{max_items};
    }
    return ($ok, $norm, undef);
}

my ($resp, $bm) = run_all_batches();
my @batch_metrics = @$bm;

my (@fail, @xpass, @crash, @skip, @prov); my ($pass, $xfail) = (0, 0);
my %cold_ok;
for my $key (@order) {
    my ($r, $spec) = @{ $meta{$key} };
    my $status = $r->{status} // 'gold';
    my ($ok, $norm, $err) = score_row($r, $resp->{$key});
    if (defined $err) {
        if ($err =~ /^no response/) { push @crash, "$key ($err)" } else { push @skip, "$key ($err)" }
        next;
    }
    $cold_ok{$key} = $ok;
    if ($status eq 'gold') {
        if ($ok) { $pass++ } else { push @fail, "$key\n      expect: " . encode_json($r->{expect}) . "\n      got: " . substr($norm, 0, 200) }
    } elsif ($status eq 'xfail') {
        if ($ok) { push @xpass, "$key (XPASS — promote to gold)" } else { $xfail++ }
    } else {
        push @prov, $key unless $ok;
    }
}

# ---- warm pass: the same suite again, against the cache the cold pass wrote ----
# Cold is the first time a user opens a project; warm is every time after. A
# fact that is derived at registration and not rebuilt from a cache blob answers
# correctly once and silently stops answering — no error, no crash, just a
# capability that quietly went missing on day two. Cold-only runs cannot see
# that class at all, which is why this is a lane and not an assertion folded
# into the first pass: `warm-FAIL` names the state transition, not the row.
#
# Deliberately NOT solved by having the harness clear the cache and run once.
# That makes the suite deterministic by deleting the only evidence the bug
# exists — the green run would be honest about the cold path and silent about
# the one users actually spend their time on.
my (@warm_fail, @warm_xpass); my $warm_xfail = 0; my $warm_wall = 0;
my $do_warm = !grep { $_ eq '--no-warm' } @ARGV;
if ($do_warm) {
    my ($wresp, $wmets) = run_all_batches();
    $warm_wall += $_->{wall_s} for @$wmets;
    for my $key (@order) {
        my ($r) = @{ $meta{$key} };
        # Gold rows that passed cold are the only ones with a warm claim to
        # make. A row already FAILing cold has nothing to regress from, and
        # xfail/provisional rows assert nothing the second pass could break.
        next unless ($r->{status} // 'gold') eq 'gold' && $cold_ok{$key};
        my ($ok) = score_row($r, $wresp->{$key});
        my $known = ($r->{warm} // '') eq 'xfail';
        if ($ok) {
            push @warm_xpass, "$key (warm-XPASS — drop \"warm\": \"xfail\" from the row)" if $known;
        } elsif ($known) {
            $warm_xfail++;
        } else {
            push @warm_fail, "$key (passes cold, fails warm)";
        }
    }
}
printf "\nGold-corpus harness\n  binary: %s\n  corpus: %s\n", $bin, $corpus;
# Naming the cache and the root count makes "every root started cold" something
# the reader can check, rather than a property they have to trust.
printf "  cache:  %s (private, empty at start; %d roots)\n\n", $cachedir, $n_roots;
printf "  %-9s %d\n", 'PASS',  $pass;
printf "  %-9s %d\n", 'xfail', $xfail;
printf "  %-9s %d\n", 'FAIL',  scalar @fail;
printf "  %-9s %d\n", 'XPASS', scalar @xpass;
printf "  %-9s %d\n", 'CRASH', scalar @crash;
# ALWAYS printed, even at zero, and counted as a failure below. A row whose
# input did not resolve did not run, and a run that did not run a row has not
# tested it — reporting that quietly under a green summary is the one thing
# this harness must never do. Printing the zero makes "nothing was skipped" an
# assertion a reader can see rather than an absence they have to notice.
printf "  %-9s %d\n", 'skip',  scalar @skip;
printf "  %-9s %d\n", 'prov?', scalar @prov if @prov;
# lang-skip is NOT a failure: a default (perl-only) build legitimately cannot
# serve the cpp rows, and CI builds exactly that. Still printed unconditionally
# so the count is visible rather than inferred.
printf "  %-9s %d\n", 'lang-skip', scalar @lang_skip;
# Printed unconditionally when the warm pass ran, zeros included: "no row
# regressed on rehydration" is a result worth seeing asserted. When the pass is
# skipped the line says so, so a green summary can never be mistaken for one
# that checked the warm path.
if ($do_warm) {
    printf "  %-9s %d\n", 'warm-FAIL',  scalar @warm_fail;
    printf "  %-9s %d\n", 'warm-XPASS', scalar @warm_xpass;
    printf "  %-9s %d\n", 'warm-xfail', $warm_xfail;
} else {
    printf "  %-9s %s\n", 'warm', '(skipped: --no-warm)';
}
print "\n!! CRASH (process aborted):\n",                 map { "  - $_\n" } @crash if @crash;
# Attribute each aborted batch: which root, how it died, and the stderr it
# left behind — without this a CRASH block names the victims but not the cause.
for my $m (grep { $_->{abort} } @batch_metrics) {
    print "\n!! batch abort: root=$m->{root} ($m->{abort})\n";
    if (defined $m->{stderr_tail} && $m->{stderr_tail} =~ /\S/) {
        print "   stderr tail:\n";
        print "   | $_\n" for split /\n/, $m->{stderr_tail};
    } else {
        print "   (stderr empty)\n";
    }
}
print "\n!! FAIL (gold assertion no longer holds):\n",   map { "  - $_\n" } @fail  if @fail;
print "\n** XPASS (known gap fixed — update fixture):\n", map { "  - $_\n" } @xpass if @xpass;
print "\n!! warm-FAIL (passes on a cold cache, fails on a warm one):\n",
                                                          map { "  - $_\n" } @warm_fail  if @warm_fail;
print "\n** warm-XPASS (warm gap fixed — update fixture):\n",
                                                          map { "  - $_\n" } @warm_xpass if @warm_xpass;
print "\nprovisional misses:\n",                         map { "  - $_\n" } @prov  if @prov;
print "\n!! SKIP (row did not run — its input did not resolve):\n",
                                                          map { "  - $_\n" } @skip  if @skip;
print "\nlang-skipped (binary doesn't serve that language):\n", map { "  - $_\n" } @lang_skip if @lang_skip;
print "\n";

# ---- cost report: what do features cost? ----
# Per-capability wall time (each response's arrival delta attributed to
# its row's capability), per-root startup (first-response latency,
# dominated by workspace index + cache warm), child CPU, and peak RSS.
# Written to $METRICS_OUT as JSON when set, for run-over-run comparison.
{
    my %by_cap;
    for my $met (@batch_metrics) {
        while (my ($id, $dt) = each %{ $met->{per_id} }) {
            my ($cap) = $id =~ m{^([^/]+)/};
            $cap //= $id;
            push @{ $by_cap{$cap} }, $dt;
        }
    }
    my ($cpu_u, $cpu_s, $hwm, $wall) = (0, 0, 0, 0);
    for my $met (@batch_metrics) {
        $cpu_u += $met->{cpu_user_s};
        $cpu_s += $met->{cpu_sys_s};
        $wall  += $met->{wall_s};
        $hwm = $met->{vm_hwm_kb} if $met->{vm_hwm_kb} > $hwm;
    }
    print "Cost report\n";
    printf "  %-20s %4s %9s %8s %8s\n", 'capability', 'n', 'total ms', 'mean ms', 'max ms';
    for my $cap (sort { _sum($by_cap{$b}) <=> _sum($by_cap{$a}) } keys %by_cap) {
        my @d = @{ $by_cap{$cap} };
        my $tot = _sum(\@d);
        my ($max) = sort { $b <=> $a } @d;
        printf "  %-20s %4d %9.1f %8.1f %8.1f\n",
            $cap, scalar @d, $tot * 1000, $tot * 1000 / @d, $max * 1000;
    }
    for my $met (@batch_metrics) {
        printf "  startup %-46s %8.1f ms\n",
            _bn($met->{root}), ($met->{first_s} // 0) * 1000;
    }
    printf "  wall %.2fs   cpu user %.2fs sys %.2fs   peak rss %.1f MB\n",
        $wall, $cpu_u, $cpu_s, $hwm / 1024;
    # The cost report measures the COLD pass; the warm wall is printed beside it
    # because "what does a warm start actually save" is the number this second
    # pass yields for free, and it is the one a caching change has to move.
    printf "  warm wall %.2fs\n", $warm_wall if $do_warm;
    print "\n";
    if (my $mo = $ENV{METRICS_OUT}) {
        my %caps = map {
            my @d = @{ $by_cap{$_} };
            ($_ => { n => scalar @d, total_ms => _sum(\@d) * 1000 });
        } keys %by_cap;
        if (open(my $fh, '>', $mo)) {
            print $fh JSON::PP->new->canonical->encode({
                wall_s => $wall, cpu_user_s => $cpu_u, cpu_sys_s => $cpu_s,
                peak_rss_kb => $hwm, capabilities => \%caps,
                startup_ms => { map { _bn($_->{root}) => ($_->{first_s} // 0) * 1000 } @batch_metrics },
            });
            close($fh);
        }
    }
}
sub _sum { my $t = 0; $t += $_ for @{ $_[0] }; $t }

# @skip fails the run. It is not a softer FAIL — it is worse, because a failing
# row at least proves the harness reached it. @lang_skip is excluded on purpose:
# it means the binary does not serve that language, which is a real supported
# configuration (CI's perl-only build), not a missing input.
exit((@fail || @crash || @xpass || @skip || @warm_fail || @warm_xpass) ? 1 : 0);
