default:
    just --list

# Build everything
build:
    cargo build

run-backend:
    cargo run --package backend

run-frontend:
    just -f frontend/justfile run

clean:
    cargo clean

test:
    cargo test

mosquitto:
    docker run -it -p 1883:1883 -v "$PWD/mosquitto/config:/mosquitto/config" -v /mosquitto/data -v /mosquitto/log eclipse-mosquitto

mqtt-monitor:
    mosquitto_sub -h localhost -p 1883 -t '#' -v
