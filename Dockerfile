FROM dart:3.7.2

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    git \
    wget \
    build-essential \
    pkg-config \
    libssl-dev \
    strace \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN sh -c "$(wget -O- https://github.com/deluan/zsh-in-docker/releases/download/v1.2.1/zsh-in-docker.sh)" -- \
    -p git \
    -p https://github.com/zsh-users/zsh-autosuggestions \
    -p https://github.com/zsh-users/zsh-completions \
    -p https://github.com/zsh-users/zsh-history-substring-search \
    -p https://github.com/zsh-users/zsh-syntax-highlighting \
    -p https://github.com/unixorn/fzf-zsh-plugin.git

# ---- Install Rust & Cargo via rustup (stable toolchain) ----
RUN curl https://sh.rustup.rs -sSf | \
    sh -s -- -y --profile minimal                      \
    && echo 'source $HOME/.cargo/env' >> /etc/profile.d/rust.sh
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app

ENTRYPOINT [ "/bin/zsh" ]
CMD ["-l"]