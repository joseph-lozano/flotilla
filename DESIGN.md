---
name: Flotilla
description: A desktop control room for a fleet of coding agents on cloud VMs, in the icon's plum with pixel sails.
colors:
  plum: "#311E36"
  plum-ink: "#24152A"
  coral: "#F37173"
  amber: "#F6BC63"
  lavender: "#C2A7E2"
  day-paper: "#FCFAFD"
  day-rail: "#F4EFF6"
  day-muted: "#F2ECF4"
  day-border: "#E7DEEB"
  day-text-muted: "#6C5C73"
  day-selection: "#E9DEF0"
  day-danger: "#C93D47"
  day-warning: "#9A6410"
  day-success: "#1E7F48"
  day-link: "#6B4A94"
  day-ring: "#8E6CB8"
  night-water: "#1D1322"
  night-rail: "#170F1B"
  night-muted: "#281B2D"
  night-border: "#33243A"
  night-text: "#F1E9F4"
  night-text-muted: "#A898AE"
  night-selection: "#33233B"
  night-success: "#6ACB92"
  night-link: "#D3BFEC"
typography:
  title:
    fontFamily: ".SystemUIFont"
    fontSize: "15px"
    fontWeight: 600
  body:
    fontFamily: ".SystemUIFont"
    fontSize: "14px"
    fontWeight: 400
  meta:
    fontFamily: ".SystemUIFont"
    fontSize: "12px"
    fontWeight: 400
  data:
    fontFamily: "Menlo"
    fontSize: "12px"
    fontWeight: 400
rounded:
  pip: "1.5px"
  control: "6px"
  panel: "10px"
  composer: "16px"
spacing:
  cell: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
components:
  button-primary-day:
    backgroundColor: "{colors.plum}"
    textColor: "{colors.day-paper}"
    rounded: "{rounded.control}"
    height: "32px"
  button-primary-night:
    backgroundColor: "{colors.lavender}"
    textColor: "{colors.plum-ink}"
    rounded: "{rounded.control}"
    height: "32px"
  hold-to-merge-day:
    backgroundColor: "{colors.day-success}"
    textColor: "{colors.day-paper}"
    rounded: "{rounded.control}"
    width: "148px"
    height: "32px"
  hold-to-merge-night:
    backgroundColor: "{colors.night-success}"
    textColor: "{colors.plum-ink}"
    rounded: "{rounded.control}"
    width: "148px"
    height: "32px"
  status-pip:
    rounded: "{rounded.pip}"
    size: "6px"
---

# Design System: Flotilla

## Overview

**A flotilla at dusk.** The app is the plum water the agents' sails ride on: plum-tinted neutrals for the ground, the icon's coral, amber and lavender for the few things that should catch the eye, and a soft pixel motif taken from the icon's stepped sails. It's an Operate surface: the layout, navigation and controls are the platform's and the settled ones (sidebar groups and four-line rows, the two-line thread header, the PR bar joined to the composer). The world lends only palette, type, density and one signature move, the soft pixel.

Inspired by boat.dev (pixel sails on a duotone sea; small monospace captions that are real commands), not a copy: Flotilla's sea is plum, its sails are the icon's three colours, and there's no illustrated seascape behind working screens.

Two themes, following macOS: **Day** (lavender-tinted paper, plum ink) and **Night** (deep plum water, lavender light). Defined in `crates/flotilla-app/src/flotilla-theme.json`, installed at startup by `theme::install`.

## Colors

### Primary

- **Plum** `#311E36`: the icon's ground. Day's primary buttons and selected pills; Night's darkest tones derive from it.
- **Lavender** `#C2A7E2`: Night's primary (buttons, selected pills, focus ring, caret, selection at ~33%). By day it appears only as tint (selection `#E9DEF0`, ring `#8E6CB8`).

### Secondary

- **Coral** `#F37173`: failure and conflict (Night as is; Day deepens it to `#C93D47` for text contrast).
- **Amber** `#F6BC63`: waiting and pending (Day `#9A6410`).
- **Green** (Day `#1E7F48`, Night `#6ACB92`): pass, ready, merge. Not from the icon; kept because "green means go" is the category's language for checks and merging.

### Neutral

Every neutral is tinted toward plum; there is no pure grey. Day: paper `#FCFAFD`, rail `#F4EFF6`, muted `#F2ECF4`, border `#E7DEEB`, muted text `#6C5C73`. Night: water `#1D1322`, rail `#170F1B`, muted `#281B2D`, border `#33243A`, text `#F1E9F4`, muted text `#A898AE`.

### Named Rules

- **The Three Sails Rule.** Coral, amber and lavender at full strength are brand art (the pixel sails) and Night's state colours. Never body text on Day; Day uses the deepened values.
- **Problems Speak, Successes Whisper.** Coral and amber appear only for a state that needs the user; a hook that ran, a VM that's up, checks that pass stay muted or green-small.

## Typography

The system UI font for everything people read; Menlo for everything that is data. Sizes are gpui-kit's (`text_xs` 12, `text_sm` 14, base 16); weights 400 and 600 only.

