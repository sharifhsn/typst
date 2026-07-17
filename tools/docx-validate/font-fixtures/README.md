# Public-corpus font fixtures

Each subdirectory is named after a frozen public-corpus document's `name`.
`corpus.py` discovers that directory automatically and passes it to every PDF,
DOCX, and DOCX-review compile with `--font-path`. LibreOffice visual checks also
receive it through `SAL_FONTPATH`.

Keep these fixtures small, redistributable, pinned, and accompanied by their
upstream license. The corpus result records a SHA-256 digest and file list so a
font-dependent authority can be reproduced exactly.
