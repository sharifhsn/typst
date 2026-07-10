use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Datelike, Timelike, Utc};
use ecow::{EcoVec, eco_format, eco_vec};
use parking_lot::RwLock;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use typst::diag::{
    At, HintedStrResult, HintedString, SourceDiagnostic, SourceResult, StrResult, Warned,
    bail,
};
use typst::foundations::{Datetime, Smart};
use typst::layout::{Abs, PageRanges};
use typst::model::Document;
use typst::syntax::Span;
use typst_bundle::{Bundle, BundleOptions, VirtualFs};
use typst_docx::{DocxDocument, DocxOptions};
use typst_html::{HtmlDocument, HtmlOptions};
use typst_kit::diagnostics::DiagnosticWorld;
use typst_kit::timer::Timer;
use typst_layout::{Page, PagedDocument};
use typst_pandoc::{PandocDocument, PandocOptions};
use typst_pdf::{PdfOptions, PdfStandards, Timestamp};
use typst_pptx::{PptxOptions, SpeakerNote};
use typst_render::RenderOptions;
use typst_svg::SvgOptions;
use typst_utils::Scalar;

use crate::args::{
    CompileArgs, CompileCommand, DepsFormat, DiagnosticFormat, Input, Output,
    OutputFormat, PdfStandard, WatchCommand,
};
use crate::deps::write_deps;
use crate::watch::Status;
use crate::world::SystemWorld;
use crate::{set_failed, terminal};

#[cfg(feature = "http-server")]
use typst_kit::server::HttpServer;

/// Execute a compilation command.
pub fn compile(command: &'static CompileCommand) -> HintedStrResult<()> {
    let mut timer = Timer::new_or_placeholder(command.args.timings.clone());
    let mut config = CompileConfig::new(command)?;
    let mut world = SystemWorld::new(
        Some(&command.args.input),
        &command.args.world,
        &command.args.process,
    )
    .map_err(|err| eco_format!("{err}"))?;
    timer.record(&mut world, |world| compile_once(world, &mut config))?
}

/// A preprocessed `CompileCommand`.
pub struct CompileConfig {
    /// Static warnings to emit after compilation.
    pub warnings: Vec<HintedString>,
    /// Whether we are watching.
    pub watching: bool,
    /// Path to input Typst file or stdin.
    pub input: Input,
    /// Path to output file (PDF, PNG, SVG, or HTML).
    pub output: Output,
    /// The format of the output file.
    pub output_format: OutputFormat,
    /// Whether to make the serialized document pretty.
    pub pretty: bool,
    /// Which pages to export.
    pub pages: Option<PageRanges>,
    /// The document's creation date formatted as a UNIX timestamp, with UTC suffix.
    pub creation_timestamp: Option<DateTime<Utc>>,
    /// The format to emit diagnostics in.
    pub diagnostic_format: DiagnosticFormat,
    /// Opens the output file with the default viewer or a specific program after
    /// compilation.
    pub open: Option<Option<String>>,
    /// A list of standards the PDF should conform to.
    pub pdf_standards: PdfStandards,
    /// Whether to write PDF (accessibility) tags.
    pub tagged: bool,
    /// A destination to write a list of dependencies to.
    pub deps: Option<Output>,
    /// The format to use for dependencies.
    pub deps_format: DepsFormat,
    /// The PPI (pixels per inch) to use for PNG export.
    pub ppi: f64,
    /// The export cache for images, used for caching output files in `typst
    /// watch` sessions with images.
    pub export_cache: ExportCache,
    /// Server for `typst watch` to HTML.
    #[cfg(feature = "http-server")]
    pub server: Option<HttpServer>,
}

impl CompileConfig {
    /// Preprocess a `CompileCommand`, producing a compilation config.
    pub fn new(command: &CompileCommand) -> HintedStrResult<Self> {
        Self::new_impl(&command.args, None)
    }

    /// Preprocess a `WatchCommand`, producing a compilation config.
    pub fn watching(command: &WatchCommand) -> HintedStrResult<Self> {
        Self::new_impl(&command.args, Some(command))
    }

