FROM ubuntu:24.04
LABEL mantainer="Vincenzo Palazzo vincenzopalazzodev@gmail.com"

ENV BITCOIN_VERSION=25.1
ENV DEBIAN_FRONTEND=noninteractive

WORKDIR /work
COPY . .

RUN apt-get -qq update && \
    apt-get -qq install --no-install-recommends --allow-unauthenticated -yy \
    autoconf \
    automake \
    clang \
    cppcheck \
    docbook-xml \
    pkg-config \
    shellcheck \
    eatmydata \
    software-properties-common \
    build-essential \
    autoconf \
    locales \
    libtool \
    libssl-dev \
    libprotobuf-c-dev \
    libsqlite3-dev \
    libgmp-dev \
    python3-distutils-extra \
    git \
    python3 \
    valgrind \
    net-tools \
    python3-mako \
    python3-pip \
    python3-setuptools \
    python3-dev \
    build-essential \
    libffi-dev \
    virtualenv \
    shellcheck \
    libxml2-utils \
    wget \
    gettext \
    curl \
    xsltproc \
    pipx \
    jq \
    protobuf-compiler \
    zlib1g-dev && \
    rm -rf /var/lib/apt/lists/*

ENV LANGUAGE=en_US.UTF-8
ENV LANG=en_US.UTF-8
ENV LC_ALL=en_US.UTF-8
RUN locale-gen en_US.UTF-8 && dpkg-reconfigure locales

RUN cd /tmp/ && \
    wget https://bitcoincore.org/bin/bitcoin-core-$BITCOIN_VERSION/bitcoin-$BITCOIN_VERSION-x86_64-linux-gnu.tar.gz -O bitcoin.tar.gz && \
    tar -xvzf bitcoin.tar.gz && \
    mv /tmp/bitcoin-$BITCOIN_VERSION/bin/bitcoin* /usr/local/bin/ && \
    rm -rf bitcoin.tar.gz /tmp/bitcoin-$BITCOIN_VERSION

RUN pipx ensurepath
ENV PATH="/root/.local/bin:${PATH}"
RUN pipx completions
RUN echo "eval \"\$(register-python-argcomplete pipx)\"" >> ~/.bashrc
RUN pipx install poetry

RUN git config --global user.name "John Doe" && \
	git config --global user.email johndoe@example.com && \
	git clone https://github.com/ElementsProject/lightning.git && \
    cd lightning && git checkout v25.02.2 && \
    poetry install --no-root && \
    poetry run ./configure --disable-rust && \
    poetry run make -j$(nproc) install

# Install Rust
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
RUN echo 'source $HOME/.cargo/env' >> $HOME/.bashrc

# VLS signer test binaries (vlsd + remote_hsmd_socket), pinned to v1.0.0-rc.1.
# `make check-vls` runs the lampo_vls_tests against these; the tests fail if
# VLSD_EXE/REMOTE_HSMD_SOCKET_EXE are unset.
ENV VLS_VERSION=v1.0.0-rc.1
RUN git clone --depth 1 --branch $VLS_VERSION https://gitlab.com/lightning-signer/validating-lightning-signer.git /tmp/vls && \
    cd /tmp/vls && cargo build --release --bin vlsd --bin remote_hsmd_socket && \
    mv /tmp/vls/target/release/vlsd /tmp/vls/target/release/remote_hsmd_socket /usr/local/bin/ && \
    rm -rf /tmp/vls
ENV VLSD_EXE=/usr/local/bin/vlsd
ENV REMOTE_HSMD_SOCKET_EXE=/usr/local/bin/remote_hsmd_socket

CMD ["./docker/entrypoint.sh"]