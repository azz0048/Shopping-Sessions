FROM rust@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY pgo.profdata /pgo.profdata
# Fast binary: AVX-512 (cascadelake) flags from .cargo/config.toml, plus profile-guided
# optimization from recorded runs. Falls back to the plain build if the profile is not accepted.
RUN (RUSTFLAGS="-C target-cpu=cascadelake -C relocation-model=static -C target-feature=+crt-static -C llvm-args=-x86-branches-within-32B-boundaries -C llvm-args=-x86-pad-max-prefix-size=5 -C profile-use=/pgo.profdata" \
     cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline --target-dir /t/pgo \
     && cp /t/pgo/x86_64-unknown-linux-gnu/release/pipeline /pipeline) \
 || (cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline \
     && cp target/x86_64-unknown-linux-gnu/release/pipeline /pipeline)
# Safety net: generic pipeline for baseline x86-64, used only if the CPU
# lacks AVX-512 (RUSTFLAGS overrides the config's build.rustflags).
RUN RUSTFLAGS="-C target-cpu=x86-64-v2 -C target-feature=+crt-static" \
    cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline-portable --target-dir /tmp/tport \
 && cp /tmp/tport/x86_64-unknown-linux-gnu/release/pipeline-portable /pipeline-portable

FROM scratch
COPY --from=build /pipeline /pipeline
COPY --from=build /pipeline-portable /pipeline-portable
USER 1000:1000
CMD ["/pipeline"]