    /// The shared implementation of [`CompileConfig::new`] and
    /// [`CompileConfig::watching`].
    fn new_impl(
        args: &CompileArgs,
        watch: Option<&WatchCommand>,
    ) -> HintedStrResult<Self> {
        let mut warnings = Vec::new();
        let input = args.input.clone();

        let output_format = if let Some(specified) = args.format {
            specified
        } else if let Some(Output::Path(output)) = &args.output {
            match output.extension() {
                Some(ext) if ext.eq_ignore_ascii_case("pdf") => OutputFormat::Pdf,
                Some(ext) if ext.eq_ignore_ascii_case("png") => OutputFormat::Png,
                Some(ext) if ext.eq_ignore_ascii_case("svg") => OutputFormat::Svg,
                Some(ext) if ext.eq_ignore_ascii_case("html") => OutputFormat::Html,
                Some(ext) if ext.eq_ignore_ascii_case("docx") => OutputFormat::Docx,
                Some(ext) if ext.eq_ignore_ascii_case("pandoc") => OutputFormat::Pandoc,
                Some(ext) if ext.eq_ignore_ascii_case("pptx") => OutputFormat::Pptx,
                _ => bail!(
                    "could not infer output format for path {}.\n\
                     consider providing the format manually with `--format/-f`",
                    output.display(),
                ),
            }
        } else {
            OutputFormat::Pdf
        };

        let output = args.output.clone().unwrap_or_else(|| {
            let Input::Path(path) = &input else {
                panic!("output must be specified when input is from stdin, as guarded by the CLI");
            };
            Output::Path(path.with_extension(
                match output_format {
                    OutputFormat::Pdf => "pdf",
                    OutputFormat::Png => "png",
                    OutputFormat::Svg => "svg",
                    OutputFormat::Html => "html",
                    OutputFormat::Docx => "docx",
                    OutputFormat::Pandoc => "pandoc",
                    OutputFormat::Pptx => "pptx",
                    OutputFormat::Bundle => "",
                },
            ))
        });

        let pages = args.pages.as_ref().map(|export_ranges| {
            PageRanges::new(export_ranges.iter().map(|r| r.0.clone()).collect())
        });

        if output_format == OutputFormat::Docx && pages.is_some() {
            // Refuse rather than silently export the whole document: a user
            // selecting pages expects the rest to be absent from the output.
            bail!(
                "--pages is not supported for DOCX export";
                hint: "a Word document flows continuously and has no fixed pages to select from";
            );
        }

        let tagged = !args.no_pdf_tags && pages.is_none();
        if output_format == OutputFormat::Pdf && pages.is_some() && !args.no_pdf_tags {
            warnings.push(
                HintedString::from("using --pages implies --no-pdf-tags").with_hints([
                    "the resulting PDF will be inaccessible".into(),
                    "add --no-pdf-tags to silence this warning".into(),
                ]),
            );
        }

        if !tagged {
            const ACCESSIBLE: &[(PdfStandard, &str)] = &[
                (PdfStandard::A_1a, "PDF/A-1a"),
                (PdfStandard::A_2a, "PDF/A-2a"),
                (PdfStandard::A_3a, "PDF/A-3a"),
                (PdfStandard::UA_1, "PDF/UA-1"),
            ];

            for (standard, name) in ACCESSIBLE {
                if args.pdf_standard.contains(standard) {
                    if args.no_pdf_tags {
                        bail!("cannot disable PDF tags when exporting a {name} document");
                    } else {
                        bail!(
                            "cannot disable PDF tags when exporting a {name} document";
                            hint: "using --pages implies --no-pdf-tags";
                        );
                    }
                }
            }
        }

        let pdf_standards = PdfStandards::new(
            &args.pdf_standard.iter().copied().map(Into::into).collect::<Vec<_>>(),
        )?;

        #[cfg(feature = "http-server")]
        let server = if let Some(command) = watch
            && !command.server.no_serve
            && matches!(output_format, OutputFormat::Html | OutputFormat::Bundle)
        {
            Some(HttpServer::new(
                &eco_format!("{input}"),
                command.server.port,
                !command.server.no_reload,
            )?)
        } else {
            None
        };

        let mut deps = args.deps.clone();
        let mut deps_format = args.deps_format;

        if let Some(path) = &args.make_deps
            && deps.is_none()
        {
            deps = Some(Output::Path(path.clone()));
            deps_format = DepsFormat::Make;
            warnings.push(
                "--make-deps is deprecated, use --deps and --deps-format instead".into(),
            );
        }

        match (&output, &deps, watch) {
            (Output::Stdout, _, Some(_)) => {
                bail!("cannot write document to stdout in watch mode");
            }
            (_, Some(Output::Stdout), Some(_)) => {
                bail!("cannot write dependencies to stdout in watch mode")
            }
            (Output::Stdout, Some(Output::Stdout), _) => {
                bail!("cannot write both output and dependencies to stdout")
            }
            _ => {}
        }

        Ok(Self {
            warnings,
            watching: watch.is_some(),
            input,
            output,
            output_format,
            pretty: args.pretty,
            pages,
            pdf_standards,
            tagged,
            creation_timestamp: args
                .world
                .creation_timestamp
                .map(|time| {
                    chrono::DateTime::from_timestamp(time, 0)
                        .ok_or("creation timestamp is out of range")
                })
                .transpose()?,
            ppi: args.ppi,
            diagnostic_format: args.process.diagnostic_format,
            open: args.open.clone(),
            export_cache: ExportCache::new(),
            deps,
            deps_format,
            #[cfg(feature = "http-server")]
            server,
        })
    }
}

