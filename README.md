# Music Player

A standalone MyWallpaper add-on that controls the music already playing on Windows. Four distinct presentations share one implementation: Minimal, Artwork + spectrum, Full artwork, and Compact bar. The Windows companion owns media access; the application core is unchanged.

## What it controls

The companion uses Windows Global System Media Transport Controls (GSMTC). Spotify, browsers and other players appear only when they publish a Windows media session. The card displays the real title, artist, album, cover, playback state and timeline. Play/pause, previous/next, seeking, shuffle and repeat are enabled only when the selected session reports support. The options menu also selects among available media sessions. A rejected command is reported; the card never presents a simulated playback success.

The heart saves a track name in the add-on’s **local device settings**. It keeps at most the 100 most recently saved tracks. It does not add a Spotify like or alter the source application’s library. Unsupported player actions stay disabled. Live broadcasts can lack a seekable timeline or an album cover.

The spectrum presentation uses a real, optional WASAPI loopback capture of the default Windows output’s **system mix**, not microphone audio. Other applications’ output can contribute. Capture starts only for that presentation and stops when it is deselected or the add-on is disposed. No raw audio is saved or uploaded. If capture is unavailable, playback controls continue to work and no artificial bars replace the missing audio.

## Settings and use

Pair the local development session in MyWallpaper Desktop’s Developer settings and accept the native companion only after reviewing its source. Add the layer, resize/position it using the standard editor, then choose a presentation from its settings or its ⋯ menu. Layout, background treatment, background color/opacity/blur, text color, accent, border opacity and language are per layer. New instances start transparent, without a border or a fixed colored glow. Choose a solid color or the current track cover when a background is useful. Background opacity never fades the text or controls. Saved tracks are device-scoped. Settings and Windows messages are validated before display; track names are rendered as text.

The card uses the selected player’s cover. The mountain artwork and “Better Days / Luna River” shown in the browser preview and thumbnail are synthetic demonstration assets; they never replace real playback metadata in interactive mode. No music files or account credentials are bundled.

Version 1.1.0 removes the overlapping Centered, Frosted controls and Editorial choices. Existing instances stay pinned to their installed release; when updating, use the settings compatibility preview in MyWallpaper to choose a supported layout. No automatic migration script is included.

## Build

Use Node 22+, pnpm 10.33.0 and the Rust toolchain pinned in `rust-toolchain.toml`.

```sh
pnpm install --frozen-lockfile
pnpm build
pnpm test
```

On Windows, build the companion with the pinned Rust toolchain and the Visual C++ build tools:

```sh
cargo install --path native/companion --locked --target x86_64-pc-windows-msvc --root native/out/windows-x86_64 --force
```

The output is `native/out/windows-x86_64/bin/backend.exe`. Only x64 is declared because this version of the MyWallpaper SDK exposes `windows-x86_64`.

The canonical `mywallpaper.config.json` defines finite web/native builds; `mywallpaper dev` owns watching and immutable revisions. After a contract change run `mywallpaper generate`, then `mywallpaper check` to verify the manifest, generated declarations and distribution closure. Native binaries, `dist/` and local development state are generated and are not committed.

## Visual preview

```sh
pnpm preview
```

Open http://localhost:5188/preview.html. It deliberately uses a marked synthetic fixture to compare all four layouts without Windows playback. This preview is not a replacement for a paired Desktop session. It is not part of the shipped Canvas entry.

## Release

Publication is centrally dispatched by MyWallpaper: this repository intentionally has no copied publication workflow or pre-created GitHub release. The tagged public source is rebuilt by the existing MyWallpaper native add-on toolchain. Register the repository and submit its exact tagged release using the creator CLI or MCP. The account service remains authoritative for creator terms, publication eligibility and validation. A local build or preview is not a catalogue publication.
