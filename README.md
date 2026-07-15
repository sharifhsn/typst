<h1 align="center">
  <img alt="Typst" src="https://user-images.githubusercontent.com/17899797/226108480-722b770e-6313-40d7-84f2-26bebb55a281.png">
</h1>

<p align="center">
  <a href="https://typst.app/docs/">
    <img alt="Documentation" src="https://img.shields.io/website?down_message=offline&label=docs&up_color=007aff&up_message=online&url=https%3A%2F%2Ftypst.app%2Fdocs"
  ></a>
  <a href="https://typst.app/">
    <img alt="Typst App" src="https://img.shields.io/website?down_message=offline&label=typst.app&up_color=239dad&up_message=online&url=https%3A%2F%2Ftypst.app"
  ></a>
  <a href="https://discord.gg/2uDybryKPe">
    <img alt="Discord Server" src="https://img.shields.io/discord/1054443721975922748?color=5865F2&label=discord&labelColor=555"
  ></a>
  <a href="https://github.com/typst/typst/blob/main/LICENSE">
    <img alt="Apache-2 License" src="https://img.shields.io/badge/license-Apache%202-brightgreen"
  ></a>
  <a href="https://typst.app/jobs/">
    <img alt="Jobs at Typst" src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Ftypst.app%2Fassets%2Fdata%2Fshields.json&query=%24.jobs.text&label=jobs&color=%23A561FF&cacheSeconds=1800"
  ></a>
</p>

Typst is a new markup-based typesetting system that is designed to be as powerful
as LaTeX while being much easier to learn and use. Typst has:

- Built-in markup for the most common formatting tasks
- Flexible functions for everything else
- A tightly integrated scripting system
- Math typesetting, bibliography management, and more
- Fast compile times thanks to incremental compilation
- Friendly error messages in case something goes wrong

This repository contains the Typst compiler and its CLI, which is everything you
need to compile Typst documents locally. For the best writing experience,
consider signing up to our [collaborative online editor][app] for free.

