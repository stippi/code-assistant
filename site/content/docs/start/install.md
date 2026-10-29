---
title: Install
weight: 10
lede: Download a prebuilt build, or build it from source with the Rust toolchain.
frontends: [app, tui, acp]
---

## Download a build

Prebuilt builds are on the [Releases page](https://github.com/stippi/code-assistant/releases/latest):

| Platform | File |
|---|---|
| macOS, Apple Silicon | `Code-Assistant-macos-aarch64.app.zip` |
| macOS, Intel | `Code-Assistant-macos-x86_64.app.zip` |
| Linux | `code-assistant-linux-x86_64.zip` |
| Windows | `code-assistant-windows-x86_64.zip` |

On macOS, unzip it and move **Code Assistant.app** to your Applications
folder. The same file is used for every frontend. To use the terminal or ACP
mode, call the binary inside the bundle:

```bash
/Applications/Code\ Assistant.app/Contents/MacOS/code-assistant --tui
```

{{< callout tip "Put it on your PATH" >}}
A symlink saves typing: `ln -s "/Applications/Code Assistant.app/Contents/MacOS/code-assistant" /usr/local/bin/code-assistant`.
The rest of these guides call it `code-assistant`.
{{< /callout >}}

## Build from source

```bash
# Rust toolchain (macOS and Linux)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Linux (Debian/Ubuntu): the system libraries the GUI needs
sudo apt-get install -y --no-install-recommends \
    pkg-config build-essential libssl-dev libzstd-dev \
    libfontconfig-dev libwayland-dev libx11-xcb-dev \
    libxkbcommon-x11-dev libasound2-dev libvulkan1

# macOS: the Metal toolchain
xcodebuild -downloadComponent MetalToolchain

git clone https://github.com/stippi/code-assistant
cd code-assistant
cargo build --release
```

The binary is at `target/release/code-assistant`.

### A macOS app bundle

To get a `.app` with a dock icon from your own build, use the bundle script.
It needs only stock macOS tools:

```bash
cargo build --locked --release --target aarch64-apple-darwin   # or x86_64-apple-darwin
./scripts/bundle-macos.sh aarch64                              # or x86_64, universal; --no-build reuses a binary
```

The result is `target/macos-bundle/Code Assistant.app`, plus a zipped copy.

## Next

[Connect a model →](/docs/start/first-run/)
