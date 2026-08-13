#!/bin/bash
# Publish this fork to GitHub, always the same way.
#
# The fork is meant to look like upstream plus our code changes and nothing
# else, so pull requests read as a clean diff. Local working material —
# CLAUDE.md, the review and bug-report docs, the changelog, build output —
# stays on the machine and is never pushed.
#
#   ./publish-to-fork.sh              # dry run: show exactly what would go up
#   ./publish-to-fork.sh --push       # actually push
#
# Mechanism: a throwaway `publish` branch is rebuilt from the current branch,
# the local-only files are removed from it, README.md is checked, and that
# branch is force-pushed. Your working branch is never modified — the script
# returns you to it whether it succeeds or fails.

set -euo pipefail

REMOTE="${REMOTE:-origin}"
PUBLISH_BRANCH="${PUBLISH_BRANCH:-publish}"

# Files that exist only locally. Everything here is deliberately withheld:
# CLAUDE.md is agent instructions, docs/CODE_REVIEW.md and the KiCad bug
# reports are our working notes, version_history.md and README-FORK.md are
# superseded upstream by README.md itself.
LOCAL_ONLY=(
	"CLAUDE.md"
	"version_history.md"
	"README-FORK.md"
	"docs/CODE_REVIEW.md"
	"docs/kicad-bug-report-eeschema-api-null-frame.md"
	"docs/kicad-bug-report-forum-post.md"
	"publish-to-fork.sh"
)

# Upstream tag this fork is based on — used to flag files we added that are
# neither code nor on the withhold list, so nothing sneaks into a PR.
UPSTREAM_TAG="${UPSTREAM_TAG:-v0.2.2}"
UPSTREAM_REPO="${UPSTREAM_REPO:-mixelpixx/Konnect}"

cd "$(dirname "${BASH_SOURCE[0]}")"

PUSH=0
[ "${1:-}" = "--push" ] && PUSH=1

# ─── Preconditions ───────────────────────────────────────────────────────────
if [ -n "$(git status --porcelain)" ]; then
	echo "ERROR: working tree is dirty. Commit or stash first." >&2
	git status --short >&2
	exit 1
fi

SOURCE_BRANCH="$(git branch --show-current)"
if [ -z "$SOURCE_BRANCH" ]; then
	echo "ERROR: detached HEAD; check out a branch first." >&2
	exit 1
fi

if ! git remote get-url "$REMOTE" >/dev/null 2>&1; then
	echo "ERROR: remote '$REMOTE' is not configured." >&2
	echo "  git remote add $REMOTE git@github.com:JYPochez/VNS-Kicad-Konnect.git" >&2
	exit 1
fi

# The publish branch is built in a throwaway worktree, never by switching this
# one. `git rm --cached` leaves the removed files on disk as untracked, which
# then blocks checking the source branch back out — building elsewhere avoids
# touching your working tree at all.
WORKTREE="$(mktemp -d)/publish"
cleanup() {
	git worktree remove --force "$WORKTREE" 2>/dev/null || true
	rmdir "$(dirname "$WORKTREE")" 2>/dev/null || true
}
trap cleanup EXIT

echo "source branch : $SOURCE_BRANCH"
echo "remote        : $REMOTE ($(git remote get-url "$REMOTE"))"
echo "publish branch: $PUBLISH_BRANCH"
echo

# ─── Gate: the change must actually build and pass ───────────────────────────
if [ "$PUSH" = "1" ]; then
	echo "Running the upstream CI gate before pushing…"
	export PATH="$HOME/.cargo/bin:$PATH"
	export PROTOC="${PROTOC:-$(command -v protoc || true)}"
	cargo fmt --all -- --check
	cargo clippy --workspace -- -D warnings
	cargo test --workspace --lib --tests >/dev/null
	echo "  gate passed"
	echo
fi

# ─── Build the publish branch in isolation ───────────────────────────────────
git branch -f "$PUBLISH_BRANCH" "$SOURCE_BRANCH"
git worktree add -q --checkout "$WORKTREE" "$PUBLISH_BRANCH"

REMOVED=()
for f in "${LOCAL_ONLY[@]}"; do
	if git -C "$WORKTREE" ls-files --error-unmatch "$f" >/dev/null 2>&1; then
		git -C "$WORKTREE" rm -q -f "$f"
		REMOVED+=("$f")
	fi
done

if [ ${#REMOVED[@]} -gt 0 ]; then
	git -C "$WORKTREE" commit -q -m "chore: exclude local-only working files from the published fork

CLAUDE.md, the code review, the KiCad bug reports, the local changelog and
this script are working material for the machine this fork is developed on.
The published tree is upstream's file set plus the code changes, so a pull
request reads as a clean diff."
fi

# ─── Warn about anything else we added that upstream does not have ───────────
echo "Checking for files not present in upstream $UPSTREAM_TAG …"
if UP_LIST=$(git ls-tree -r --name-only "upstream/$UPSTREAM_TAG" 2>/dev/null); then
	:
elif UP_LIST=$(curl -sfL "https://api.github.com/repos/$UPSTREAM_REPO/git/trees/$UPSTREAM_TAG?recursive=1" |
	python3 -c 'import json,sys; print("\n".join(e["path"] for e in json.load(sys.stdin).get("tree",[])))' 2>/dev/null); then
	:
else
	UP_LIST=""
	echo "  (could not read upstream file list — skipping this check)"
fi

if [ -n "$UP_LIST" ]; then
	EXTRA=$(comm -23 <(git -C "$WORKTREE" ls-files | sort) <(echo "$UP_LIST" | sort) || true)
	if [ -n "$EXTRA" ]; then
		echo "  files added on top of upstream (verify each belongs in a PR):"
		echo "$EXTRA" | sed 's/^/    /'
	else
		echo "  file set matches upstream exactly"
	fi
fi
echo

# ─── Report ──────────────────────────────────────────────────────────────────
echo "Withheld from the push:"
if [ ${#REMOVED[@]} -eq 0 ]; then
	echo "    (none present)"
else
	printf '    %s\n' "${REMOVED[@]}"
fi
echo
echo "Commits that would be pushed:"
git log --oneline "$UPSTREAM_TAG..$PUBLISH_BRANCH" 2>/dev/null | sed 's/^/    /' ||
	git log --oneline -8 | sed 's/^/    /'
echo

if [ "$PUSH" = "1" ]; then
	git push -f "$REMOTE" "$PUBLISH_BRANCH:$PUBLISH_BRANCH"
	echo
	echo "Pushed $PUBLISH_BRANCH to $REMOTE."
	echo "Open a PR from JYPochez/VNS-Kicad-Konnect:$PUBLISH_BRANCH against $UPSTREAM_REPO."
else
	echo "DRY RUN — nothing was pushed. Re-run with --push when ready."
fi
