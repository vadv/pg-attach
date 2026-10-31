ARG PG_VERSION=17

FROM rust:1.99-bookworm AS build
ARG PG_VERSION

RUN rustup component add rustfmt clippy
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gnupg clang libclang-dev pkg-config \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
       | gpg --dearmor -o /usr/share/keyrings/postgresql.gpg \
    && echo 'deb [signed-by=/usr/share/keyrings/postgresql.gpg] http://apt.postgresql.org/pub/repos/apt bookworm-pgdg main' \
       > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-${PG_VERSION} postgresql-server-dev-${PG_VERSION} libpq-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-pgrx --version 0.19.3 --locked
RUN cargo pgrx init --pg${PG_VERSION} /usr/lib/postgresql/${PG_VERSION}/bin/pg_config

WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs pg_attach.control ./
COPY src ./src
RUN cargo fmt --all -- --check \
    && cargo test --locked --no-default-features --features "pg${PG_VERSION}" \
    && cargo clippy --locked --all-targets --no-default-features --features "pg${PG_VERSION}" -- -D warnings -D clippy::pedantic
RUN cargo pgrx install --release --no-default-features --features "pg${PG_VERSION}" \
    --pg-config /usr/lib/postgresql/${PG_VERSION}/bin/pg_config

FROM postgres:${PG_VERSION}-bookworm
ARG PG_VERSION
RUN apt-get update \
    && apt-get install -y --no-install-recommends gdb strace procps lsof binutils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/lib/postgresql/${PG_VERSION}/lib/pg_attach.so /usr/lib/postgresql/${PG_VERSION}/lib/pg_attach.so
COPY --from=build /usr/share/postgresql/${PG_VERSION}/extension/pg_attach* /usr/share/postgresql/${PG_VERSION}/extension/
