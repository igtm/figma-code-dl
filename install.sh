#!/bin/sh

set -eu

owner="igtm"
repo="figma-code-dl"
exe_name="figma-code-dl"
github_url="https://github.com"
api_url="https://api.github.com"
version=""
executable_folder="/usr/local/bin"

get_arch() {
    arch="$(uname -m)"
    case "${arch}" in
        x86_64|amd64)
            echo "x86_64"
            ;;
        aarch64|arm64)
            echo "aarch64"
            ;;
        *)
            echo ""
            ;;
    esac
}

get_os() {
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    case "${os}" in
        darwin)
            echo "apple-darwin"
            ;;
        linux)
            echo "unknown-linux-gnu"
            ;;
        *)
            echo ""
            ;;
    esac
}

for arg in "$@"; do
    case "${arg}" in
        -b=*)
            executable_folder="${arg#*=}"
            ;;
        -v=*)
            version="${arg#*=}"
            ;;
        *)
            ;;
    esac
done

arch="$(get_arch)"
os="$(get_os)"

if [ -z "${arch}" ] || [ -z "${os}" ]; then
    echo "ERROR: Unsupported platform $(uname -s)/$(uname -m)" >&2
    exit 1
fi

if [ -z "${version}" ]; then
    version="$(
        command curl -fsSL "${api_url}/repos/${owner}/${repo}/releases/latest" |
        command grep '"tag_name":' |
        command sed -E 's/.*"([^"]+)".*/\1/' |
        command head -n 1
    )"
    if [ -z "${version}" ]; then
        echo "ERROR: Failed to resolve latest release version" >&2
        exit 1
    fi
fi

case "${version}" in
    v*)
        ;;
    *)
        version="v${version}"
        ;;
esac

target="${arch}-${os}"
download_dir="$(mktemp -d)"
trap 'rm -rf "${download_dir}"' EXIT HUP INT TERM
archive_name="${exe_name}_${version}_${target}.tar.gz"
archive_path="${download_dir}/${archive_name}"
asset_uri="${github_url}/${owner}/${repo}/releases/download/${version}/${archive_name}"

mkdir -p "${executable_folder}"

echo "[1/3] Download ${asset_uri}"
command curl --fail --location --output "${archive_path}" "${asset_uri}"

echo "[2/3] Install ${exe_name} to ${executable_folder}"
command tar -xzf "${archive_path}" -C "${executable_folder}"
command chmod +x "${executable_folder}/${exe_name}"

echo "[3/3] Done"
echo "${exe_name} was installed successfully to ${executable_folder}/${exe_name}"