/// Compile a single time.
///
/// Returns whether it compiled without errors.
#[typst_macros::time(name = "compile once")]
pub fn compile_once(
    world: &mut SystemWorld,
    config: &mut CompileConfig,
) -> HintedStrResult<()> {
    let start = std::time::Instant::now();
    if config.watching {
        Status::Compiling.print(config).unwrap();
    }

    let Warned { output, mut warnings } = compile_and_export(world, config);

    // Add static warnings (for deprecated CLI flags and such).
    for warning in config.warnings.iter() {
        warnings.push(
            SourceDiagnostic::warning(Span::detached(), warning.message())
                .with_hints(warning.hints().iter().map(Into::into)),
        );
    }

    match &output {
        // Print success message and possibly warnings.
        Ok(_) => {
            let duration = start.elapsed();
            if config.watching {
                if warnings.is_empty() {
                    Status::Success(duration).print(config).unwrap();
                } else {
                    Status::PartialSuccess(duration).print(config).unwrap();
                }
            }

            print_diagnostics(world, &[], &warnings, config.diagnostic_format)
                .map_err(|err| eco_format!("failed to print diagnostics ({err})"))?;

            open_output(config)?;
        }

        // Print failure message and diagnostics.
        Err(errors) => {
            set_failed();

            if config.watching {
                Status::Error.print(config).unwrap();
            }

            print_diagnostics(world, errors, &warnings, config.diagnostic_format)
                .map_err(|err| eco_format!("failed to print diagnostics ({err})"))?;
        }
    }

    if let Some(dest) = &config.deps {
        write_deps(world, dest, config.deps_format, output.as_deref().ok())
            .map_err(|err| eco_format!("failed to create dependency file ({err})"))?;
    }

    Ok(())
}

/// Compile and then export the document.
fn compile_and_export(
    world: &mut SystemWorld,
    config: &mut CompileConfig,
) -> Warned<SourceResult<Vec<Output>>> {
    match config.output_format {
        OutputFormat::Pdf
        | OutputFormat::Png
        | OutputFormat::Svg
        | OutputFormat::Pptx => {
            let Warned { output, mut warnings } = typst::compile::<PagedDocument>(world);
            let result = match output {
                Ok(document) => {
                    if let Some(page) = document.pages().first() {
                        let size = page.frame.size();
                        if let Some(warning) = target_mismatch_warning(
                            config.output_format,
                            (size.x.to_pt(), size.y.to_pt()),
                        ) {
                            warnings.push(warning);
                        }
                    }
                    if let Some(warning) = mixed_page_size_warning(&document, config) {
                        warnings.push(warning);
                    }
                    export_paged(&document, config)
                }
                Err(errors) => Err(errors),
            };
            Warned { output: result, warnings }
        }
        OutputFormat::Html => {
            let Warned { output, warnings } = typst::compile::<HtmlDocument>(world);
            let result = output.and_then(|document| export_html(&document, config));
            Warned {
                output: result.map(|()| vec![config.output.clone()]),
                warnings,
            }
        }
        OutputFormat::Bundle => {
            let Warned { output, warnings } = typst::compile::<Bundle>(world);
            let result = output.and_then(|bundle| export_bundle(bundle, config));
            Warned { output: result, warnings }
        }
        OutputFormat::Docx => {
            let Warned { output: paged, mut warnings } =
                typst::compile::<PagedDocument>(world);
            let result = match paged {
                Ok(paged_document) => {
                    let primary = Arc::clone(paged_document.introspector());
                    let seed = Arc::clone(&primary);
                    // Each real page's true frame size, so an `auto` page axis
                    // (a common ticket/certificate/single-page-diagram idiom)
                    // resolves to Typst's own content-driven size rather than a
                    // hardcoded A4 fallback (see `real_section_size` in
                    // typst-docx).
                    let page_sizes = Arc::new(
                        paged_document
                            .pages()
                            .iter()
                            .map(|page| page.frame.size())
                            .collect::<Vec<_>>(),
                    );
                    let paged_geometry = Arc::new(
                        typst_export_common::paged::PagedGeometry::from_document(
                            &paged_document,
                        ),
                    );
                    let Warned { output, warnings: docx_warnings } =
                        typst::compile_with::<DocxDocument, _>(
                            world,
                            Some(seed.as_ref()),
                            move |engine, content, styles| {
                                typst_docx::docx_document_with_paged_geometry(
                                    engine,
                                    content,
                                    styles,
                                    Arc::clone(&primary),
                                    Arc::clone(&page_sizes),
                                    Arc::clone(&paged_geometry),
                                )
                            },
                        );
                    warnings.extend(docx_warnings);
                    match output {
                        Ok(document) => {
                            if let Some(warning) = target_mismatch_warning(
                                config.output_format,
                                document.page_size_pt(),
                            ) {
                                warnings.push(warning);
                            }
                            export_docx(&document, config)
                                .map(|()| vec![config.output.clone()])
                        }
                        Err(errors) => Err(errors),
                    }
                }
                Err(errors) => Err(errors),
            };
            Warned { output: result, warnings }
        }
        OutputFormat::Pandoc => {
            let Warned { output, warnings } = typst::compile::<PandocDocument>(world);
            let result = output.and_then(|document| export_pandoc(&document, config));
            Warned { output: result, warnings }
        }
    }
}

