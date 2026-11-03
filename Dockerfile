# Reproducible build matching the host stack (Arch: GTK 4.24, libadwaita 1.10, mpv 0.41).
FROM archlinux:latest

RUN pacman -Syu --noconfirm --needed \
        rust pkgconf gtk4 libadwaita mpv

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY player-core ./player-core
COPY player-video ./player-video
COPY player-ui ./player-ui

RUN cargo build --release --locked

# GUI smoke test inside the container needs a display; run with:
#   docker run --rm -it -e DISPLAY=$DISPLAY -v /tmp/.X11-unix:/tmp/.X11-unx \
#     mpv-gtk-build target/release/mpv-gtk some-video.mkv
