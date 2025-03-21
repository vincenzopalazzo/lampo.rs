FROM ubuntu:25.04
LABEL mantainer="Vincenzo Palazzo vincenzopalazzodev@gmail.com"

ENV TZ=Europe/Minsk
RUN ln -snf /usr/share/zoneinfo/$TZ /etc/localtime && echo $TZ > /etc/timezone

# Ubuntu utils
RUN apt-get update && apt-get install -y \
    software-properties-common \
    build-essential \
    curl wget git \
    python3 python3-pip libssl-dev \
    autoconf automake pkg-config \
    pipx \
    libtool \
    autoconf \
    automake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

## Install Rust
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
RUN echo 'source $HOME/.cargo/env' >> $HOME/.bashrc

WORKDIR /workdir
COPY . .

RUN chmod +x ./docker/install-bitcoind.sh
RUN ./docker/install-bitcoind.sh

RUN pipx ensurepath
RUN pipx completions
RUN echo "eval \"\$(register-python-argcomplete pipx)\"" >> ~/.bashrc
RUN pipx install poetry --global

CMD ["./docker/entrypoint.sh"]
