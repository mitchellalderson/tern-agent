#!/bin/sh
# Check a release before tagging it. Publishes nothing, creates no tag.
#
#   scripts/release-dry-run.sh [--build] [--tag vX.Y.Z[-pre.N]]
#
# 1. Cargo.toml, apps/desktop/pubspec.yaml and CHANGELOG.md agree on the
#    version, and it is valid semver (with an optional -alpha.N, -beta.N, -rc.N).
# 2. If --tag is given, it matches that version (what release.yml enforces).
# 3. actionlint on the release workflows, when it is installed.
# 4. shellcheck on the install and packaging scripts, when it is installed.
# 5. --build: builds the CLI for this machine with --profile dist (slow: LTO),
#    runs `tern --version` and checks it reports the version. Skipped by default.
# 6. Prints what pushing the tag would publish.
#
# Exit status is non-zero if any check fails. A missing optional tool is
# reported and skipped, not failed.
set -eu

build=0
want_tag=
while [ $# -gt 0 ]; do
  case "$1" in
    --build) build=1 ;;
    --tag)
      [ $# -ge 2 ] || { echo "--tag needs a value" >&2; exit 2; }
      want_tag="$2"; shift ;;
    -h | --help) sed -n '2,19p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
failed=0
ok() { printf '  ok    %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*" >&2; failed=1; }
skip() { printf '  skip  %s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }

# --- versions ---------------------------------------------------------------
step "versions agree"
cargo_v="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
pub_full="$(sed -n 's/^version: *//p' apps/desktop/pubspec.yaml)"
pub_v="${pub_full%%+*}"
echo "  Cargo.toml   $cargo_v"
echo "  pubspec.yaml $pub_full"
semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'
if printf '%s' "$cargo_v" | grep -Eq "$semver"; then ok "Cargo.toml version is semver"; else bad "Cargo.toml version '$cargo_v' is not semver"; fi
if [ "$cargo_v" = "$pub_v" ]; then ok "pubspec version matches Cargo.toml"; else bad "pubspec ($pub_v) differs from Cargo.toml ($cargo_v)"; fi
case "$pub_full" in
  *+[0-9]*) ok "pubspec has a +build suffix" ;;
  *) bad "pubspec version has no +N build suffix" ;;
esac
if [ -f CHANGELOG.md ] && grep -q "^## \[$cargo_v\]" CHANGELOG.md; then
  ok "CHANGELOG.md has an entry for $cargo_v"
else
  bad "CHANGELOG.md has no '## [$cargo_v]' entry"
fi
if grep -q "name = \"tern\"" Cargo.lock && ! grep -A1 'name = "tern"' Cargo.lock | grep -q "version = \"$cargo_v\""; then
  bad "Cargo.lock does not have tern at $cargo_v (run: cargo update --workspace)"
else
  ok "Cargo.lock matches"
fi
tag="v$cargo_v"
if [ -n "$want_tag" ]; then
  if [ "$want_tag" = "$tag" ]; then ok "--tag $want_tag matches"; else bad "--tag $want_tag but the versions say $tag"; fi
fi
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null 2>&1; then
  bad "tag $tag already exists locally"
else
  ok "tag $tag does not exist locally yet"
fi
case "$cargo_v" in
  *-*) kind=pre-release ;;
  *) kind=release ;;
esac

# --- workflows and scripts --------------------------------------------------
step "workflows"
if command -v actionlint >/dev/null 2>&1; then
  if actionlint .github/workflows/release.yml .github/workflows/desktop-release.yml; then
    ok "actionlint release.yml desktop-release.yml"
  else
    bad "actionlint"
  fi
else
  skip "actionlint is not installed (brew install actionlint)"
fi

step "scripts"
if command -v shellcheck >/dev/null 2>&1; then
  if shellcheck install.sh scripts/*.sh apps/desktop/linux/packaging/*.sh; then
    ok "shellcheck"
  else
    bad "shellcheck"
  fi
else
  skip "shellcheck is not installed (brew install shellcheck)"
fi

# --- build ------------------------------------------------------------------
step "build (host)"
if [ "$build" = 1 ]; then
  cargo build --profile dist --locked -p tern
  got="$(target/dist/tern --version)"
  echo "  $got"
  case "$got" in
    *"$cargo_v"*) ok "tern --version reports $cargo_v" ;;
    *) bad "tern --version does not mention $cargo_v" ;;
  esac
else
  skip "not built; pass --build to run cargo build --profile dist (slow)"
fi

# --- what the tag publishes -------------------------------------------------
step "pushing $tag would publish"
repo="${GITHUB_REPOSITORY:-mitchellalderson/tern-agent}"
echo "  a GitHub $kind \"tern $tag\"$([ "$kind" = pre-release ] && echo ' (--prerelease, not latest)') on $repo, after the CI checks pass, with:"
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin; do
  echo "    tern-$t.tar.gz (+ .sha256)"
done
echo "    tern-desktop-macos.dmg (+ .sha256)   ad-hoc signed unless the Apple secrets exist"
echo "    tern-desktop-macos.zip (+ .sha256)"
echo "    SHA256SUMS"
echo "  plus generated release notes and build provenance attestations."
echo "  macOS bundle version: ${cargo_v%%-*} (build number derived by scripts/release-macos.sh)"
echo "  install:  curl -fsSL https://raw.githubusercontent.com/$repo/main/install.sh | sh"
echo "  desktop:  https://github.com/$repo/releases/download/$tag/tern-desktop-macos.dmg"
echo
echo "  tag command (not run):  git tag -a $tag -m \"tern $tag\" && git push origin $tag"

echo
if [ "$failed" = 0 ]; then
  echo "release-dry-run: all checks passed"
else
  echo "release-dry-run: FAILED" >&2
  exit 1
fi
