#!/usr/bin/env bash
# Builds the oracle: PostgreSQL from the pin in pins.toml, with the flags below, into one prefix.
#
# Usage: oracle/build.sh <source> <prefix>
#
# <source> is a PostgreSQL checkout at the pin. The caller checks the commit, so this script only
# builds. <prefix> is the install directory. The build directory is removed at the end, because the
# prefix is the only thing a run needs and the build directory is three times its size.
#
# The flags are a release build with no assertions, per document 14 section 14.3 of the notes. The
# features that are off are features that no suite in this repository needs. Each one that is on
# is a library that the server links, so each one that is off is one fewer thing that can differ
# between two machines that build the same pin.

set -euo pipefail

source=$1
prefix=$2
build=$prefix.build

if [[ "$(uname)" == Darwin ]] && command -v brew >/dev/null; then
  # meson finds OpenSSL through pkg-config, and Homebrew does not link openssl@3 into the
  # default search path.
  export PKG_CONFIG_PATH="$(brew --prefix openssl@3)/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi

rm -rf "$build" "$prefix"
meson setup "$build" "$source" \
  --prefix="$prefix" \
  --buildtype=release \
  -Dcassert=false \
  -Dssl=openssl \
  -Duuid=e2fs \
  -Dicu=disabled \
  -Dnls=disabled \
  -Dtap_tests=disabled \
  -Dllvm=disabled \
  -Dlz4=disabled \
  -Dzstd=disabled \
  -Dreadline=disabled \
  -Dgssapi=disabled \
  -Dldap=disabled \
  -Dlibxml=disabled \
  -Dlibxslt=disabled \
  -Dplperl=disabled \
  -Dplpython=disabled \
  -Dpltcl=disabled

ninja -C "$build"
ninja -C "$build" install
# The test modules and regress.so, which the regression suite loads with LANGUAGE C.
ninja -C "$build" install-test-files >/dev/null 2>&1 || true

# The test programs are not installed by meson. The runners need them next to the server.
mkdir -p "$prefix/test/bin"
for program in \
  src/test/regress/pg_regress \
  src/test/isolation/isolationtester \
  src/test/isolation/pg_isolation_regress; do
  cp "$build/$program" "$prefix/test/bin/"
done
mkdir -p "$prefix/test/pipeline/bin"
cp "$build/src/test/modules/libpq_pipeline/libpq_pipeline" "$prefix/test/pipeline/bin/"
# The build tree gives each test program a search path to libpq relative to the program:
# ../../interfaces/libpq for the programs in test/bin, and ../../../interfaces/libpq for
# libpq_pipeline, which is one level deeper in the tree. From test/bin and test/pipeline/bin both
# are the prefix itself, so a link there finds the installed libpq.
mkdir -p "$prefix/interfaces/libpq"
find "$prefix/lib" \( -name 'libpq.so*' -o -name 'libpq.*dylib' \) -exec ln -sf {} "$prefix/interfaces/libpq/" \;
for module in "$build"/src/test/regress/regress.*; do
  case "$module" in
    *.so | *.dylib | *.dll) cp "$module" "$prefix/test/bin/" ;;
  esac
done

# The inputs of the runners, so that a run needs the prefix and not the source.
for dir in regress isolation; do
  mkdir -p "$prefix/test/$dir"
  (cd "$source/src/test/$dir" && tar cf - --exclude='*.c' --exclude='*.h' --exclude='*.y' --exclude='*.l' --exclude=meson.build --exclude=Makefile .) | (cd "$prefix/test/$dir" && tar xf -)
done
mkdir -p "$prefix/test/pipeline"
cp -R "$source/src/test/modules/libpq_pipeline/traces" "$prefix/test/pipeline/"

git -C "$source" rev-parse HEAD > "$prefix/PIN"
rm -rf "$build"
echo "oracle installed in $prefix"
