# Embedded Ubuntu faces

These are the static Ubuntu TrueType faces used by the invoice PDF renderer
(`src/pdf/fonts.rs`, embedded with `include_bytes!`):

- `Ubuntu-Regular.ttf`
- `Ubuntu-Bold.ttf`
- `Ubuntu-Italic.ttf`
- `Ubuntu-BoldItalic.ttf`

Source: <https://github.com/google/fonts/tree/main/ufl/ubuntu> (Canonical's
Ubuntu Font Family). The faces are shipped unmodified and embed under the
Ubuntu Font Licence; `UFL.txt` is the licence text. The PDF uses them as simple
TrueType fonts with `/WinAnsiEncoding`, covering the Latin-1/WinAnsi characters
the document model can produce.

To refresh a face, replace the file from that source and re-run
`cargo test --lib pdf::` (the font metrics and embedding are exercised there).
