<div align="center">

<img src="ui/img/logo.png" width="96" alt="Karadelik Download Manager logo">

**English** · [Türkçe](README.tr.md)

# Karadelik Download Manager

**A fast, open-source download manager for Windows and macOS.**
Multi-connection resumable downloads, queues and a scheduler, HLS/DASH streams, FTP, BitTorrent, a site grabber, ZIP preview
and browser integration in one window. No telemetry.

![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011%20x64-0078d6)
![Tauri 2](https://img.shields.io/badge/Tauri-2-24c8db)
![Rust](https://img.shields.io/badge/Rust-stable-dea584)

</div>

---

## Features

**Download engine**
- Starts with one connection and, if the server allows it, doubles the connection count every half second
  (default max 8, configurable 1–32).
- Splitting is time-based: a connection whose remaining work would take longer than one second has its range split
  in half. That keeps the connection count sensible on both fast and slow links.
- Writes straight into a single sparse `.kdm` file: no part files and no merge step at the end.
- **Safe resume** with `ETag`/`Last-Modified` + `If-Range`: if the file changed on the server, this is detected and the
  download starts over. Downloads survive an app restart.
- Detects servers that don't support ranges, or claim to and ignore them, and falls back to a single connection.
- Retries a stalled or dropped connection from where it stopped. A refused connection on a download that has received
  nothing yet (wrong address or port) fails at once; a partial download keeps retrying, since the server may be
  restarting.
- Global and per-download speed limits (token bucket).
- Optional SHA-256 / SHA-1 / MD5 verification.
- HTTP Basic and Digest authentication; HTTP and SOCKS4/4a/5 proxies.
- File names from `Content-Disposition` (RFC 6266/5987), made safe for Windows; `name (1).ext` on collisions.

**FTP**
- EPSV / PASV, multi-connection downloads and resume with `REST`.
- Detects files that changed on the server via `MDTM`.
- The data connection always goes to the control connection's address, so a malicious server cannot redirect the
  client to another machine.
- FTPS: `ftps://` (implicit TLS) and, for `ftp://`, explicit TLS (`AUTH TLS`) set to off, "if available" or
  "required" in Preferences > Connection. The server certificate is always verified with the operating system's
  verifier; data connections resume the control connection's TLS session, which vsftpd, ProFTPD and FileZilla
  Server demand.

**BitTorrent and magnet**
- Magnet links (trackerless ones through DHT) and `.torrent` files (from this computer or a web address).
- File list in the add window: pick the files you want, only those are downloaded.
- Metadata is kept once fetched; after a restart existing data is verified piece by piece and the download resumes.
- **Web seeds (BEP 19):** if no data arrives from peers for 20 seconds, the torrent's HTTP source is used (e.g.
  archive.org). The data is verified against the torrent's piece hashes; corrupt pieces are not accepted.
- Seed ratio (default 1.0, 0: no seeding), upload limit, listening port, DHT, local peer discovery; UPnP is optional
  and off by default. Files are released when seeding ends.
- The torrent session does not open until the first torrent starts; nothing listens on a port for people who never
  use torrents.
- Torrents with file paths that are unsafe on Windows (drive-prefixed components such as `C:`, or `:`) are rejected.
- The clipboard monitor, the browser extension (context menu and "all links") and the command line recognise magnet
  links. Preferences > BitTorrent can make the app the default for magnet links and `.torrent` files (only when you
  ask).

**Streams (HLS / MPEG-DASH)**
- Pick the quality and audio language.
- AES-128 encrypted HLS.
- `SegmentTemplate`, `SegmentTimeline`, `SegmentList`, `SegmentBase` (DASH).
- Segments download in parallel; stopping and resuming skips segments that are already done.
- Merges into a single MP4 with [ffmpeg](https://ffmpeg.org) if available (no re-encoding); otherwise leaves a `.ts`
  or separate audio/video files.

**Queues and scheduler**
- Multiple queues, each with its own number of simultaneous downloads.
- Start/stop by day and time, "run only once".
- Retries failed files with exponential back-off (30 s, 1 min, 2 min…).
- When a queue finishes: exit, sleep, hibernate or shut down. Actions that affect the computer show a cancellable
  60-second countdown first.
- **Download quota:** "at most X MB in the last H hours"; when exceeded, downloads pause and resume on their own as the
  window moves on.

**Browser integration** (Chrome, Edge, Firefox)
- Takes over browser downloads together with cookies, referer and user agent.
- Context menu: "Download with Karadelik", "Download all links with Karadelik".
- Detects HLS/DASH and media streams on the page and lists them under the extension icon.
- Talks to the app over a local WebSocket. The connection is authenticated with a key that changes on every launch
  and is only accepted from the extension's own origin, so ordinary web pages cannot inject downloads.

**More**
- Categories (Compressed, Documents, Music, Programs, Video): files go to a folder by extension.
- **Site Grabber:** crawls a site with depth, file type, path and size rules; honours `robots.txt`.
- **ZIP preview:** see what's inside an archive without downloading it, then download and extract only the files you pick.
- Clipboard monitoring, drag and drop, paste (Ctrl+V).
- Antivirus scan after download (Windows Defender by default).
- Tray icon, Turkish and English interface.
- Automatic updates verified with SHA-256.

## Privacy

Karadelik DM sends no usage data. The addresses you download, cookies and file names stay on this computer. Its only
own network request is the optional update check (`latest.json` on GitHub). By the nature of BitTorrent, your IP
address is visible to the peers and trackers you connect to while downloading a torrent.

## Installation

1. Download and run `KaradelikDM-<version>-setup.exe` from the [releases](https://github.com/enesbsafak/karadelikdm/releases/latest)
   page. It installs per user and needs no administrator rights.
2. Browser extension: run `node build.mjs` in the `extension/` folder.
   - **Chrome / Edge:** `chrome://extensions` → Developer mode → "Load unpacked" → `extension/chromium`.
   - **Firefox:** `about:debugging` → "Load Temporary Add-on" → `extension/firefox/manifest.json`.

   The extension finds the app through a helper (`karadelikdm-nmh.exe`) that the app registers with the browsers on first launch.

## Command line

```text
karadelikdm <url> [url...] [--dir FOLDER] [--name NAME] [--queue QUEUE] [--start | --later]
```

An address can be `http(s)://`, `ftp(s)://`, `magnet:?xt=urn:btih:…` or the path of a `.torrent` file.

Without `--start` or `--later` the add window opens. If the app is already running, the addresses are passed to it.
For older scripts `/d URL /p FOLDER /f NAME /n` (start now) and `/a` (add to queue) are accepted too.

## Folders

| What | Where |
| --- | --- |
| Settings, download list, log | `%APPDATA%\KaradelikDM` (`settings.json`, `downloads.json`, `kdm.log`) |
| Downloaded files | Preferences > Saving (default: Downloads, with category subfolders) |
| Partial download | `<name>.kdm` in the target folder |
| Torrent metadata, DHT state | `%APPDATA%\KaradelikDM\torrents` |

`downloads.json` is backed up to `.bak` before every save; if the main file is damaged the backup is used.

## Known limitations

- No active-mode FTP (PORT), no FTPS client certificates; FTP does not use the proxy. In "if available" mode a
  server whose certificate cannot be verified (self-signed, common on FTP) makes the download fail rather than
  silently falling back to plain FTP; explicit TLS is therefore off by default.
- DRM-protected and SAMPLE-AES streams, live streams, and videos played through `blob:` / MSE (e.g. YouTube) cannot
  be downloaded.
- Browser downloads started by a POST form are handed over as GET and may fail.
- Names in ZIP archives without the UTF-8 flag are read as CP437; older archives made on Turkish Windows may show
  ş, ğ, ı incorrectly.
- The site grabber fetches pages one at a time and does not see links created by JavaScript or CSS `url()`.
- If IDM is installed on the same computer, its driver captures the file types on its own list before the browser
  does, and our extension never gets a chance. Turn off IDM's browser integration if you use both.
- BitTorrent: only BitTorrent v1 (`btih`); v2-only (`btmh`) magnet links do not open. No protocol encryption
  (MSE/PE). Per-download speed limits do not apply to torrents (the global limit does). Seeding continues only while
  the app is open. Web seeding works per file and starts only when peers make no progress. No magnet / `.torrent`
  association on macOS yet.
- The macOS build has not been tested yet. Cross-compiling it on Windows is not possible either (C dependencies need
  Apple SDK headers); `.github/workflows/ci.yml` builds and tests on macOS on every push.

## Building from source

Requirements: [Rust](https://rustup.rs) (stable), [Node.js](https://nodejs.org) 20+, WebView2 (included in Windows 10/11).

```powershell
npm install
cargo test --workspace                     # 220+ tests, no network access needed
cargo build -p kdm-nmh
copy target\debug\karadelikdm-nmh.exe src-tauri\binaries\karadelikdm-nmh-x86_64-pc-windows-msvc.exe
npm run dev                                # development
powershell -ExecutionPolicy Bypass -File scripts\release.ps1   # installer + latest.json
```

### Architecture

| Crate | Role |
| --- | --- |
| `crates/kdm-core` | Download engine, queues/scheduler, quota, categories |
| `crates/kdm-ftp` | FTP client |
| `crates/kdm-torrent` | BitTorrent / magnet (on top of [librqbit](https://github.com/ikatson/rqbit)), web seed addresses, path checks |
| `crates/kdm-media` | HLS/DASH parsing, segment download, merging with ffmpeg |
| `crates/kdm-grabber` | Site crawler, `robots.txt` |
| `crates/kdm-zip` | Remote ZIP listing and selective extraction |
| `crates/kdm-bridge`, `crates/kdm-nmh` | Browser bridge and native messaging helper |
| `src-tauri` | Tauri app: windows, tray, commands |
| `ui` | Interface (HTML/CSS/JS) |
| `extension` | Chrome/Edge/Firefox extension |

Design decisions and their reasoning (in Turkish): [`docs/tasarim.md`](docs/tasarim.md).

## License

[MIT](LICENSE) © 2026 enesbsafak

The BitTorrent protocol is provided by [librqbit](https://github.com/ikatson/rqbit) (Apache-2.0).
