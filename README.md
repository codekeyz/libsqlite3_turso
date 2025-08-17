# libsqlite-turso

A dynamic strategy wrapper for `libsqlite3.so` that allows any SQLite client to seamlessly connect to [Turso](https://turso.tech/) databases — with zero client-side changes.

This project provides drop-in `libsqlite3.so` support with automatic strategy selection depending on runtime context.

## ✨ Features

- ✅ Works with **any SQLite client** that uses `libsqlite3.so`
- 🔁 Automatically picks between strategies:
  - **`EnvVarStrategy`** — for general use outside Globe
  - **`GlobeStrategy`** — for auto-authenticated execution inside a Globe edge function
- 🔌 No custom SQLite client logic or HRANA knowledge required

---

## 🔧 Setup

### 1. Build the custom `libsqlite3.so`

```bash
cargo build --release
```

### 1. Place `libsqlite3.so` in your system

This project assumes `libsqlite3.so` is available at runtime.

Place it in a standard library path (e.g., `/usr/lib`, or use `/usr/local/lib/`).

---

## 🚀 Usage

Use **any standard SQLite library** in your language/runtime — this project handles the dynamic strategy and connection logic under the hood.
