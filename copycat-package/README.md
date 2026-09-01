# Copycat Design & Product Package

This bundle captures the product direction discussed for the programmable clipboard daemon.

## Selected direction

- **Working name:** Copycat
- **Brand:** Variant 4
- **Core:** Rust
- **Interfaces first:** daemon + CLI + Ratatui TUI
- **Platforms:** macOS, Windows, Linux
- **GUI later:** evaluate GPUI first; Tauri 2 fallback

## Files

```text
COPYCAT_PRD_ADR.md             Full PRD + architecture decisions

docs/BRAND.md                 Brand specification and naming caveat
docs/CLI_SPEC.md              Proposed CLI/action language
docs/STATE_MACHINE.md          Core ordering/duplicate semantics
docs/config.example.toml       Programmable leader/hotkey example
docs/RESEARCH_NOTES.md         Verified technical references

brand/selected/                Variant 4 working assets
brand/concepts/                All four Copycat concept boards
```

## Asset note

The selected concept board is the original generated raster design. The SVG mark/icon/lockup are editable vector reconstructions intended as implementation starting points; they are not exact automated traces of the concept artwork.

## Naming note

“Copycat” is a working name. Existing clipboard products and a clipboard CLI already use the name, so public release requires name/trademark/package/domain clearance.
