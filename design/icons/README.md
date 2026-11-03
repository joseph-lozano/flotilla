# Flotilla app icon

`AppIcon.png` is the app icon: three single sails in coral, gold and lilac on plum, as coarse
pixel art. It was made with Codex image generation, picked from eight candidates (the rest were
dropped). It borrows boat.dev's pixel-art technique but not its look: no two-sail side-view
boat, no cobalt and white, no text.

- Palette: a plum background with coral, gold and lilac sails.
- Format: full-bleed, opaque, 1024×1024, square corners. The rounded macOS shape and shadow
  are added at build time.

`AppIcon-dev.png` is Flotilla Dev's (debug builds): the same sails on sea green over "DEV" in
pixels, made from `AppIcon.png` by `make-dev-icon.py`.

`script/bundle-macos` builds `target/release/Flotilla.app` from it (`--dev`: Flotilla Dev's);
`script/make-iconset.swift` cuts it into the macOS icon sizes, keeping the pixels crisp.
