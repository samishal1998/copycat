# Copycat — Brand Direction

**Selected concept:** Copycat Variant 4 — premium modern monogram.

This package preserves all four Copycat concept boards, but Variant 4 is the working direction. Variant 2 is the strongest alternate reference.

## Positioning

Copycat is not primarily a clipboard-history browser. It is a **programmable clipboard execution layer**: deterministic stack, queue, group, and indexed paste semantics driven by a daemon and keyboard commands.

The brand should therefore feel:

- precise rather than whimsical;
- fast without looking disposable;
- developer-native without looking like a terminal-only tool;
- local/private by default;
- expressive enough to become a polished desktop product later.

## Selected mark

The Variant 4 mark combines:

1. a cat silhouette shaped as an open **C**;
2. layered sheets in the center, representing stack/history;
3. the open right edge, suggesting flow and movement rather than passive storage.

The included SVGs are **production-oriented reconstructions** of the generated concept, not exact vector traces of the raster concept board. They are intended as editable starting masters.

## Palette

| Token | Hex | Use |
|---|---:|---|
| Burnt orange | `#E4672B` | Primary brand / active state |
| Charcoal | `#1C1F23` | Main dark surface |
| Slate | `#6B6F76` | Secondary text / inactive layers |
| White | `#F7F7F7` | Primary text / top clipboard layer |
| Soft gold | `#D2A84A` | Optional premium accent, sparingly |

Recommended application UI should use orange for active semantic state and avoid turning every control orange.

## Typography

**Primary direction:** Inter.

Use Inter for product UI and branding. Do not bundle font files in this asset pack; rely on the system, a licensed web delivery path, or the application's normal font packaging process.

For terminal/TUI surfaces, use the user's monospace font. The TUI should not force a custom font.

## Wordmark and tagline

Working wordmark: **Copycat**

Recommended product tagline:

> Copy. Stack. Paste with intent.

The original Variant 4 board uses “Automate. Orchestrate. Copy.” That language is visually good but overstates orchestration relative to the actual product. Keep it only as concept-board history.

## Icon usage

- `copycat-mark.svg`: standalone mark for docs, small badges, splash screens.
- `copycat-app-icon.svg`: dark rounded application icon composition.
- `copycat-lockup.svg`: mark + wordmark + tagline.
- raster PNGs are convenience exports.
- concept crops are visual references only.

## UI language

Prefer names that describe behavior exactly:

- Stack
- Queue
- Group
- Paste by index
- Preserve duplicates
- Collapse duplicates
- Capture from now
- Last N

Avoid generic productivity language where a deterministic systems term exists.

## Naming risk

**Copycat is currently a working name, not a cleared product name.** There are already clipboard products and developer tools using “Copycat,” including existing clipboard-manager applications and a cross-platform clipboard CLI. Before public release, perform trademark, domain, package-name, app-store, and repository-name clearance. The architecture and visual system are intentionally separable from the final product name.
