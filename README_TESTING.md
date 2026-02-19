# How To Test

Follow these steps from the project root.

## 1. Pull the submodule dependencies

`pubspec.yaml` uses a local path override from the `third_party/sqlite3.dart` submodule, so initialize and update submodules first.

```bash
git submodule update --init --recursive third_party/sqlite3.dart
```

## 2. Build the Docker image

```bash
docker build -t libturso:sandbox .
```

## 3. Run the container with the current directory mounted

```bash
docker run --rm -it \
  -v "$(pwd):/app" \
  -w /app \
  libturso:sandbox
```

## 4. Compile the binary inside the container

```bash
cargo build
```

## 5. Run the Dart file in `bin/` to test

```bash
dart run bin/libsqlite3_turso.dart
```