### Hierarchy

- **Title** (15px, 600): the thread's title, dialog titles.
- **Body** (14px, 400): replies, prompts, settings prose.
- **Meta** (12px, 400, muted): the header's second line, row metadata, hints.
- **Data** (Menlo 12px): branches, SHAs (`main@212f6bd`), scripts (`.agents/setup`), commands and tool calls, IDs.

### Named Rules

- **The Real Data Rule.** Monospace marks real data, never decoration: a branch, a SHA, a script, a command. Labels and prose stay in the UI font.

## Layout

Fixed shell: a 280px sidebar rail, the thread to the right; minimum window 760×480 (at 760 the header's project part and the PR title truncate first). Density is a working tool's: rows four lines, the header two lines plus a problems line only when needed, the PR bar one 40px strip on the composer. Spacing steps of 4px (the pixel cell): 4, 8, 12, 16.

## Elevation & Depth

Flat and tonal: the rail is a step darker (Night) or tinted (Day), selection is a tint, the PR bar is the muted tone. Only floating things carry gpui-kit's soft shadow: dialogs, popovers, menus, the hover card, toasts. The dialog backdrop is plum-tinted (Day `#2416293D`, Night `#0B060DA6`).

## Shapes

Controls 6px radius, panels and dialogs 10px, the composer 16px. Status pips are squares with corners a quarter of their size (6px pip, 1.5px radius): the soft pixel. Pills stay pills (small controls only).

## Components

- **Primary button:** plum by day, lavender by night, 32px tall. Secondary/outline buttons use the tinted secondary tone.
- **PR bar:** joined to the top of the composer, muted ground; `#N` and title, checks, +/−, the state in words (coral for conflicts and failing checks, with an icon), actions full-height at the right edge.
- **Hold to merge** (signature): a 148×32 green-tinted button that fills with solid green from the left while held (1.2 s, linear), advancing in whole 4px cells; letting go drains it in 200ms (ease-out `cubic-bezier(0.23, 1, 0.32, 1)`). The label swaps to the fill's foreground under the fill.
- **Status pip:** `pixel::pip`, for check state, VM state and unread (the ring's lavender: full by night, deepened by day).
- **Pixel sail and mark:** `pixel::sail` (the icon's sail: a 6-row stepped triangle on its mast over a 2-row hull, 8 cells wide) and `pixel::mark` (coral, amber two cells higher, lavender). Drawn from exact cell runs at 4px cells. The mark sits over the New thread composer; a lavender sail over a thread's empty state.

## Do's and Don'ts

- Do use the theme roles (`theme.primary`, `theme.danger`, …); the only literal colours are the three sail colours in `pixel.rs`.
- Do keep pixel accents to empty states, the mark and status marks; at most one sail or mark per screen.
- Do keep the sidebar still: no animation there (Joseph's rule); pips are static.
- Don't use a pixel or monospace font for prose, or monospace as a "technical" costume.
- Don't add a seascape or texture behind working screens; the sea is the plum ground itself.
- Don't say "template" anywhere in the UI.
- Don't introduce pure greys; tint neutrals toward plum.

## The website (useflotilla.com)

`www/index.html` is a Persuade surface in the same world: the app's palette and Day/Night, with more room for the brand than the app allows itself. One static file, no build step.

- **Type:** the system font, display at 600. H1 `clamp(2.5rem, 1.3rem + 3.4vw, 4rem)`, line-height 1.03, tracking -0.026em; section heads `clamp(1.875rem, 1.3rem + 1.9vw, 2.75rem)` at -0.021em; body 17px/1.6, ledes 18px. Menlo only in the command blocks and for real names like `main`.
- **The fleet chart** (hero): devices, the Flotilla server, and boat.dev's sea (the rail tone) with a berth per thread, each with its agent's sail. Berths and nodes are 10px panels; routes are 2px rules in the border tone; packets are 6px squares in the ring colour. A narrow layout has its own vertical chart.
- **Motion moves in whole cells** (`steps()`): packets, the sails' bob, OpenCode's sail hoisting when its stopped VM wakes, the hand-off diagram (plays once), the end card's sails (once). Loops pause off screen. Reduced motion shows every diagram at its end state.
- **Hold to merge** is the app's: 148×32 in the screenshot's own scale, 1.2 s linear in 4px cells, 200ms drain on `cubic-bezier(0.23, 1, 0.32, 1)`, Space or Enter held works too. Below 760px of window it moves under the screenshot at 40px tall.
- **Frames:** Mac windows 12px radius with a title bar in the page's theme; iPhones 52px outer, 41px glass. Every product capture has a Day and a Night version, swapped by `<picture>` with `prefers-color-scheme`, and the Hold to merge laid over the Mac capture takes that theme's sampled colours. Command blocks are plum-ink in both themes.
- **The end card** is the one full-plum field: the mark at 336px, the wordmark, the lavender button.
- **Copy rules** live in `design/site-positioning.md`.
