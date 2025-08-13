FROM ubuntu:24.10
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
RUN pipx completions
RUN echo "eval \"\$(register-python-argcomplete pipx)\"" >> ~/.bashrc
RUN pipx install poetry --global

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

CMD ["./docker/entrypoint.sh"]