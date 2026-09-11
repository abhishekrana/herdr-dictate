# Builds from source on a bare system using only the documented dependencies.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
ENV PATH=/root/.cargo/bin:$PATH

# For rustup only. Neither the build nor the plugin needs these.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY scripts/install-deps.sh scripts/
RUN scripts/install-deps.sh && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal

# Dependencies first, against a stub crate: whisper.cpp is most of the build and
# does not change when this crate does.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && : > src/lib.rs \
    && cargo build --release --locked \
    && cargo build --release --locked --features vulkan \
    && rm -rf src

COPY . .

RUN cargo build --release --locked
RUN cargo build --release --locked --features vulkan
RUN cargo test --locked

# No microphone and no Herdr in a container; these need neither.
RUN ./target/release/herdr-dictate --version \
    && ./target/release/herdr-dictate setup --print
