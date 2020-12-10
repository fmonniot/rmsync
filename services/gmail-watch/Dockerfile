# Use an image with prebuilt support for openssl and cross compilation
# https://github.com/emk/rust-musl-builder
FROM ekidd/rust-musl-builder:latest as builder

# Copy local code to the container image.
COPY . .

# Install production dependencies and build a release artifact.
RUN cargo build --release

RUN ls /home/rust/src/target/x86_64-unknown-linux-musl/release/

FROM alpine

# Take the binary and put it in our image
COPY --from=builder /home/rust/src/target/x86_64-unknown-linux-musl/release/gmail-watch /usr/local/bin/

# Service must listen to $PORT environment variable.
# This default value facilitates local development.
ENV PORT 8080

# Run the web service on container startup.
CMD ["gmail-watch"]
