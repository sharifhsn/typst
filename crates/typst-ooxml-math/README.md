# typst-ooxml-math

Convert OOXML math (**OMML** — the `<m:oMath>` XML Microsoft Word uses for
equations) into **idiomatic** Typst math source.

This is the inverse of the OMML *generator* in `typst-docx`. Where the common
docx→typst path (pandoc) produces garbage math — e.g. `upright("𝑎")^(upright("2"))`
for `a^2`, because it treats Word's italic-unicode math letters literally — this
crate emits the clean Typst a user would actually write and keep:

| equation | this crate | pandoc |
|----------|------------|--------|
| `a^2`    | `a^2`      | `upright("𝑎")^(upright("2"))` |
| `α + β`  | `alpha + beta` | `upright("𝛼") + upright("𝛽")` |
| `√x`     | `sqrt(x)`  | (image / raw) |
| `∑_(i=1)^n x_i` | `sum_(i=1)^n x_i` | `sum_(...)` with upright junk |

## Library

```rust
let typst = typst_ooxml_math::omml_to_typst(omml_xml);
// place between $ … $
```

## CLI

```sh
omml2typst equation.xml       # read from a file
cat equation.xml | omml2typst # or from stdin
```

## Coverage

Runs/text (with correct upright-vs-italic recovery), fractions (`frac`,
`binom`), sub/superscripts (combined, pre-scripts → `attach`), radicals
(`sqrt`/`root`), n-ary operators (`sum`/`product`/`integral` with limits),
delimiters (→ parens/brackets/`abs`/`norm`/`floor`/`ceil`, else `lr`), matrices
(`mat`), equation arrays, accents (`hat`/`bar`/`dot`/`vec`/…), over/under bars
and braces, functions and `lim`, and a full Unicode→Typst symbol table (Greek,
operators, arrows, set theory) derived as the inverse of the `codex` symbol
table the compiler uses.

Unrecognised constructs degrade to a readable best-effort; the converter never
panics.
