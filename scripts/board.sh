#!/usr/bin/env bash
# Message-board client for a GitHub issue used as a multi-agent channel.
#
# Every agent on an arc posts to and reads from one issue. Doing that with a
# bare `gh api` call is wrong in four ways that all fail SILENTLY — the caller
# gets plausible output and no error. Each is encoded here so nobody rediscovers
# it:
#
#   1. `?per_page=100` returns the FIRST page. On a thread past 100 comments the
#      newest are on page 2+, so `.[-1]` freezes on a stale comment and a watcher
#      built on it goes quiet while the board is active. Fix: `--paginate`.
#   2. `sort=created&direction=desc` is IGNORED by the issue-comments endpoint.
#      It does not error; it returns the OLDEST page. Fix: never sort server-side,
#      order by id locally.
#   3. Under `--paginate`, gh runs the jq filter ONCE PER PAGE, so `.[-1].id`
#      emits one id per page, not one id total. It looks like a single value
#      when the thread is short and silently becomes N lines when it grows;
#      taking any one of them without ordering (or reading the first) gives a
#      stale comment. Fix: order by id explicitly, never trust array position.
#   4. Filtering by author to skip your own posts silences EVERYONE when the
#      agents share one GitHub account, which they do. Fix: record the ids of
#      comments this script posts, and skip exactly those.
#
# Usage:
#   scripts/board.sh new [issue]           unseen comments, oldest first; marks seen
#   scripts/board.sh peek [issue]          same, but does NOT mark seen
#   scripts/board.sh watch [issue] [secs]  poll; print each new comment as it lands
#   scripts/board.sh post <file> [issue]   post a comment, record it as self
#   scripts/board.sh catchup [issue]       mark everything seen without printing
#   scripts/board.sh show <comment-id>     print one comment in full
#
# State lives in .git/board-state/<issue>/ — inside .git so it is per-clone,
# never committed, and survives a worktree switch.
set -euo pipefail

REPO="${BOARD_REPO:-tree-sitter-perl/perl-lsp}"
DEFAULT_ISSUE="${BOARD_ISSUE:-120}"

