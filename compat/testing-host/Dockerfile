# SSH host for remote-execution and server-validation tests.
#
# Hermetic: `docker` is a stand-in script, so validating this host never
# touches the Docker daemon of the machine running the tests. Each Linux user
# is one scenario; its PATH (from ~/.ssh/environment) decides which tools it
# sees. Scenario users other than deploy get a pass-through `sudo` that keeps
# that PATH:
#
#   root       everything; validation succeeds
#   deploy     non-root with real passwordless sudo; validation succeeds
#   nojq       jq missing                -> prerequisites error
#   nodocker   docker missing            -> Docker Engine is not installed
#   olddocker  Docker Engine 20.10.7     -> version too old
#   badjson    docker prints a banner    -> version unreadable
#   gentoo     /etc/os-release says gentoo -> OS not supported
FROM debian:12-slim

ARG TEST_PUBLIC_KEY
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server sudo git curl jq ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /opt/tools /run/sshd \
 && mv /usr/bin/jq /opt/tools/jq

COPY fake-docker /opt/tools/docker
COPY fake-cat /opt/tools/cat-gentoo
COPY passthrough-sudo /opt/tools/sudo-passthrough

RUN set -eu; chmod 755 /opt/tools/*; \
    mk() { d="/opt/$1/bin"; mkdir -p "$d"; shift; for t in "$@"; do ln -s "/opt/tools/$t" "$d/${t%%-*}"; done; }; \
    mk ok jq docker; \
    mk nojq docker sudo-passthrough; \
    mk nodocker jq sudo-passthrough; \
    mk gentoo jq docker cat-gentoo sudo-passthrough; \
    mkdir -p /opt/olddocker/bin /opt/badjson/bin; \
    for d in olddocker badjson; do ln -s /opt/tools/jq /opt/$d/bin/jq; ln -s /opt/tools/sudo-passthrough /opt/$d/bin/sudo; done; \
    printf '#!/bin/sh\nFAKE_DOCKER_VERSION=20.10.7 exec /opt/tools/docker "$@"\n' > /opt/olddocker/bin/docker; \
    printf '#!/bin/sh\nFAKE_DOCKER_BANNER=1 exec /opt/tools/docker "$@"\n' > /opt/badjson/bin/docker; \
    chmod 755 /opt/olddocker/bin/docker /opt/badjson/bin/docker; \
    for u in root deploy nojq nodocker olddocker badjson gentoo; do \
      [ "$u" = root ] || useradd -m -s /bin/bash "$u"; \
      home=$(getent passwd "$u" | cut -d: -f6); \
      case "$u" in root|deploy) p=ok;; *) p=$u;; esac; \
      mkdir -p "$home/.ssh"; \
      echo "$TEST_PUBLIC_KEY" > "$home/.ssh/authorized_keys"; \
      echo "PATH=/opt/$p/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" > "$home/.ssh/environment"; \
      chown -R "$u:$u" "$home/.ssh"; chmod 700 "$home/.ssh"; chmod 600 "$home/.ssh/"*; \
    done; \
    echo 'deploy ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/deploy; \
    echo 'Defaults:deploy secure_path="/opt/ok/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"' >> /etc/sudoers.d/deploy; \
    chmod 440 /etc/sudoers.d/deploy; \
    ssh-keygen -A; \
    printf 'PermitUserEnvironment yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPermitRootLogin prohibit-password\n' > /etc/ssh/sshd_config.d/test.conf

EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