/// A soft advisory when the output container looks like a mismatch for the
/// document's page proportions: slide-shaped pages written to `.docx`, or a
/// page-shaped document written to `.pptx`. Returns `None` for any other
/// format, or when the proportions already suit the chosen format.
fn target_mismatch_warning(
    format: OutputFormat,
    (width, height): (f64, f64),
) -> Option<SourceDiagnostic> {
    // Landscape with a common slide aspect ratio (16:9, 16:10, 4:3, or cinema)
    // reads as a deck. A4-landscape (√2 ≈ 1.414) matches none of these and is
    // deliberately treated as a document — it is the usual flyer/handout shape.
    let slide_shaped = width > height && {
        let ratio = width / height;
        [16.0 / 9.0, 16.0 / 10.0, 4.0 / 3.0, 2.35]
            .iter()
            .any(|target| (ratio - target).abs() < 0.05)
    };
    let (message, hint) = match format {
        OutputFormat::Docx if slide_shaped => (
            "the document has slide-shaped pages but is being exported to DOCX",
            "export to a .pptx file for one editable slide per page",
        ),
        OutputFormat::Pptx if !slide_shaped => (
            "the document has page-shaped proportions but is being exported to PPTX",
            "export to a .docx file for a reflowable Word document",
        ),
        _ => return None,
    };
    Some(SourceDiagnostic::warning(Span::detached(), message).with_hint(hint))
}

/// PPTX has a single global slide size, so a deck whose exported pages differ
/// in size cannot be represented without an explicit per-page transform. Warn
/// when that happens; `None` for any other format or a uniform deck.
fn mixed_page_size_warning(
    document: &PagedDocument,
    config: &CompileConfig,
) -> Option<SourceDiagnostic> {
    if config.output_format != OutputFormat::Pptx {
        return None;
    }
    let mut sizes = document
        .pages()
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            config
                .pages
                .as_ref()
                .is_none_or(|ranges| ranges.includes_page_index(*i))
        })
        .map(|(_, page)| page.frame.size());
    let first = sizes.next()?;
    let uniform = sizes.all(|size| {
        (size.x - first.x).abs() < Abs::pt(0.5) && (size.y - first.y).abs() < Abs::pt(0.5)
    });
    (!uniform).then(|| {
        SourceDiagnostic::warning(
            Span::detached(),
            "the presentation mixes pages of different sizes",
        )
        .with_hint(
            "every slide uses the first page's canvas; off-size content may crop or leave extra space",
        )
    })
}

/// Export to DOCX.
fn export_docx(document: &DocxDocument, config: &CompileConfig) -> SourceResult<()> {
    let options = DocxOptions { pretty: config.pretty };
    let bytes = typst_docx::docx(document, &options)?;
    config
        .output
        .write(&bytes)
        .map_err(|err| eco_format!("failed to write DOCX file ({err})"))
        .at(Span::detached())
}

