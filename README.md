<p align="center">
  <img src="assets/popcorn.png" width="120" alt="Popcorn">
</p>

<h1 align="center">Popcorn</h1>

<p align="center">
  A workbench for risky work.<br>
  Each job runs in a <b>corn</b>: a small virtual machine that cannot touch your computer.
</p>

<p align="center">
  <a href="https://github.com/KunalSin9h/popcorn/releases/latest"><img src="https://img.shields.io/badge/Download-for%20Linux-F0BE2C?style=for-the-badge&labelColor=B23A2E&logo=linux&logoColor=white" alt="Download for Linux"></a>
</p>

---

## What it does

Popcorn is a desktop app for Linux. Use it for untrusted code, suspicious packages and unknown
binaries. You and Claude Code do the work in a corn. Your files, keys and network stay outside.

🌽 A corn is a [Firecracker](https://firecracker-microvm.github.io/) microVM with its own Linux
kernel. It starts in about one second.

<p align="center">
  <img src="assets/screenshots/ask.png" width="860" alt="A corn asks before it connects to github.com">
</p>

<img src="assets/screenshots/review.png" width="340" align="right" alt="Review each changed file before it is written back">

- **Terminal.** Open a shell in a corn.
- **Network.** Each new host waits for your approval: deny, allow once or always allow.
- **Files in.** Drop files on the window, or select them. They go to `~/inbox`.
- **Files out.** Export files from `~/outbox`. Popcorn saves them as copies that cannot run.
- **Shared folder.** The corn gets a copy of a folder. Popcorn writes back only the changes that you approve.

<br clear="right">

## How it keeps you safe

| Guarantee | How |
|---|---|
| A corn has its own kernel | Each corn is a separate virtual machine, not a container. A kernel exploit stays inside the corn. |
| A corn has no network card | Its only link is a private channel to Popcorn. It has no route to your network or the internet. |
| You approve each new host | Popcorn holds each new connection and asks you. If you do not answer in 90 seconds, Popcorn refuses it. |
| A corn cannot reach your network | Popcorn refuses addresses on your computer or local network (loopback, private, link-local), even for a host you allowed, and connects only to the addresses it checked. |
| Your Claude token stays outside | Claude Code in the corn has only a fake token. Popcorn adds your real token to requests to `api.anthropic.com`. |
| A corn cannot see your files | No folder from your computer is connected to a corn. A corn has only its own Linux system and its own disk. |
| Files come out only when you export them | A corn cannot send files to your computer. Exported files are plain copies. |
| Your folder changes only with your approval | You review each changed file. Popcorn writes only the files that you approve, and only inside that folder. |
| Your keys stay outside | Popcorn does not share your home folder, or a folder with `.ssh`, `.gnupg`, `.aws` or `.config`. It also leaves out `.git` and ignored files. |

### Limits

- A hypervisor escape is rare, but possible. Firecracker runs as your user, without its jailer.
- If you allow a host, the corn can send data to that host.
- Inside a corn, your account has root. The corn is the sandbox, not the account.
- Keep your kernel and Firecracker up to date.

## Architecture

```mermaid
flowchart LR
  subgraph host["Your computer"]
    app["Popcorn app"]
    gate["Network gate<br/>ask, allow, refuse<br/>public addresses only"]
    proxy["Claude proxy<br/>adds your token"]
    token[("Claude token")]
    files["Files<br/>inbox, outbox, review"]
  end

  subgraph corn["Corn: Firecracker microVM"]
    shell["Shell and Claude Code"]
    disk[("Corn disk<br/>over a read-only image")]
  end

  shell -- "vsock" --> gate
  shell -- "vsock" --> proxy
  gate -- "approved hosts" --> internet(("Internet"))
  proxy --> anthropic(("api.anthropic.com"))
  token -.-> proxy
  app <-- "vsock: terminal" --> shell
  files <-- "vsock: tar" --> shell
  shell --- disk
```

A new connection waits for you:

```mermaid
sequenceDiagram
  participant C as Code in corn
  participant G as Network gate
  participant Y as You
  participant H as Remote host
  C->>G: CONNECT example.com:443
  alt An address on your network, such as 127.0.0.1
    G-->>C: 403 Forbidden, without asking
  else A name
    G->>Y: Show the yellow band
    alt Deny, or no answer in 90 s
      G-->>C: 403 Forbidden
    else Allow once or always
      G->>G: Resolve the name. Keep public addresses only.
      alt No public address
        G-->>C: 403 Forbidden
      else
        G->>H: Connect to a checked address
        G-->>C: 200 Connection established
      end
    end
  end
```

## Quick start

Popcorn runs on Linux with KVM. Read [docs/DEV.md](docs/DEV.md) for system packages.

```sh
./scripts/fetch-firecracker.sh   # Firecracker and a guest kernel, one time
./scripts/build-image.sh         # the workbench image, with Docker, one time
cargo run --release
```

Popcorn then asks for three items: your name, the image, and a Claude token (optional). Get a token with
`claude setup-token`.

## Documentation

- [docs/DEV.md](docs/DEV.md): stack, build, tests and workflow.