_state_dir() {
  local issue="$1" root
  root="$(git rev-parse --git-common-dir 2>/dev/null || echo .git)"
  # --git-common-dir is relative in some worktrees; anchor it.
  case "$root" in /*) ;; *) root="$(cd "$root" && pwd)" ;; esac
  printf '%s/board-state/%s' "$root" "$issue"
}

# --- transport -------------------------------------------------------------
# Prefer `gh`; fall back to unauthenticated curl, because some agent sandboxes
# have no gh CLI at all (GitHub reaches those through MCP tools instead). This
# repo is public, so READS work unauthenticated, at a lower rate limit; POSTING
# still needs gh or $GITHUB_TOKEN. Anyone with neither should follow the four
# rules in the header BY HAND — they are the contract, and this script is only
# one implementation of it.
# BOARD_NO_GH=1 forces the curl path — the only way to exercise the fallback on
# a box that has gh, and therefore the only way it stays working.
HAVE_GH=0
if [ "${BOARD_NO_GH:-0}" != "1" ] && command -v gh >/dev/null 2>&1; then HAVE_GH=1; fi

_curl_pages() {  # walk pages explicitly; never trust a single page to be all
  local path="$1" page=1 body
  local auth=(); [ -n "${GITHUB_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
  while :; do
    body="$(curl -sSf "${auth[@]}" -H 'Accept: application/vnd.github+json' \
             "https://api.github.com/$path?per_page=100&page=$page" 2>/dev/null)" || break
    [ "$(jq 'length' <<<"$body" 2>/dev/null || echo 0)" -eq 0 ] && break
    printf '%s\n' "$body"
    page=$((page+1))
  done
}

_jqfmt() { printf '%s' '"\u001b[36m─── #\(.id)  \(.user.login)  \(.created_at)  \(.html_url)\u001b[0m\n\(.body)\n"'; }

# All comment ids, ascending. The one place that talks to the API.
_all_ids() {
  if [ $HAVE_GH -eq 1 ]; then
    gh api --paginate "repos/$REPO/issues/$1/comments" --jq '.[].id' | sort -n
  else
    _curl_pages "repos/$REPO/issues/$1/comments" | jq -r '.[].id' | sort -n
  fi
}

_render() {
  local id="$1" fmt; fmt="$(_jqfmt)"
  if [ $HAVE_GH -eq 1 ]; then
    gh api "repos/$REPO/issues/comments/$id" --jq "$fmt"
  else
    local auth=(); [ -n "${GITHUB_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
    curl -sSf "${auth[@]}" -H 'Accept: application/vnd.github+json' \
      "https://api.github.com/repos/$REPO/issues/comments/$id" | jq -r "$fmt"
  fi
}

_brief() {  # one line, for the watch loop / Monitor
  local id="$1" fmt='"[#\(.id)] \(.body[0:400] | gsub("\n";" "))"'
  if [ $HAVE_GH -eq 1 ]; then
    gh api "repos/$REPO/issues/comments/$id" --jq "$fmt" 2>/dev/null
  else
    local auth=(); [ -n "${GITHUB_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
    curl -sSf "${auth[@]}" -H 'Accept: application/vnd.github+json' \
      "https://api.github.com/repos/$REPO/issues/comments/$id" 2>/dev/null | jq -r "$fmt"
  fi
}

_unseen() {
  local issue="$1" dir seen
  dir="$(_state_dir "$issue")"; mkdir -p "$dir"
  seen="$dir/seen"; touch "$seen"
  # comm needs lexically-sorted input; ids are fixed-width enough in practice,
  # but sort -n then re-sort as text keeps comm correct regardless.
  comm -23 <(_all_ids "$issue" | sort) <(sort "$seen") | sort -n
}

_mark() {
  local issue="$1" dir; dir="$(_state_dir "$issue")"; mkdir -p "$dir"
  cat >> "$dir/seen"
  sort -u -o "$dir/seen" "$dir/seen"
}

cmd_new() {
  local issue="${1:-$DEFAULT_ISSUE}" ids n=0
  ids="$(_unseen "$issue")"
  [ -z "$ids" ] && { echo "no new comments on #$issue"; return 0; }
  while read -r id; do [ -n "$id" ] || continue; _render "$id"; n=$((n+1)); done <<< "$ids"
  printf '%s\n' "$ids" | _mark "$issue"
  echo "($n new on #$issue, now marked seen)"
}

cmd_peek() {
  local issue="${1:-$DEFAULT_ISSUE}" ids
  ids="$(_unseen "$issue")"
  [ -z "$ids" ] && { echo "no new comments on #$issue"; return 0; }
  while read -r id; do [ -n "$id" ] || continue; _render "$id"; done <<< "$ids"
  echo "(not marked seen — use 'new' to consume)"
}

cmd_catchup() {
  local issue="${1:-$DEFAULT_ISSUE}" ids
  ids="$(_unseen "$issue")"
  [ -z "$ids" ] && { echo "already caught up on #$issue"; return 0; }
  printf '%s\n' "$ids" | _mark "$issue"
  echo "marked $(printf '%s\n' "$ids" | grep -c .) comment(s) seen on #$issue"
}

# One line per new comment, so it can drive a Monitor. Never exits on its own.
cmd_watch() {
  local issue="${1:-$DEFAULT_ISSUE}" every="${2:-60}" ids
  while true; do
    ids="$(_unseen "$issue" || true)"
    if [ -n "$ids" ]; then
      while read -r id; do
        [ -n "$id" ] || continue
        _brief "$id" || true
      done <<< "$ids"
      printf '%s\n' "$ids" | _mark "$issue"
    fi
    sleep "$every"
  done
}

cmd_post() {
  local file="$1" issue="${2:-$DEFAULT_ISSUE}" url id dir
  [ -f "$file" ] || { echo "no such file: $file" >&2; return 1; }
  if [ $HAVE_GH -eq 0 ]; then
    # Posting needs a write credential. Say so plainly rather than failing with
    # "gh: command not found" three frames down.
    echo "board.sh post needs the gh CLI (not found)." >&2
    echo "Reads work without it; posting does not. Use your sandbox's GitHub" >&2
    echo "tooling to comment, then: scripts/board.sh catchup $issue" >&2
    return 127
  fi
  url="$(gh issue comment "$issue" --repo "$REPO" --body-file "$file")"
  id="${url##*-}"
  dir="$(_state_dir "$issue")"; mkdir -p "$dir"
  # Record as seen AND as self, so the watcher never reports our own post back
  # to us. Exact by construction — no marker string to forget, and no author
  # filter (which would silence every agent sharing this account).
  printf '%s\n' "$id" | _mark "$issue"
  printf '%s\n' "$id" >> "$dir/self"
  echo "$url"
}

cmd_show() { _render "$1"; }

case "${1:-}" in
  new)     shift; cmd_new "$@" ;;
  peek)    shift; cmd_peek "$@" ;;
  watch)   shift; cmd_watch "$@" ;;
  post)    shift; cmd_post "$@" ;;
  catchup) shift; cmd_catchup "$@" ;;
  show)    shift; cmd_show "$@" ;;
  *) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