/// Export to a Pandoc JSON AST.
///
/// When the document has a bibliography, also synthesizes a BibLaTeX `.bib`
/// sidecar (via hayagriva's `to_biblatex_str`) next to the JSON output and
/// records its filename in the document's `meta.bibliography`, so that
/// `pandoc --citeproc` can re-resolve the structured `Cite` nodes the exporter
/// emits. Returns every file written (the JSON, plus the sidecar if any) so the
/// dependency tracker sees them.
fn export_pandoc(
    document: &PandocDocument,
    config: &CompileConfig,
) -> SourceResult<Vec<Output>> {
    let mut written = Vec::with_capacity(2);

    // If there is a bibliography and we are writing to a real path (not stdout),
    // write the `.bib` sidecar next to the output and reference it in the
    // metadata. With stdout (or no bibliography) we skip the sidecar, but still
    // emit structured `Cite` + the self-contained fallback references, so the
    // JSON is correct without citeproc either way.
    let bib_meta = match (document.bibliography(), &config.output) {
        (Some(bib), Output::Path(out_path)) => {
            let bib_path = out_path.with_extension("bib");
            std::fs::write(&bib_path, bib.as_bytes())
                .map_err(|err| {
                    eco_format!("failed to write bibliography sidecar ({err})")
                })
                .at(Span::detached())?;
            written.push(Output::Path(bib_path.clone()));
            // Record the sidecar's *filename* (relative to the JSON output) in the
            // metadata, so the reference is portable: pandoc resolves a relative
            // `bibliography` path against its working directory, and the common
            // case runs pandoc from the output directory.
            bib_path.file_name().map(|n| n.to_string_lossy().into_owned())
        }
        _ => None,
    };

    let options = PandocOptions { pretty: config.pretty, bibliography: bib_meta };
    let bytes = typst_pandoc::pandoc(document, &options)?;
    config
        .output
        .write(&bytes)
        .map_err(|err| eco_format!("failed to write Pandoc file ({err})"))
        .at(Span::detached())?;
    written.push(config.output.clone());

    Ok(written)
}

/// Export to HTML.
fn export_html(document: &HtmlDocument, config: &CompileConfig) -> SourceResult<()> {
    let options = HtmlOptions { pretty: config.pretty };
    let html = typst_html::html(document, &options)?;
    let result = config.output.write(html.as_bytes());

    #[cfg(feature = "http-server")]
    if let Some(server) = &config.server {
        server.set_html(html);
    }

    result
        .map_err(|err| eco_format!("failed to write HTML file ({err})"))
        .at(Span::detached())
}

/// Export to a paged target format.
fn export_paged(
    document: &PagedDocument,
    config: &CompileConfig,
) -> SourceResult<Vec<Output>> {
    match config.output_format {
        OutputFormat::Pdf => {
            export_pdf(document, config).map(|()| vec![config.output.clone()])
        }
        OutputFormat::Png => {
            export_image(document, config, ImageExportFormat::Png).at(Span::detached())
        }
        OutputFormat::Svg => {
            export_image(document, config, ImageExportFormat::Svg).at(Span::detached())
        }
        OutputFormat::Pptx => {
            export_pptx(document, config).map(|()| vec![config.output.clone()])
        }
        OutputFormat::Html
        | OutputFormat::Bundle
        | OutputFormat::Docx
        | OutputFormat::Pandoc => unreachable!(),
    }
}

/// Export to a PPTX.
fn export_pptx(document: &PagedDocument, config: &CompileConfig) -> SourceResult<()> {
    let mut exported_pages = EcoVec::new();
    let mut page_to_slide = vec![None; document.pages().len()];
    for (i, page) in document.pages().iter().enumerate() {
        if config.pages.as_ref().is_none_or(|exported_page_ranges| {
            exported_page_ranges.includes_page_index(i)
        }) {
            page_to_slide[i] = Some(exported_pages.len());
            exported_pages.push(page.clone());
        }
    }

    if exported_pages.is_empty() {
        // A slide-less presentation has an empty `p:sldIdLst`, which PowerPoint
        // treats as a corrupt file; refuse rather than write one.
        return Err(eco_vec![
            SourceDiagnostic::error(
                Span::detached(),
                "the selected --pages range contains no pages to export",
            )
            .with_hint("PowerPoint cannot open a presentation with zero slides")
        ]);
    }

    let filtered = PagedDocument::new(exported_pages, document.info().clone());
    let speaker_notes = typst_pptx::speaker_notes(document)
        .into_iter()
        .filter_map(|note| {
            let slide_index = page_to_slide.get(note.slide_index).copied().flatten()?;
            Some(SpeakerNote { slide_index, text: note.text })
        })
        .collect();
    let bytes =
        typst_pptx::pptx(&filtered, &PptxOptions { speaker_notes: Some(speaker_notes) })?;
    config
        .output
        .write(&bytes)
        .map_err(|err| eco_format!("failed to write PPTX file ({err})"))
        .at(Span::detached())?;
    Ok(())
}

