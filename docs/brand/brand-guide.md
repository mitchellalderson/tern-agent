# tern brand guide

Visual reference: [style-sheet.html](style-sheet.html). That sheet is the source of truth for how things look; this file is the source of truth for the rules and values. If they disagree, fix the sheet.

**The idea:** vintage Toyota racing stripe, Baja racer. Warm cream paper, sharp ink, and one orange / yellow / teal stripe. Simple, warm, clean.

## 1. Color

All colors are tokens. Never hard-code a hex in UI code; use the token.

### Stripes

| Token    | Light     | Dark      | Use |
|----------|-----------|-----------|-----|
| `orange` | `#F07A2B` | `#F58A3D` | Top stripe, primary accent: primary buttons, selection edge |
| `yellow` | `#F6B92B` | `#F9C846` | Middle stripe, hover, focus glow, highlights |
| `teal`   | `#2A8C8C` | `#4DB3AE` | Bottom stripe only. Not for text or fills elsewhere |

Red is not a stripe color: on a dark background red, orange and yellow read as a national flag. `red` (`#D93A2B` light, `#E5483A` dark) exists only for destructive fills.

### Surfaces and text

| Token      | Light     | Dark      | Use |
|------------|-----------|-----------|-----|
| `cream`    | `#F7EEDC` | `#1F1812` | App background |
| `paper`    | `#FCF7EB` | `#2A211A` | Raised: cards, inputs, agent bubbles |
| `sand`     | `#EFE3CB` | `#261E17` | Sidebar, user bubble, code blocks |
| `hairline` | `#E0D2B6` | `#3D3226` | Dividers, resting borders |
| `ink`      | `#2B2118` | `#F3E8D2` | Text, interactive borders |
| `muted`    | `#6F604F` | `#B3A58F` | Secondary text |
| `link`     | `#B3281D` | `#F08A7A` | Links |

### Status

| Token   | Light     | Dark      |
|---------|-----------|-----------|
| `ok`    | `#4F7A3A` | `#8DB56F` |
| `warn`  | `#B8841A` | `#E3B04B` |
| `error` | `#B3281D` | `#F08A7A` |

### Color rules

- Body text is `ink` (or `muted` for secondary). Never orange or yellow text on cream; they fail contrast.
- Text on an orange, yellow, or teal fill is `ink`, never white. Exception: text on the `red` destructive button is `paper`.
- The background is always warm. No pure white, no pure black, no neutral gray.
- Dark mode is its own palette, not an inversion. Primary button text stays dark `ink` (`#2B2118`) in both modes.
- No gradients, glows, glass, or drop shadows. Depth comes from `paper` vs `sand` vs `cream`.

## 2. The stripe

Three bars, hard-edged, no gaps, always in this order: orange, yellow, teal (top to bottom, or left to right when vertical). Never reorder, recolor, or space them.

| Variant   | Bar thickness | Use |
|-----------|---------------|-----|
| Page edge | 6 to 8px      | Top of a window or page |
| Slanted   | 12 to 14px, skewed -18° | Hero, empty states, onboarding |
| Small     | 3 to 4px, skewed -18°   | Dividers and quiet accents (the sidebar logo uses the bird, not a stripe) |
| Vertical  | 12px          | Side tab or divider |

- At most one stripe element per view, plus the logo lockup.
- The logo lockup is the lowercase wordmark with the bird mark (section 3) to its right. The stripe is a page and hero motif, not part of the lockup.

## 3. The mark

A tern in flight, seen from below, on an `orange` circle. Source: `apps/desktop/assets/icon/tern-icon.svg`; the macOS app icons are generated from it with `apps/desktop/tool/make_app_icons.sh`.

| Part   | Token   | Value (both modes) |
|--------|---------|--------------------|
| Field  | `orange` | `#F07A2B` |
| Bird   | `paper` | `#FCF7EB` |
| Cap    | `ink`   | `#2B2118` |

