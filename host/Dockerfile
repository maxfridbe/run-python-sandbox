FROM debian:bookworm-slim

# Install system dependencies
# - podman: For running nested containers
# - iptables: For user-owner based egress blocking
# - gosu: Safe privilege dropping
# - python3: Python interpreter for the sandbox
# - uidmap: For newuidmap/newgidmap (nested rootless podman namespaces)
# - ca-certificates: Secure SSL connections if net access is enabled
RUN sed -i 's/[Cc]omponents: main/Components: main contrib non-free/g' /etc/apt/sources.list.d/debian.sources || \
    sed -i 's/main/main contrib non-free/g' /etc/apt/sources.list

RUN echo "ttf-mscorefonts-installer msttcorefonts/accepted-mscorefonts-eula select true" | debconf-set-selections

RUN apt-get update && apt-get install -y --no-install-recommends \
    podman \
    slirp4netns \
    iptables \
    gosu \
    python3 \
    uidmap \
    ca-certificates \
    time \
    python3-reportlab \
    python3-pil \
    python3-pypdf \
    poppler-utils \
    cabextract \
    ttf-mscorefonts-installer \
    fonts-liberation \
    fonts-croscore \
    fonts-crosextra-caladea \
    fonts-crosextra-carlito \
    fonts-dejavu \
    fonts-urw-base35 \
    && rm -rf /var/lib/apt/lists/*

# Pre-create the sandbox user (UID 10001)
RUN groupadd -g 10001 sandbox-user && \
    useradd -u 10001 -g sandbox-user -m -s /bin/bash sandbox-user

# Configure subordinate UID/GID mappings inside the container for rootless podman.
# These must map within the parent user namespace limits (typically 1-65536).
RUN echo "sandbox-user:20000:40000" > /etc/subuid && \
    echo "sandbox-user:20000:40000" > /etc/subgid

# Copy Podman configuration files to optimize rootless-in-rootless run using VFS storage
COPY storage.conf /etc/containers/storage.conf
COPY containers.conf /etc/containers/containers.conf

# Setup sandboxing folders
RUN mkdir -p /sandbox /output && \
    chown -R sandbox-user:sandbox-user /sandbox /output

# Copy the entrypoint script
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Run entrypoint.sh as root (inside container namespace) so we can configure iptables
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
