#!/bin/sh
# Prepare a release: set the version in both manifests and regenerate the
# changelog. Review the diff, then commit and tag.
set -eu

version=${1:-}
case "$version" in
    v[0-9]*) ;;
    *)
        echo "usage: scripts/release.sh vX.Y.Z" >&2
        exit 2
        ;;
esac
number=${version#v}

command -v git-cliff >/dev/null 2>&1 || {
    echo "git-cliff is required: cargo install git-cliff" >&2
    exit 1
}
[ -z "$(git status --porcelain)" ] || {
    echo "working tree is dirty" >&2
    exit 1
}
git rev-parse "$version" >/dev/null 2>&1 && {
    echo "$version already exists; bump instead of moving it" >&2
    exit 1
}

# Both manifests carry the version: cargo reads one, Herdr the other.
for manifest in Cargo.toml herdr-plugin.toml; do
    sed -i "0,/^version = \".*\"/s//version = \"$number\"/" "$manifest"
done
cargo update --workspace --quiet

git-cliff --config cliff.toml --tag "$version" -o CHANGELOG.md

echo
echo "Prepared $version. Review, then:"
echo "  git commit -am 'chore(release): $version'"
echo "  git tag -a $version -m 'herdr-dictate $number'"
echo "  git push && git push origin $version"
