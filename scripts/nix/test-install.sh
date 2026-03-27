#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=./install.sh
source "$SCRIPT_DIR/install.sh"

assert_eq() {
    local actual="$1"
    local expected="$2"
    local message="$3"

    if [ "$actual" != "$expected" ]; then
        echo "FAIL: $message" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

run_architecture_case() {
    local os_name="$1"
    local cpu_name="$2"
    local ldd_version="$3"
    local expected="$4"

    MOCK_UNAME_S="$os_name"
    MOCK_UNAME_M="$cpu_name"
    MOCK_LDD_VERSION="$ldd_version"
    RETVAL=""

    get_architecture

    assert_eq "$RETVAL" "$expected" "$os_name/$cpu_name with ldd '$ldd_version'"
}

run_download_case() {
    local os_name="$1"
    local cpu_name="$2"
    local ldd_version="$3"
    local expected_url="$4"

    MOCK_UNAME_S="$os_name"
    MOCK_UNAME_M="$cpu_name"
    MOCK_LDD_VERSION="$ldd_version"
    CAPTURED_URL=""

    download_binary_and_run_installer >/tmp/apollo-mcp-server-install-test.log 2>&1

    assert_eq "$CAPTURED_URL" "$expected_url" \
        "download URL for $os_name/$cpu_name with ldd '$ldd_version'"
}

uname() {
    case "$1" in
        -s)
            echo "$MOCK_UNAME_S"
            ;;
        -m)
            echo "$MOCK_UNAME_M"
            ;;
        *)
            command uname "$@"
            ;;
    esac
}

ldd() {
    if [ "$1" = "--version" ]; then
        echo "$MOCK_LDD_VERSION"
    else
        command ldd "$@"
    fi
}

downloader() {
    if [ "$1" = "--check" ]; then
        return 0
    fi

    CAPTURED_URL="$1"
    : > "$2"
}

tar() {
    return 0
}

mv() {
    return 0
}

run_architecture_case "Linux" "aarch64" "ldd (GNU libc) 2.34" "aarch64-unknown-linux-musl"
run_architecture_case "Linux" "aarch64" "ldd (GNU libc) 2.38" "aarch64-unknown-linux-gnu"
run_architecture_case "Linux" "x86_64" "ldd (GNU libc) 2.34" "x86_64-unknown-linux-musl"
run_architecture_case "Linux" "x86_64" "ldd (GNU libc) 2.38" "x86_64-unknown-linux-gnu"
run_download_case \
    "Linux" \
    "aarch64" \
    "ldd (GNU libc) 2.34" \
    "https://github.com/apollographql/apollo-mcp-server/releases/download/${PACKAGE_VERSION}/apollo-mcp-server-${PACKAGE_VERSION}-aarch64-unknown-linux-musl.tar.gz"

echo "install.sh target selection checks passed"
