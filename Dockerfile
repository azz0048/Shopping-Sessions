# Shopping Sessions: native Rust pipeline, static binary in an empty image.
FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY vendor vendor
COPY src src
RUN cargo build --release --offline --locked --target x86_64-unknown-linux-gnu \
 && cp target/x86_64-unknown-linux-gnu/release/ss /ss

FROM scratch
COPY --from=build /ss /ss
USER 1000:1000
CMD ["/ss"]
