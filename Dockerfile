# DIAGNOSTIC ONLY (fails on purpose): times /pipeline under setting variants on the scorer.
FROM rust@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
RUN cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline \
 && cp target/x86_64-unknown-linux-gnu/release/pipeline /pipeline

FROM rust:1-slim AS diag
WORKDIR /d
COPY diag/Cargo.toml diag/Cargo.lock ./
COPY diag/.cargo .cargo
COPY diag/vendor vendor
COPY diag/src src
RUN cargo build --release --offline --locked --target x86_64-unknown-linux-gnu \
 && cp target/x86_64-unknown-linux-gnu/release/ss /ss

FROM scratch
COPY --from=build /pipeline /pipeline
COPY --from=diag /ss /ss
USER 1000:1000
CMD ["/ss"]
