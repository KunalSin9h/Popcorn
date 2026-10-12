# The golden image every case boots from. Built once on the host (trusted network), then
# mounted read-only in each microVM. Everything a case needs is already here.
FROM node:22-bookworm

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      socat iproute2 sudo procps less vim nano tree ripgrep jq ca-certificates git curl wget \
      python3-pip python3-venv pipx sqlite3 openssl htop build-essential \
      `# network: most of these only reach localhost, a corn has no network card` \
      iputils-ping dnsutils netcat-openbsd net-tools lsof tcpdump \
      `# tracing and reverse engineering` \
      strace ltrace gdb file binutils elfutils patchelf xxd bsdextrautils yara \
      `# archives and metadata` \
      unzip xz-utils p7zip-full cabextract libimage-exiftool-perl \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code && npm cache clean --force

# uv: fast Python package manager, a single static binary.
RUN curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin INSTALLER_NO_MODIFY_PATH=1 sh

# The analyst account. Passwordless sudo: the VM is disposable and has nothing of yours in it.
RUN userdel -r node 2>/dev/null || true \
    && useradd -m -u 1000 -s /bin/bash analyst \
    && echo 'analyst ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/analyst \
    && mkdir -p /home/analyst/inbox /home/analyst/outbox /home/analyst/work && chown -R analyst:analyst /home/analyst

COPY init /sbin/popcorn-init
COPY stage2 /sbin/popcorn-stage2
COPY profile.sh /etc/profile.d/popcorn.sh
COPY channels.sh /usr/local/bin/popcorn-channels
COPY claude.json /home/analyst/.claude.json
RUN chmod 755 /sbin/popcorn-init /sbin/popcorn-stage2 /usr/local/bin/popcorn-channels \
    && for c in inbox outbox share-in share-out; do ln -s popcorn-channels /usr/local/bin/popcorn-$c; done \
    && chown analyst:analyst /home/analyst/.claude.json \
    && echo popcorn > /etc/hostname
