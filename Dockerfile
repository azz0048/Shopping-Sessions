# DIAGNOSTIC ONLY (fails on purpose): times compile-time variants of the pipeline on the scorer.
FROM rust@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY pgo.profdata /pgo.profdata
ENV F="-C target-cpu=cascadelake -C relocation-model=static -C target-feature=+crt-static -C llvm-args=-x86-branches-within-32B-boundaries -C llvm-args=-x86-pad-max-prefix-size=5"
RUN mkdir /v && cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline && cp target/x86_64-unknown-linux-gnu/release/pipeline /v/base
RUN (RUSTFLAGS="$F -C profile-use=/pgo.profdata" cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline --target-dir /t/pgo && cp /t/pgo/x86_64-unknown-linux-gnu/release/pipeline /v/pgo) || cp /v/base /v/pgo_FAILED
RUN XL_BB=14 cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline --target-dir /t/bb14 && cp /t/bb14/x86_64-unknown-linux-gnu/release/pipeline /v/bb14
RUN XL_UB=13 cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline --target-dir /t/ub13 && cp /t/ub13/x86_64-unknown-linux-gnu/release/pipeline /v/ub13
RUN XL_CKPF=0 cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline --target-dir /t/ckpf0 && cp /t/ckpf0/x86_64-unknown-linux-gnu/release/pipeline /v/ckpf0
RUN XL_IOADV_DEF=0 cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline --target-dir /t/ioadv0 && cp /t/ioadv0/x86_64-unknown-linux-gnu/release/pipeline /v/ioadv0
RUN cp /v/base /pipeline

FROM rust:1-slim AS diag
WORKDIR /d
COPY diag/Cargo.toml diag/Cargo.lock ./
COPY diag/.cargo .cargo
COPY diag/vendor vendor
COPY diag/src src
RUN cargo build --release --offline --locked --target x86_64-unknown-linux-gnu \
 && cp target/x86_64-unknown-linux-gnu/release/ss /ss

FROM scratch
COPY --from=build /v /v
COPY --from=build /pipeline /pipeline
COPY --from=diag /ss /ss
USER 1000:1000
CMD ["/ss"]
