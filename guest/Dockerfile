# The golden image every case boots from. Built once on the host (trusted network), then
# mounted read-only in each microVM. Everything a case needs is already here.
FROM node:22-bookworm

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      socat iproute2 sudo strace ltrace jq file binutils procps less vim-tiny unzip xz-utils \
      python3-pip python3-venv ca-certificates git curl tree ripgrep \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code && npm cache clean --force

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