/// Export to a PDF.
fn export_pdf(document: &PagedDocument, config: &CompileConfig) -> SourceResult<()> {
    let options = pdf_options(config);
    let buffer = typst_pdf::pdf(document, &options)?;
    config
        .output
        .write(&buffer)
        .map_err(|err| eco_format!("failed to write PDF file ({err})"))
        .at(Span::detached())?;
    Ok(())
}

/// Export to a bundle, a collection of files in a directory.
fn export_bundle(bundle: Bundle, config: &CompileConfig) -> SourceResult<Vec<Output>> {
    let options = BundleOptions {
        html: html_options(config),
        pdf: pdf_options(config),
        png: png_options(config),
        svg: svg_options(config),
    };

    let fs = typst_bundle::export(&bundle, &options)?;
    let root = match &config.output {
        Output::Path(path) => path,
        Output::Stdout => {
            bail!(Span::detached(), "cannot write bundle to standard output")
        }
    };

    let outputs = write_virtual_fs(root, &fs).at(Span::detached())?;

    #[cfg(feature = "http-server")]
    if let Some(server) = &config.server {
        server.set_bundle(bundle, fs);
    }

    Ok(outputs)
}

/// Writes a bundle's files to disk.
fn write_virtual_fs(root: &Path, fs: &VirtualFs) -> StrResult<Vec<Output>> {
    std::fs::create_dir_all(root)
        .map_err(|err| eco_format!("failed to create output directory ({err})"))?;

    fs.par_iter()
        .map(|(path, data)| {
            let realized = path
                .realize(root)
                .map_err(|err| eco_format!("failed to realize path ({err})"))?;

            if let Some(parent) = realized.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| eco_format!("failed to create directory ({err})"))?;
            }

            std::fs::write(&realized, data)
                .map_err(|err| eco_format!("failed to write file ({err})"))?;
            Ok(Output::Path(realized))
        })
        .collect()
}

/// Convert [`chrono::DateTime`] to [`Datetime`]
fn convert_datetime<Tz: chrono::TimeZone>(
    date_time: chrono::DateTime<Tz>,
) -> Option<Datetime> {
    Datetime::from_ymd_hms(
        date_time.year(),
        date_time.month().try_into().ok()?,
        date_time.day().try_into().ok()?,
        date_time.hour().try_into().ok()?,
        date_time.minute().try_into().ok()?,
        date_time.second().try_into().ok()?,
    )
}

/// An image format to export in.
#[derive(Copy, Clone)]
enum ImageExportFormat {
    Png,
    Svg,
}

/// Export to one or multiple images.
fn export_image(
    document: &PagedDocument,
    config: &CompileConfig,
    fmt: ImageExportFormat,
) -> StrResult<Vec<Output>> {
    // Determine whether we have indexable templates in output
    let can_handle_multiple = match config.output {
        Output::Stdout => false,
        Output::Path(ref output) => {
            output_template::has_indexable_template(output.to_str().unwrap_or_default())
        }
    };

    let exported_pages = document
        .pages()
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            config.pages.as_ref().is_none_or(|exported_page_ranges| {
                exported_page_ranges.includes_page_index(*i)
            })
        })
        .collect::<Vec<_>>();

    if !can_handle_multiple && exported_pages.len() > 1 {
        let err = match config.output {
            Output::Stdout => "to stdout",
            Output::Path(_) => {
                "without a page number template ({p}, {0p}) in the output path"
            }
        };
        bail!("cannot export multiple images {err}");
    }

    // The results are collected in a `Vec<()>` which does not allocate.
    exported_pages
        .par_iter()
        .map(|(i, page)| {
            // Use output with converted path.
            let output = match &config.output {
                Output::Path(path) => {
                    let storage;
                    let path = if can_handle_multiple {
                        storage = output_template::format(
                            path.to_str().unwrap_or_default(),
                            i + 1,
                            document.pages().len(),
                        );
                        Path::new(&storage)
                    } else {
                        path
                    };

                    // If we are not watching, don't use the cache.
                    // If the frame is in the cache, skip it.
                    // If the file does not exist, always create it.
                    if config.watching
                        && config.export_cache.is_cached(*i, page)
                        && path.exists()
                    {
                        return Ok(Output::Path(path.to_path_buf()));
                    }

                    Output::Path(path.to_owned())
                }
                Output::Stdout => Output::Stdout,
            };

            export_image_page(config, page, &output, fmt)?;
            Ok(output)
        })
        .collect::<StrResult<Vec<Output>>>()
}

mod output_template {
    const INDEXABLE: [&str; 3] = ["{p}", "{0p}", "{n}"];

