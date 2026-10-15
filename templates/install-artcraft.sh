#!/bin/sh
# Install only the headless CLI and accompanying license documentation.
# Usage: sh install-artcraft.sh releases.txt /usr/local
set -eu
manifest=${1:?release manifest is required}
prefix=${2:-/usr/local}
case "$(uname -m)" in
    x86_64) ;;
    *) echo 'jbox: ArtCraft CLI releases require an x86_64 guest' >&2; exit 1 ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
mkdir -p "$prefix/bin" "$prefix/share/doc"
while read -r app version digest extra; do
    case "$app" in ''|'#'*) continue ;; esac
    case "$app" in
        photocraft|vectorcraft|filmcraft|lightcraft|pdfcraft|effectcraft|designcraft|figcraft|wordcraft|soundcraft|deckcraft|cadcraft|gridcraft) ;;
        *) echo "jbox: unknown ArtCraft app: $app" >&2; exit 1 ;;
    esac
    if [ -n "$extra" ] || ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || ! printf '%s\n' "$digest" | grep -Eq '^[0-9a-f]{64}$'; then
        echo "jbox: invalid ArtCraft release manifest entry: $app" >&2
        exit 1
    fi
    archive="$app-$version-linux-x86_64"
    echo "jbox: downloading $app-cli $version"
    curl --fail --location --show-error --silent --retry 3 --connect-timeout 30 --max-time 600 \
        "https://github.com/storytold/$app/releases/download/v$version/$archive.tar.gz" \
        -o "$work/release.tar.gz"
    printf '%s  %s\n' "$digest" "$work/release.tar.gz" | sha256sum --check --strict
    mkdir "$work/extract"
    tar --extract --gzip --file "$work/release.tar.gz" --directory "$work/extract" \
        --no-same-owner --no-same-permissions "$archive/bin/$app-cli" "$archive/share/doc/$app"
    # PdfCraft OCR resolves its bundled models relative to the CLI executable.
    if [ "$app" = pdfcraft ]; then
        tar --extract --gzip --file "$work/release.tar.gz" --directory "$work/extract" \
            --no-same-owner --no-same-permissions "$archive/share/pdfcraft"
        for model in text-detection.rten text-recognition.rten; do
            test -s "$work/extract/$archive/share/pdfcraft/models/$model"
        done
        cp -R "$work/extract/$archive/share/pdfcraft" "$prefix/share/"
    fi
    cli="$work/extract/$archive/bin/$app-cli"
    if [ ! -f "$cli" ] || [ -L "$cli" ]; then
        echo "jbox: missing regular ArtCraft CLI: $app" >&2
        exit 1
    fi
    install -m 0755 "$cli" "$prefix/bin/$app-cli"
    cp -R "$work/extract/$archive/share/doc/$app" "$prefix/share/doc/"
    # Fail the image build on missing libraries or a broken release, without a display.
    timeout 30 "$prefix/bin/$app-cli" --version
    case "$app" in
        # These tools print --help but exit 1. Check their headless registries.
        deckcraft|cadcraft) timeout 30 "$prefix/bin/$app-cli" commands >/dev/null ;;
        *) timeout 30 "$prefix/bin/$app-cli" --help >/dev/null ;;
    esac
    if [ "$app" = pdfcraft ]; then
        ocr_status=$(timeout 30 "$prefix/bin/pdfcraft-cli" run ocr_status)
        if ! printf '%s\n' "$ocr_status" | grep -Eq '"available"[[:space:]]*:[[:space:]]*true'; then
            echo 'jbox: PdfCraft OCR models are unavailable' >&2
            exit 1
        fi
    fi
    rm -rf "$work/extract" "$work/release.tar.gz"
done < "$manifest"
install -m 0644 "$manifest" "$prefix/share/doc/artcraft-releases.txt"
