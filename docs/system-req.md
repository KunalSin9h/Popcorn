# System requirements

Popcorn is built with [GPUI](https://gpui.rs/), Zed's GPU-accelerated Rust UI framework.

## Rust

Latest stable, via rustup:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

## Linux (Debian/Ubuntu)

Wayland or X11, plus Vulkan.

```sh
sudo apt install -y libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libvulkan-dev libfontconfig1-dev libfreetype-dev clang pkg-config \
  build-essential libx11-dev libxcb1-dev libssl-dev cmake libzstd-dev
```

## macOS

Xcode and the command line tools. Metal is used for rendering.

## Windows

No extra packages. Uses Win32 and DirectWrite.

## Sandbox

- **KVM**: `/dev/kvm` must be readable and writable by your user (`ls -l /dev/kvm`).
- **Docker**: only to build the workbench image once (`scripts/build-image.sh`).
- **Firecracker** and a guest kernel live in `~/.local/share/popcorn/` (`bin/firecracker`, `vmlinux`).
- **A terminal app** such as Ptyxis, GNOME Terminal or Konsole, for "Open terminal".
