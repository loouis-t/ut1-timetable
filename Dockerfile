# Builder stage
FROM rust:latest as builder
WORKDIR /app

# Copy over your manifest
ADD . .
# Cache dependencies
RUN cargo build --release

# Runtime stage
FROM debian:12-slim
WORKDIR /app

# Install Chromium
RUN apt-get update && apt-get install -y chromium openssh-client

# Copy over the built application from the builder stage
COPY --from=builder /app/target/release/ut1-timetable /app/ut1-timetable
COPY --from=builder /app/.env /app/.env
COPY ./id_rsa /app/id_rsa

# Run the binary
CMD ["/app/ut1-timetable"]