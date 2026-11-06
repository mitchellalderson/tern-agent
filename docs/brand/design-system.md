# tern design system

The brand guide ([brand-guide.md](brand-guide.md)) sets the rules and values, and the style sheet ([style-sheet.html](style-sheet.html)) shows them. This file and [tokens.json](tokens.json) turn them into tokens that tools and agents can read.

- **[tokens.json](tokens.json)**: every brand token in one file, as lists of `{name, value, usage}`. It covers color (light and dark), type styles, the bundled font files, spacing, radius, stroke widths and stripe sizes. Color values are copied from the guide unchanged. Every `TernColors` field in `apps/desktop/lib/theme.dart` matches its token, except `selection`, which has no brand token (see below).
- **Published design system**: https://claude.ai/artifact/VNQGETC8nopXSXbB3EgUeK. It holds the same tokens, a component card with a rendered preview for each item in guide section 5, the mark, and the fonts. It is private until someone shares it from its Share menu.

When a token changes, update the guide first, then `tokens.json`, then `theme.dart`.

## Defaults and open questions

The brand guide is the source of truth. These are the places it is silent, where the two brand files disagree, or where its values miss a contrast floor. Treat the defaults as provisional until the guide decides them.

### Defaults chosen where the guide is silent

- **Spacing scale.** The guide gives only message padding (14 by 9). `space-6` to `space-56` are taken from the style sheet's measurements; `space-4` and `space-8` are added defaults. The desktop app also uses 5, 7, 9, 11 and 14px paddings freely.
- **`radius-inner` (6px).** Used by the style sheet for sidebar rows and the Send chip, and 25 times in the app. Not in the guide.
- **Body line height 1.45.** The guide allows 1.45 to 1.5; 1.45 matches the app.
- **Code size 12 / 1.4.** The guide names the mono family but no size; the app uses 12, the style sheet 14.
- **Heading and small line heights** (1.3, 1.4) are defaults.
- **Focus on buttons.** The guide defines focus only for inputs. Buttons here reuse the `focus-glow` ring.
- **Disabled state.** Not defined anywhere; the CSS uses 45% opacity as a placeholder.
- **Motion.** The guide says nothing. The app has a 600ms tooltip delay and instant hover changes; no motion tokens are defined.
- **Elevation.** Only "no shadows; depth from paper vs sand vs cream". Floating layers (menus, tooltips, popovers) have no rule; the app's tooltip is an `ink` fill with `cream` text, and a few widgets still set Material elevation.

### Where the style sheet and the guide disagree

The guide says to fix the sheet when they disagree.

- **Label style.** Sheet: 11px mono, uppercase, tracked. Guide: 12 / 600 DM Sans, sentence case, `muted`. This system follows the guide.
- **Small stripe.** Sheet labels it "under the logo" and draws one in the sidebar mock. Guide: the sidebar logo uses the bird, not a stripe.
- **Sidebar selection.** Guide section 5: `cream` fill. Guide section 8 maps the app's `selection` field to `sand`, and the app's dark `selection` (#2F251C) is not a brand token.
- **Title width.** Guide: 112 to 125% width. The bundled Archivo files are the 125% cut only.
- **Status weight 500.** The app bundles DM Sans 400, 600 and 700, so 500 falls back to 400 there.

### Contrast notes (values kept as the guide sets them)

- Light `warn` dot on `cream`: 2.9:1, under the 3:1 floor for a meaningful mark. The word beside it carries the meaning.
- Light `paper` text on light `red`: 4.3:1, under 4.5:1 for 14px bold text. Dark `paper` (#2A211A) on dark `red`: 4.0:1. The guide does not say whether the dark destructive button keeps a light or dark label.
- Light `muted` on `sand-deep`: 4.4:1. Keep secondary text out of user messages.
- Resting input border (`hairline` on `cream`): 1.3:1. Inputs read as inputs by their label and `paper` fill until focus.

### Gaps in the guide

- No components for tool rows, approval cards, dialogs, menus, tabs, toggles, checkboxes, tooltips or toasts.
- No icon set.
- No guidance for the terminal UI beyond the desktop app.
- The avatar palette still includes earth tones that are not tokens (#B89A78, #F5A55F, #7FA6A0, #6F8F4E); the guide asks for stripe and status colors only.
