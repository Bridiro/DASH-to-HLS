# 🎥 CENC-to-HLS: Live DASH Stream Decryption & Playback System

This project is a full-stack system for decrypting and converting **CENC-encrypted MPEG-DASH streams** to **HLS**, with a built-in web interface for playback.

## ✨ Features

- 🔐 **Cookie-based login** system (JWT stored in secure HTTP-only cookies)
- 🔄 **Real-time DASH to HLS conversion** via FFmpeg (or custom processing)
- 🧠 **Memory-efficient streaming manager** with idle timeout cleanup
- 🧼 Simple `.toml` config files for channels and users
- 💻 Lightweight, no database — runs with flat config files

## 🗂️ Tech Stack

- **Backend:** Rust + Actix-Web
- **Frontend:** HTML + HLS.js
- **Streaming:** FFmpeg  & bento4 (or direct segment manipulation)
- **Auth:** JWT via cookies, parsed from `.env`

## 🚀 Getting Started

### 1. Setup the Rust backend

Make sure to install **ffmpeg** as a minimum requirement, and **bento4** for maximum performance.

```bash
touch .env
echo "<create a token>" > .env
```

Create the config files as specified below, and you're ready to go.

```bash
cargo run --release
```

### 2. Browse in web:

- Go to `http://<your-ip>:8080`
- Log in using user credentials in `users.toml`

## 📁 Config Files

### `channels.toml`

```toml
[[channel]]
id = "demo"
name = "Demo Channel"
url = "https://example.com/manifest.mpd"
key = "0123456789abcdef"
```

### `users.toml`

```toml
[[user]]
username = "alessandro"
password = "12345abcde"
```

> [!WARNING]
> This is a personal project intended for educational/experimental use. Not intended for public redistribution of copyrighted content.

## 📄 License

This project is licensed under the [MIT License](./LICENSE).
