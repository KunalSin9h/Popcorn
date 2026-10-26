# Developing Popcorn

## Stack

| Part | Choice |
|---|---|
| App | Rust 2024, [GPUI](https://gpui.rs/) 0.2.2 (Zed's GPU UI framework) |
| Isolation | [Firecracker](https://github.com/firecracker-microvm/firecracker) microVMs on KVM |
| Guest | Debian bookworm (from `node:22-bookworm`), built with Docker into an ext4 image |
| TLS to Anthropic | `rustls` with the `ring` provider, `webpki-roots` |
| Fonts | IBM Plex Sans and Plex Mono, embedded |
| Icons | Lucide (ISC), embedded |

The app has four other crates as dependencies: `gpui`, `include_dir`, `rustls` and `webpki-roots`.

## Layout

```
src/
  main.rs      UI: title bar, setup, corn view, dialogs, window frame
  sandbox.rs   Corns: create, start, stop, delete, terminal attach, file copy, export
  gate.rs      Network gate, Claude proxy, safe tar reader, activity log
  share.rs     Shared folder: git bundles in and out, review, apply approved files
  theme.rs     Light and dark palettes, appearance setting
  icons.rs     Embedded assets
guest/
  Dockerfile   The workbench image
  init         PID 1, stage one: overlay the corn disk on the read-only image
  stage2       PID 1, stage two: account, vsock bridges, channels
  channels.sh  Inbox, outbox and shared-folder channels
  profile.sh   Proxy settings and the placeholder Claude credential
scripts/
  fetch-firecracker.sh   Firecracker, jailer and guest kernel
  build-image.sh         Build golden.ext4 from guest/
assets/        Fonts, icons, desktop entry
vendor/xattr   Patched xattr 0.2 (libc dropped ENOATTR)
```

## System packages

### Debian and Ubuntu

```sh
sudo apt install -y build-essential clang pkg-config cmake \
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libvulkan-dev \
  libfontconfig1-dev libfreetype-dev libx11-dev libxcb1-dev libssl-dev libzstd-dev \
  socat e2fsprogs bubblewrap git docker.io
```

Install Rust with rustup:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

### Machine

- **KVM.** Your user must read and write `/dev/kvm`. Check with `ls -l /dev/kvm`.
- **Docker.** Popcorn needs Docker only to build the image.
- **A terminal app.** Popcorn tries Ptyxis, GNOME Terminal, Console, Konsole, Alacritty, kitty, `x-terminal-emulator`, then xterm.

Popcorn runs on Linux only. The UI also builds on macOS and Windows, but corns need KVM.

## Build and run

```sh
./scripts/fetch-firecracker.sh   # one time
./scripts/build-image.sh         # one time, and after each change in guest/
cargo build --release
./target/release/popcorn
./install.sh                     # optional: install to ~/.local/bin with a desktop entry
```

Rebuild the image after you change a file in `guest/`. Running corns keep the old image until
you stop and start them.

## Releases

A release has four files: an AppImage, a `.deb`, the compressed workbench image and `SHA256SUMS`.

| Part | Where it comes from |
|---|---|
| App version | `version` in `Cargo.toml` |
| Workbench image | `golden-<version>.ext4.zst`, built by `scripts/release-image.sh` |
| Image hash | Compiled into the app from `POPCORN_IMAGE_SHA256` |
| Firecracker, jailer, kernel | Inside both packages, in `/usr/lib/popcorn` |

To make a release, change the version in `Cargo.toml`, commit, then push a tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The `release` workflow builds the image, tests, packages and publishes. To build the same files
locally:

```sh
./scripts/fetch-firecracker.sh
./scripts/release-image.sh      # dist/golden-<version>.ext4.zst and its .sha256
./scripts/package.sh all        # dist/Popcorn-<version>-x86_64.AppImage, dist/popcorn_<version>_amd64.deb
```

How images work in a release:

- On first run, the app downloads the image for its own version from the GitHub release.
- The app checks the SHA-256 of the download against the hash compiled into it. It unpacks
  nothing before the check passes.
- Each version has its own file, `images/golden-<version>.ext4`. A corn records its image. An
  update does not change the image under an existing corn.
- A development build (no `POPCORN_IMAGE_SHA256`) builds the image locally with Docker.
- `POPCORN_IMAGE_URL` replaces the download address, for example for a mirror.

The download needs a public release. For a private repository, set `POPCORN_IMAGE_URL` to an
address that does not need a login.

## Files on disk

| Path | Contents |
|---|---|
| `~/.local/share/popcorn/bin/` | `firecracker`, `jailer` |
| `~/.local/share/popcorn/vmlinux` | Guest kernel |
| `~/.local/share/popcorn/images/golden-<version>.ext4` | Read-only workbench image of a release |
| `~/.local/share/popcorn/golden.ext4` | Read-only workbench image of a development build |
| `~/.local/share/popcorn/allow` | Hosts you always allow, one for each line. All corns use this list. |
| `~/.local/share/popcorn/corns/<corn>/` | `disk.ext4`, `image`, `vm.json`, `console.log`, `activity.log`, `mode`, `title`, `inbox.sent`, `share`, `terminal.lock`, sockets. Older versions used `cases/`; Popcorn moves it. |
| `~/.config/popcorn/` | `user`, `claude-key` (mode 0600), `appearance`, `claude-skipped` |

## Vsock channels

The corn has no network card. All traffic uses vsock. The host CID is 2.

| Port | Direction | Use |
|---|---|---|
| 1080 | corn to host | HTTP CONNECT proxy through the network gate. Public addresses only; 64 connections at a time. |
| 1081 | corn to host | Claude API. The host adds the token and opens TLS. |
| 1024 | host to corn | Login shell for the terminal |
| 1025 | host to corn | Files in, as a tar stream, to `~/inbox` |
| 1026 | host to corn | Outbox export, as a tar stream |
| 1029 | host to corn | Shared folder in, as a git bundle, to `~/work/<name>` |
| 1030 | host to corn | Shared folder back, as a git bundle of new objects, for review |

## Environment variables

| Variable | Effect |
|---|---|
| `POPCORN_HOME` | Use another data folder. Keep the path short: Unix socket paths have a limit of 108 bytes. |
| `POPCORN_CLAUDE_KEY` | Use this token instead of the saved one. An empty value means no token. |
| `POPCORN_USER` | Use this account name in corns instead of the saved one. |

## Tests

```sh
cargo test --release                        # unit tests, fast
```

The VM tests boot real corns. They need KVM, the image and network access. Run them one at a
time, in a short temporary home:

```sh
H=$XDG_RUNTIME_DIR/pcdemo; mkdir -p $H
for f in golden.ext4 vmlinux bin; do ln -sfn ~/.local/share/popcorn/$f $H/$f; done
POPCORN_HOME=$H cargo test --release -- --include-ignored --test-threads=1
rm -rf $H
```

| Test | Checks |
|---|---|
| `case_boots_isolated_and_gated` | No network card. Denied host refused. Internal addresses refused, literal or resolved. Allowed host works. Claude proxy swaps the token, works while sealed, and returns 401 with no token. Token not in the guest. Outbox export drops links. Shared folder: secrets stay home, changes detected, only approved files written. Runs Firecracker inside bubblewrap. |
| `attach_is_responsive` | Terminal echo under 150 ms, `TERM` set, prompt shows the user name. |
| `internal_addresses_are_not_public` | Loopback, private, link-local, metadata, multicast and mapped IPv6 addresses are refused; public ones pass. |
| `import_keeps_only_plain_files_inside_quarantine` | Tar reader drops links, devices and unsafe paths. |
| `shares_through_git_and_trusts_nothing_back` | Ignored files stay home. Other version control folders stay home, and a folder with `.hgignore` rules is refused until they are in `.gitignore`. The corn's links are never offered. Your own edits are flagged. New files are never executable. Applied changes are not offered again. A forged `.git` path and garbage bundles are refused. |
| `writes_stay_inside_the_folder` | Approved writes never leave the folder or go through a link. |
| `refuses_home_and_secret_folders` | Home, `/`, folders above home and folders with keys are refused. |
| `terminal_drops_clipboard_escapes` | Clipboard escapes from the corn never reach your terminal, in any form or split; other output passes. |
| `names_become_safe_account_names` | Typed names become safe Linux account names. |

## Debugging

- **A corn does not boot.** Read `corns/<corn>/console.log`. It has the guest kernel and init output.
- **A connection fails.** Read `corns/<corn>/activity.log`. Each line has a time, a verdict and a target.
- **The terminal does not connect.** Make sure the corn runs, then run `popcorn attach <corn folder>` in a terminal.
- **Screenshots on GNOME Wayland.** GNOME blocks screen capture. Run Popcorn under XWayland with
  `env -u WAYLAND_DISPLAY`, then capture the window with `ffmpeg -f x11grab -window_id <id>`.

## Conventions

- **Text in the app.** Use simple English close to ASD-STE100: short sentences, active voice, one
  instruction for each sentence. Use hints, not explanations.
- **Shortcuts in code.** Mark a known limit with a `ponytail:` comment. Say the limit and the fix.
- **Security code.** Most guarantees in the README have a test. Add a test when you add or change a guarantee.
