# Mathnote font fixture

Retained fonts used by the frozen `alex222222222222-mathnote` corpus document.
Pass this directory with `--font-path` for both the reference PDF and Office
export so the harness measures the same glyph metrics.

- `Iosevka-Regular.ttf`: Iosevka 34.7.0 Regular from the official release,
  licensed under the SIL Open Font License in `LICENSE-Iosevka.md`.
- `FiraMono-Regular.ttf`: Fira Mono Regular from Google Fonts, licensed under
  the SIL Open Font License in `OFL-FiraMono.txt`.
- `Cambria.ttc` (optional, gitignored): copied from the local Microsoft Word
  installation for consumer-authority QA. It is intentionally excluded from
  version control and must not be redistributed.

The source theme requests `("Iosevka", "Fira Mono")`. Retaining both avoids
relying on workstation font state and gives the PDF and Office renderers the
same glyph metrics.
