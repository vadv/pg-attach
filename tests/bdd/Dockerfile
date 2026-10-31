ARG PG_VERSION=17

FROM rust:1.99-bookworm AS build
RUN rustup component add clippy
WORKDIR /work
COPY tests/bdd/Cargo.toml tests/bdd/Cargo.lock ./
COPY tests/bdd/main.rs tests/bdd/postgres.rs tests/bdd/steps.rs tests/bdd/world.rs ./
RUN cargo clippy --release --all-targets --locked -- -D warnings -D clippy::pedantic \
    && cargo test --release --locked \
    && cargo build --release --locked

FROM pg-attach-pg${PG_VERSION}:dev
ARG PG_VERSION
ENV PATH="/usr/lib/postgresql/${PG_VERSION}/bin:${PATH}" \
    PG_ATTACH_FEATURES=/opt/pg-attach-bdd/features
COPY --from=build /work/target/release/pg-attach-bdd /usr/local/bin/pg-attach-bdd
COPY tests/bdd/features /opt/pg-attach-bdd/features
CMD ["pg-attach-bdd"]
