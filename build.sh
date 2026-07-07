#!/bin/bash

# Set the hostname for the remarkable based on $1 and fall back to `remarkable`
remarkable="${1:-remarkable}"

if [ "$1" == "local" ]; then
    cargo build --release
elif [[ "$1" == rmpp* ]]; then
    cross build \
      --release \
      --target=aarch64-unknown-linux-gnu \
      && scp target/aarch64-unknown-linux-gnu/release/smart_remarkable root@$remarkable:
else
    cross build \
      --release \
      --target=armv7-unknown-linux-gnueabihf \
      && scp target/armv7-unknown-linux-gnueabihf/release/smart_remarkable root@$remarkable:
fi