    pub fn has_indexable_template(output: &str) -> bool {
        INDEXABLE.iter().any(|template| output.contains(template))
    }

    pub fn format(output: &str, this_page: usize, total_pages: usize) -> String {
        // Find the base 10 width of number `i`
        fn width(i: usize) -> usize {
            1 + i.checked_ilog10().unwrap_or(0) as usize
        }

        let other_templates = ["{t}"];
        INDEXABLE.iter().chain(other_templates.iter()).fold(
            output.to_string(),
            |out, template| {
                let replacement = match *template {
                    "{p}" => format!("{this_page}"),
                    "{0p}" | "{n}" => format!("{:01$}", this_page, width(total_pages)),
                    "{t}" => format!("{total_pages}"),
                    _ => unreachable!("unhandled template placeholder {template}"),
                };
                out.replace(template, replacement.as_str())
            },
        )
    }
}

/// Export single image.
fn export_image_page(
    config: &CompileConfig,
    page: &Page,
    output: &Output,
    fmt: ImageExportFormat,
) -> StrResult<()> {
    match fmt {
        ImageExportFormat::Png => {
            let options = png_options(config);
            let pixmap = typst_render::render(page, &options);
            let buf = pixmap
                .encode_png()
                .map_err(|err| eco_format!("failed to encode PNG file ({err})"))?;
            output
                .write(&buf)
                .map_err(|err| eco_format!("failed to write PNG file ({err})"))?;
        }
        ImageExportFormat::Svg => {
            let options = svg_options(config);
            let svg = typst_svg::svg(page, &options);
            output
                .write(svg.as_bytes())
                .map_err(|err| eco_format!("failed to write SVG file ({err})"))?;
        }
    }
    Ok(())
}

/// Creates options for HTML export.
fn html_options(config: &CompileConfig) -> HtmlOptions {
    HtmlOptions { pretty: config.pretty }
}

/// Creates options for PDF export.
fn pdf_options(config: &CompileConfig) -> PdfOptions {
    // If the timestamp is provided through the CLI, use UTC suffix,
    // else, use the current local time and timezone.
    let timestamp = match config.creation_timestamp {
        Some(timestamp) => convert_datetime(timestamp).map(Timestamp::new_utc),
        None => {
            let local_datetime = chrono::Local::now();
            convert_datetime(local_datetime).and_then(|datetime| {
                Timestamp::new_local(
                    datetime,
                    local_datetime.offset().local_minus_utc() / 60,
                )
            })
        }
    };

    PdfOptions {
        ident: Smart::Auto,
        creator: Smart::Auto,
        timestamp,
        page_ranges: config.pages.clone(),
        standards: config.pdf_standards.clone(),
        tagged: config.tagged,
        pretty: config.pretty,
    }
}

/// Creates options for SVG export.
fn svg_options(config: &CompileConfig) -> SvgOptions {
    SvgOptions { render_bleed: false, pretty: config.pretty }
}

/// Creates options for PNG export.
fn png_options(config: &CompileConfig) -> RenderOptions {
    RenderOptions {
        pixel_per_pt: Scalar::new(config.ppi / 72.0),
        render_bleed: false,
    }
}

/// Caches exported files so that we can avoid re-exporting them if they haven't
/// changed.
///
/// This is done by having a list of size `files.len()` that contains the hashes
/// of the last rendered frame in each file. If a new frame is inserted, this
/// will invalidate the rest of the cache, this is deliberate as to decrease the
/// complexity and memory usage of such a cache.
pub struct ExportCache {
    /// The hashes of last compilation's frames.
    pub cache: RwLock<Vec<u128>>,
}

impl ExportCache {
    /// Creates a new export cache.
    pub fn new() -> Self {
        Self { cache: RwLock::new(Vec::with_capacity(32)) }
    }

    /// Returns true if the entry is cached and appends the new hash to the
    /// cache (for the next compilation).
    pub fn is_cached(&self, i: usize, page: &Page) -> bool {
        let hash = typst::utils::hash128(page);

        let mut cache = self.cache.upgradable_read();
        if i >= cache.len() {
            cache.with_upgraded(|cache| cache.push(hash));
            return false;
        }

        cache.with_upgraded(|cache| std::mem::replace(&mut cache[i], hash) == hash)
    }
}

/// Opens the output if desired.
fn open_output(config: &mut CompileConfig) -> StrResult<()> {
    let Some(viewer) = config.open.take() else { return Ok(()) };

    #[cfg(feature = "http-server")]
    if let Some(server) = &config.server {
        let url = format!("http://{}", server.addr());
        return open_path(OsStr::new(&url), viewer.as_deref());
    }

    // Can't open stdout.
    let Output::Path(path) = &config.output else { return Ok(()) };

    // Some resource openers require the path to be canonicalized.
    let path = path
        .canonicalize()
        .map_err(|err| eco_format!("failed to canonicalize path ({err})"))?;

    open_path(path.as_os_str(), viewer.as_deref())
}

