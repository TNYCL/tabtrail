# Disposable test image for Tabtrail's shell and SSH integration tests.
# Build:  docker build -t tabtrail-test:ubuntu-24.04 --build-arg BASE=ubuntu:24.04 tests/docker
# Nothing from the host is modified; tests run against a mounted checkout.
ARG BASE=ubuntu:24.04
FROM ${BASE}
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash bash-completion zsh fish python3 git ca-certificates \
      openssh-server openssh-client time \
 && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash tester
WORKDIR /home/tester