- The mark is one color set in light and dark; it does not change with the theme.
- The bird is `paper`, never pure white, and the cap is `ink`, never pure black.
- Minimum size 24px. Below that, use the bird alone as a one-color silhouette (drop the cap).
- Keep clear space of at least a third of the circle's width. No stripe, shadow or outline on the mark.
- In the lockup the mark sits to the right of the wordmark, centered on its x-height, at about 1.3 times the wordmark's cap height.

## 4. Typography

| Role    | Family               | Notes |
|---------|----------------------|-------|
| Display | Archivo (Expanded)   | Weight 800 to 900, italic, width 112 to 125%. Wordmark, titles, big numbers. |
| Body    | DM Sans              | 400 for text, 700 for emphasis and buttons. |
| Mono    | JetBrains Mono       | Code, commands and tool calls. |

The wordmark is always lowercase: `tern`, Archivo 900 italic, width 125%, letter-spacing -0.03em. Everywhere else is sentence case.

| Style   | Size / weight | Family |
|---------|---------------|--------|
| Title   | 28 / 800 italic | Display |
| Heading | 18 / 700 | Body |
| Body    | 14 / 400, line height 1.45 to 1.5 | Body |
| Small   | 12.5 / 400, `muted` | Body |
| Label   | 12 / 600, sentence case, `muted` | Body |

Flutter note: the three families must be bundled as app assets, not fetched at runtime.

## 5. Shape and components

- **Radius:** 8 for controls, 12 for cards and bubbles, full only for avatars and dots.
- **Borders:** 1px `hairline` at rest. 1.5 to 2px `ink` for anything interactive or active.
- **Primary button:** `orange` fill, `ink` text, 2px `ink` border, 8px radius, weight 700. Hover: `yellow` fill.
- **Secondary button:** transparent, `ink` text, 2px `ink` border.
- **Ghost button:** same, with a `hairline` border.
- **Destructive button:** `red` fill, `paper` text.
- **Input:** `paper` fill, 1.5px `hairline` border. Focus: `ink` border plus a 3px `yellow` glow at about 55% opacity.
- **Status:** a dot in the status color, then the word in `ink`, body 13px weight 500. No border, no fill, no uppercase. Where state shows in a list, the dot alone is fine.
- **Sidebar selection:** `cream` fill, `ink` text weight 600, 3px `orange` bar on the left edge.
- **Bubbles:** user is `sand`. Agent is `paper` with a 1px `hairline` border. Max width 80%.
- **Composer:** `paper` fill, 1.5px `ink` border, 10px radius, orange Send button with `ink` text.

## 6. Voice

Plain, short, calm. Sentence case. Say what happened and what is needed next. No exclamation marks, no hype.

## 7. Do and don't

**Do**
- Keep the background cream and let the stripes be the only loud thing.
- Use `ink` borders to mean "you can click this".
- Leave generous empty space.

**Don't**
- Put orange or yellow text on cream, or white text on orange.
- Use gradients, glows, glass, or shadows.
- Wrap labels or statuses in bordered pills, chips or badges. Prefer a dot and a word, or plain text with a hairline rule.
- Use pure white or neutral gray.
- Reorder, recolor, or space out the stripes.

## 8. Mapping to the desktop app

`apps/desktop/lib/theme.dart` holds `TernColors`. Planned mapping from the current fields:

| `TernColors` field | Brand token |
|--------------------|-------------|
| `background` | `cream` |
| `sidebar` | `sand` |
| `card` | `paper` |
| `hairline` | `hairline` |
| `text` | `ink` |
| `muted` | `muted` |
| `accent` | `orange` |
| `onAccent` | `ink` (dark in both modes) |
| `ok` / `warn` / `error` | `ok` / `warn` / `error` |
| `userBubble` | `sand` |
| `selection` | `sand`, with the orange left bar |

New fields to add: `stripeOrange`, `stripeYellow`, `stripeTeal`, `link`, `ink` (for interactive borders). The multi-color avatar palette in `ui/widgets.dart` should be reduced to stripe and status colors.
