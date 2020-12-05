# Use the official Rust image.
# https://hub.docker.com/_/rust
FROM rust:1.48.0

# Copy local code to the container image.
WORKDIR /usr/src/app
COPY . .

# Install production dependencies and build a release artifact.
RUN cargo build --release
RUN mv target/release/gmail-watch /usr/local/bin/gmail-watch

# Service must listen to $PORT environment variable.
# This default value facilitates local development.
ENV PORT 8080

# Run the web service on container startup.
CMD ["gmail-watch"]