/// Opens the given file using:
///
/// - The default file viewer if `app` is `None`.
/// - The given viewer provided by `app` if it is `Some`.
fn open_path(path: &OsStr, viewer: Option<&str>) -> StrResult<()> {
    if let Some(viewer) = viewer {
        open::with_detached(path, viewer)
            .map_err(|err| eco_format!("failed to open file with {viewer} ({err})"))
    } else {
        open::that_detached(path).map_err(|err| {
            let openers = open::commands(path)
                .iter()
                .map(|command| command.get_program().to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ");
            eco_format!(
                "failed to open file with any of these resource openers: {openers} \
                 ({err})",
            )
        })
    }
}

/// Print diagnostic messages to the terminal.
pub fn print_diagnostics(
    world: &dyn DiagnosticWorld,
    errors: &[SourceDiagnostic],
    warnings: &[SourceDiagnostic],
    format: DiagnosticFormat,
) -> Result<(), codespan_reporting::files::Error> {
    typst_kit::diagnostics::emit(
        &mut terminal::out(),
        world,
        errors.iter().chain(warnings),
        match format {
            DiagnosticFormat::Human => typst_kit::diagnostics::DiagnosticFormat::Human,
            DiagnosticFormat::Short => typst_kit::diagnostics::DiagnosticFormat::Short,
        },
    )
}

impl From<PdfStandard> for typst_pdf::PdfStandard {
    fn from(standard: PdfStandard) -> Self {
        match standard {
            PdfStandard::V_1_4 => typst_pdf::PdfStandard::V_1_4,
            PdfStandard::V_1_5 => typst_pdf::PdfStandard::V_1_5,
            PdfStandard::V_1_6 => typst_pdf::PdfStandard::V_1_6,
            PdfStandard::V_1_7 => typst_pdf::PdfStandard::V_1_7,
            PdfStandard::V_2_0 => typst_pdf::PdfStandard::V_2_0,
            PdfStandard::A_1b => typst_pdf::PdfStandard::A_1b,
            PdfStandard::A_1a => typst_pdf::PdfStandard::A_1a,
            PdfStandard::A_2b => typst_pdf::PdfStandard::A_2b,
            PdfStandard::A_2u => typst_pdf::PdfStandard::A_2u,
            PdfStandard::A_2a => typst_pdf::PdfStandard::A_2a,
            PdfStandard::A_3b => typst_pdf::PdfStandard::A_3b,
            PdfStandard::A_3u => typst_pdf::PdfStandard::A_3u,
            PdfStandard::A_3a => typst_pdf::PdfStandard::A_3a,
            PdfStandard::A_4 => typst_pdf::PdfStandard::A_4,
            PdfStandard::A_4f => typst_pdf::PdfStandard::A_4f,
            PdfStandard::A_4e => typst_pdf::PdfStandard::A_4e,
            PdfStandard::UA_1 => typst_pdf::PdfStandard::Ua_1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mismatch(format: OutputFormat, size: (f64, f64)) -> bool {
        target_mismatch_warning(format, size).is_some()
    }

    #[test]
    fn slide_shaped_pages_nudge_docx_to_pptx() {
        // Common slide ratios written to DOCX warn; the same shape to PPTX does not.
        for size in [(1280.0, 720.0), (1024.0, 640.0), (960.0, 720.0), (1128.0, 480.0)] {
            assert!(mismatch(OutputFormat::Docx, size), "{size:?} should warn for DOCX");
            assert!(!mismatch(OutputFormat::Pptx, size), "{size:?} should suit PPTX");
        }
    }

    #[test]
    fn page_shaped_documents_nudge_pptx_to_docx() {
        // Portrait A4 and A4-landscape (√2) both read as documents, not slides.
        for size in [(595.0, 842.0), (842.0, 595.0)] {
            assert!(mismatch(OutputFormat::Pptx, size), "{size:?} should warn for PPTX");
            assert!(!mismatch(OutputFormat::Docx, size), "{size:?} should suit DOCX");
        }
    }

    #[test]
    fn other_formats_never_warn() {
        for format in [OutputFormat::Pdf, OutputFormat::Png, OutputFormat::Svg] {
            assert!(!mismatch(format, (1280.0, 720.0)));
            assert!(!mismatch(format, (595.0, 842.0)));
        }
    }
}
