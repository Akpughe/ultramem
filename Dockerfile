FROM rust:1-slim AS build
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p ultramem-server

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/ultramem-server /usr/local/bin/ultramem-server
EXPOSE 8080
CMD ["ultramem-server"]