> [!NOTE]
> **This is a community fork that adds native Microsoft Office export.**
> It compiles Typst straight to editable Office Open XML.
> **Word (`.docx`)** — real headings, styles, tables, OMML math, footnotes,
> cross-reference fields and lists, not a flattened image
> (`typst compile --format docx file.typ`).
> **PowerPoint (`.pptx`)** — one page per editable slide with live text, native
> shapes, gradients and images; best for slide-shaped decks
> (`typst compile deck.typ deck.pptx`).
> Both are **experimental previews** (like Typst's own HTML export); see
> **[Word export](#word-export-this-fork)** and
> **[PowerPoint export](#powerpoint-export-this-fork)** below for how to build
> them, what maps natively, and the honest limitations. A consolidated
> [Pandoc-AST target](crates/typst-pandoc/README.md) remains in the source tree,
> but it is known incomplete, is not a supported preview, and is not a release
> gate for this fork.
>
> This fork is **not affiliated with or endorsed by the Typst maintainers**, and
> the export code is not part of upstream Typst. The canonical development
> branch is `codex/office-export`; the latest release is `v0.15.0-office.2`,
> with a `v0.15.0-office.3` release in progress.

## Example
A [gentle introduction][tutorial] to Typst is available in our documentation.
However, if you want to see the power of Typst encapsulated in one image, here
it is:
<p align="center">
 <img alt="Example" width="900" src="https://user-images.githubusercontent.com/17899797/228031796-ced0e452-fcee-4ae9-92da-b9287764ff25.png">
</p>


Let's dissect what's going on:

- We use _set rules_ to configure element properties like the size of pages or
  the numbering of headings. By setting the page height to `auto`, it scales to
  fit the content. Set rules accommodate the most common configurations. If you
  need full control, you can also use [show rules][show] to completely redefine
  the appearance of an element.

- We insert a heading with the `= Heading` syntax. One equals sign creates a top
  level heading, two create a subheading and so on. Typst has more lightweight
  markup like this; see the [syntax] reference for a full list.

- [Mathematical equations][math] are enclosed in dollar signs. By adding extra
  spaces around the contents of an equation, we can put it into a separate block.
  Multi-letter identifiers are interpreted as Typst definitions and functions
  unless put into quotes. This way, we don't need backslashes for things like
  `floor` and `sqrt`. And `phi.alt` applies the `alt` modifier to the `phi` to
  select a particular symbol variant.

- Now, we get to some [scripting]. To input code into a Typst document, we can
  write a hash followed by an expression. We define two variables and a
  recursive function to compute the n-th fibonacci number. Then, we display the
  results in a center-aligned table. The table function takes its cells
  row-by-row. Therefore, we first pass the formulas `$F_1$` to `$F_8$` and then
  the computed fibonacci numbers. We apply the spreading operator (`..`) to both
  because they are arrays and we want to pass the arrays' items as individual
  arguments.

<details>
  <summary>Text version of the code example.</summary>

  ```typst
  #set page(width: 10cm, height: auto)
  #set heading(numbering: "1.")

  = Fibonacci sequence
  The Fibonacci sequence is defined through the
  recurrence relation $F_n = F_(n-1) + F_(n-2)$.
  It can also be expressed in _closed form:_

  $ F_n = round(1 / sqrt(5) phi.alt^n), quad
    phi.alt = (1 + sqrt(5)) / 2 $

  #let count = 8
  #let nums = range(1, count + 1)
  #let fib(n) = (
    if n <= 2 { 1 }
    else { fib(n - 1) + fib(n - 2) }
  )

  The first #count numbers of the sequence are:

  #align(center, table(
    columns: count,
    ..nums.map(n => $F_#n$),
    ..nums.map(n => str(fib(n))),
  ))
  ```
</details>

## Installation
Typst's CLI is available from different sources:

> [!IMPORTANT]
> The package-manager commands and upstream Typst releases below install the
> official compiler and **do not include this fork's DOCX, PPTX, or Pandoc
> exporters**. For Office export, use a binary from the
> [fork releases][fork-releases] or build this repository at the matching
> release tag or commit.

- You can get sources and pre-built binaries for the latest release of Typst
  from the [releases page][releases]. Download the archive for your platform and
  place it in a directory that is in your `PATH`. To stay up to date with future
  releases, you can simply run `typst update`.

- You can install Typst through different package managers. Note that the
  versions in the package managers might lag behind the latest release.
  - Linux:
      - View [Typst on Repology][repology]
      - View [Typst's Snap][snap]
  - macOS: `brew install typst`
  - Windows: `winget install --id Typst.Typst`

- If you have a [Rust][rust] toolchain installed, you can install
  - the latest released Typst version with
    `cargo install --locked typst-cli`
  - a development version with
    `cargo install --git https://github.com/typst/typst --locked typst-cli`

- Nix users can
  - use the `typst` package with `nix-shell -p typst`
  - build and run the [Typst flake](https://github.com/typst/typst-flake) with
    `nix run github:typst/typst-flake -- --version`.

- Docker users can run a prebuilt image with
  `docker run ghcr.io/typst/typst:latest --help`.

## Usage
Once you have installed Typst, you can use it like this:
```sh
# Creates `file.pdf` in working directory.
typst compile file.typ

# Creates a PDF file at the desired path.
typst compile path/to/source.typ path/to/output.pdf
```

You can also watch source files and automatically recompile on changes. This is
faster than compiling from scratch each time because Typst has incremental
compilation.
```sh
# Watches source files and recompiles on changes.
typst watch file.typ
```

Typst further allows you to add custom font paths for your project and list all
of the fonts it discovered:
```sh
# Adds additional directories to search for fonts.
typst compile --font-path path/to/fonts file.typ

# Lists all of the discovered fonts in the system and the given directory.
typst fonts --font-path path/to/fonts

# Or via environment variable (Linux syntax).
TYPST_FONT_PATHS=path/to/fonts typst fonts
```

For other CLI subcommands and options, see below:
```sh
# Prints available subcommands and options.
typst help

# Prints detailed usage of a subcommand.
typst help watch
```

If you prefer an integrated IDE-like experience with autocompletion and instant 
preview, you can also check out our [free web app][app]. Alternatively, there is 
a community-created language server called 
[Tinymist](https://myriad-dreamin.github.io/tinymist/) which is integrated into 
various editor extensions.

## Word export (this fork)
This fork adds a native Word exporter (`crates/typst-docx`). It walks Typst's
realized element tree under a dedicated `Docx` target and emits idiomatic Office
Open XML using Word's built-in styles, so the Navigation pane and Styles gallery
work. Representative fixtures are package-validated and have been exercised in
**Microsoft Word** and **LibreOffice**, but this preview cannot guarantee every
document will open without repair or reflow. Some
cross-references and numbering are emitted as live Word fields; updating fields
can change results when Word's semantics differ from Typst's.

Download a pre-built archive from the [fork releases][fork-releases], extract
it, and put the `typst` executable on your `PATH`. Release assets are built from
the tagged source and smoke-tested for DOCX export before upload. Install future
fork releases from the same page: these assets intentionally omit `typst
update`, whose current implementation downloads official upstream releases and
would replace the Office-capable binary.

To build the same source yourself, check out the release tag or commit you want
to use (requires a [Rust][rust] toolchain):

```sh
git clone https://github.com/sharifhsn/typst
cd typst
git checkout <release-tag-or-commit>
cargo build --release
# the binary is at target/release/typst
```

Then export to `.docx` either with an explicit format flag or a `.docx` output
extension:

```sh
target/release/typst compile --format docx document.typ
target/release/typst compile document.typ out.docx
```

**What maps natively:** paragraphs and inline formatting, `Heading N` styles with
numbering, bullet/numbered/nested lists, tables (borders, merged cells, shading),
block quotes, code blocks, links, `@ref` cross-references (as clickable fields),
footnotes, citations and bibliographies, a table of contents, OMML math
(fractions, matrices, big operators, aligned equations), page geometry and
sections, headers/footers, and PNG/JPEG images embedded verbatim. Decorative
vector shapes (`#rect`, `#line`, `#curve`, `#polygon`, gradients) map to native
DrawingML.

**What falls back to an embedded image:** graphics with no safe Word equivalent —
PDF/WebP images, CeTZ/fletcher diagrams, many transforms, and radial/conic
gradients. SVG images carry a native SVG part plus a PNG compatibility fallback.
Rasterized regions carry recovered text or descriptions where the exporter can
do so without presenting duplicate visible content to document consumers.

**Known limitations:** Word reflows native paragraphs and tables with its own
fonts and layout engine, so editability and pixel identity sometimes conflict.
Placed text, complex tables, page-varying furniture, and custom live numbering
remain fidelity-sensitive. Equations are now planned atomically: if one child
cannot be represented safely in OMML, the whole equation uses a rendered image
plus searchable text instead of emitting plausible-looking partial math.
The full per-feature support matrix and the rationale behind every mapping live
in [`crates/typst-docx/README.md`](crates/typst-docx/README.md) and
[`crates/typst-docx/COVERAGE.md`](crates/typst-docx/COVERAGE.md); a measured
comparison against typ2docx and pandoc is in [`COMPARISON.md`](COMPARISON.md).
The cross-target design, current architectural risks, and validation model are
in [`docs/dev/office-export-architecture.md`](docs/dev/office-export-architecture.md).

**Fidelity-reporting contract:** the CLI always embeds the versioned DOCX
fidelity manifest at `customXml/typstFidelity.xml` and mirrors it into
`docProps/custom.xml`. It records native, approximate, rasterized, and dropped
representations; dynamic fields; fonts; drawing labels; and suppressed
diagnostics. This is exporter provenance, not proof that Microsoft Word or
LibreOffice verified the result. Library callers always receive the same
queryable [`FidelityReport`](crates/typst-docx/src/report.rs) on `DocxDocument`,
but package embedding is deliberately opt-in through
`DocxOptions::embed_fidelity_manifest` because the payload can expose details
about the authoring environment. PPTX does not yet embed an equivalent manifest;
its package structure and CLI warnings are the current machine-visible evidence.

**Accessibility status:** the exporter emits native headings, lists, tables,
language/direction metadata, image descriptions, and explicit decorative-art
markers, and its fidelity manifest reports unlabeled drawings. Those structural
features are useful to assistive technology, but the exporter is **not yet
certified or exhaustively tested for accessibility**. Before distributing an
accessible document, run Microsoft Word's Accessibility Checker, add any
missing alternative text, verify reading order, and test the document with the
screen reader used by its audience. A successful package validation or a clean
open in Word is not an accessibility conformance result.

For identical inputs, toolchain, fonts, and environment, output is byte-for-byte
reproducible under `SOURCE_DATE_EPOCH`. This is preview
software: please report anything that opens wrong or looks off.

## PowerPoint export (this fork)
The same fork also exports **PowerPoint** presentations (`crates/typst-pptx`) —
**one Typst page per editable slide**. Where the Word exporter reflows semantic
structure, the PowerPoint exporter takes the already laid-out page as its
geometric source (it's a sibling of the PNG/SVG renderers). Native shapes and
pictures preserve those coordinates; editable text is reconstructed into
DrawingML text boxes and can reflow under PowerPoint/Impress font substitution.

```sh
target/release/typst compile deck.typ deck.pptx
target/release/typst compile --format pptx deck.typ
```

It's built for **slide-shaped documents** — decks made with
[Touying](https://touying-typ.github.io/) or [Polylux](https://polylux.dev/), or
any `#set page` in a 16:9 / 16:10 / 4:3 ratio. (Export a page-shaped document to
`.pptx` and the CLI nudges you toward `.docx`, and the reverse.)

**What maps natively:** live editable text runs, links, straight connectors,
vector shapes with solid/gradient/translucent or tiled fills, slide backgrounds,
PNG/JPEG images, SVG with a PNG fallback, DrawingML tables, native OMML math,
slide-number/title/body placeholders, and Touying/pdfpc speaker notes. Complex
clips, skew/non-uniform transforms, PDF art, CeTZ diagrams, and radial/conic
gradients can still fall back to positioned pictures.

PowerPoint has one global slide size. Mixed-size Typst pages currently produce a
warning and are uniformly scaled to fit and centered on the first page's canvas;
this preserves content but can introduce letterboxing.

**Dated fidelity snapshot:** in the 2026-07-03 comparison, across 112 real
presentation templates, PPTX-vs-PDF visual similarity averaged **0.995**
(median 0.996) with no export failures. That run predates native tables, OMML,
SVG fallback, placeholders, and notes and is not a measurement of the current
feature set.
The per-feature notes and honest limitations are in
[`crates/typst-pptx/README.md`](crates/typst-pptx/README.md), and a measured
head-to-head against the existing conversion tools (typ2pptx, typ2docx,
touying-exporter, pandoc) is in [`COMPARISON.md`](COMPARISON.md).

## Pandoc export (this fork)

> [!WARNING]
> **Known incomplete and unsupported.** This target remains available for
> development and historical compatibility, but it is not part of the supported
> DOCX/PPTX preview and should be treated as broken for general use.

The fork also exports a typed Pandoc JSON AST. Unlike Pandoc's syntax-only Typst
reader, this path evaluates packages and Typst code before lowering the realized
document.

```sh
target/release/typst compile document.typ document.pandoc
target/release/typst compile --format pandoc document.typ
```

Headings, paragraphs, lists, tables, links, footnotes, code, figures, math, and
citations map to native Pandoc nodes. A document bibliography also produces a
BibLaTeX sidecar so `pandoc --citeproc` can re-resolve structured citations.
Visual-only content uses self-contained image fallbacks. See
[`crates/typst-pandoc/README.md`](crates/typst-pandoc/README.md) for current
limitations.

## Community
The main places where the community gathers are our [Forum][forum] and our
[Discord server][discord]. The Forum is a great place to ask questions, help
others, and share cool things you created with Typst. The Discord server is more
suitable for quicker questions, discussions about contributing, or just to chat.
We'd be happy to see you there!

[Typst Universe][universe] is where the community shares templates and packages.
If you want to share your own creations, you can submit them to our
[package repository][packages].

If you had a bad experience in our community, please [reach out to us][contact].

## Contributing
We love to see contributions from the community. If you experience bugs, feel
free to open an issue. If you would like to implement a new feature or bug fix,
please follow the steps outlined in the [contribution guide][contributing].

To build Typst yourself, first ensure that you have the
[latest stable Rust][rust] installed. Then, clone this repository and build the
CLI with the following commands:

```sh
git clone https://github.com/typst/typst
cd typst
cargo build --release
```

The optimized binary will be stored in `target/release/`.

Another good way to contribute is by [sharing packages][packages] with the
community.

## Pronunciation and Spelling
IPA: /taɪpst/. "Ty" like in **Ty**pesetting and "pst" like in Hi**pst**er. When
writing about Typst, capitalize its name as a proper noun, with a capital "T".

## Design Principles
All of Typst has been designed with three key goals in mind: Power,
simplicity, and performance. We think it's time for a system that matches the
power of LaTeX, is easy to learn and use, all while being fast enough to realize
instant preview. To achieve these goals, we follow three core design principles:

- **Simplicity through Consistency:**
  If you know how to do one thing in Typst, you should be able to transfer that
  knowledge to other things. If there are multiple ways to do the same thing,
  one of them should be at a different level of abstraction than the other. E.g.
  it's okay that `= Introduction` and `#heading[Introduction]` do the same thing
  because the former is just syntax sugar for the latter.

- **Power through Composability:**
  There are two ways to make something flexible: Have a knob for everything or
  have a few knobs that you can combine in many ways. Typst is designed with the
  second way in mind. We provide systems that you can compose in ways we've
  never even thought of. TeX is also in the second category, but it's a bit
  low-level and therefore people use LaTeX instead. But there, we don't really
  have that much composability. Instead, there's a package for everything
  (`\usepackage{knob}`).

- **Performance through Incrementality:**
  All Typst language features must accommodate for incremental compilation.
  Luckily we have [`comemo`], a system for incremental compilation which does
  most of the hard work in the background.

## Acknowledgements

We'd like to thank everyone who is supporting Typst's development, be it via
[GitHub sponsors] or elsewhere. In particular, special thanks[^1] go to:

- [Posit](https://posit.co/blog/posit-and-typst/) for financing a full-time
  compiler engineer
- [NLnet](https://nlnet.nl/) for supporting work on Typst via multiple grants
  through the [NGI Zero Core](https://nlnet.nl/core) fund:
  - Work on [HTML export](https://nlnet.nl/project/Typst-HTML/)
  - Work on [PDF accessibility](https://nlnet.nl/project/Typst-Accessibility/)
- [Science & Startups](https://www.science-startups.berlin/) for having financed
  Typst development from January through June 2023 via the Berlin Startup
  Scholarship
- [Zerodha](https://zerodha.tech/blog/1-5-million-pdfs-in-25-minutes/) for their
  generous one-time sponsorship

[^1]: This list only includes contributions for our open-source work that exceed
    or are expected to exceed €10K.

[docs]: https://typst.app/docs/
[app]: https://typst.app/
[discord]: https://discord.gg/2uDybryKPe
[forum]: https://forum.typst.app/
[universe]: https://typst.app/universe/
[tutorial]: https://typst.app/docs/tutorial/
[show]: https://typst.app/docs/reference/styling/#show-rules
[math]: https://typst.app/docs/reference/math/
[syntax]: https://typst.app/docs/reference/syntax/
[scripting]: https://typst.app/docs/reference/scripting/
[rust]: https://rustup.rs/
[releases]: https://github.com/typst/typst/releases/
[fork-releases]: https://github.com/sharifhsn/typst/releases/
[repology]: https://repology.org/project/typst/versions
[contact]: https://typst.app/contact
[architecture]: https://github.com/typst/typst/blob/main/docs/dev/architecture.md
[contributing]: https://github.com/typst/typst/blob/main/CONTRIBUTING.md
[packages]: https://github.com/typst/packages/
[`comemo`]: https://github.com/typst/comemo/
[snap]: https://snapcraft.io/typst
[GitHub sponsors]: https://github.com/sponsors/typst/
