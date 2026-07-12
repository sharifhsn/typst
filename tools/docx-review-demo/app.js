const initialSource = `#set page(
  header: [Research Note · July 2026],
  footer: [#context counter(page).display()],
)

= _A practical path to better collaboration_

Typst gives authors a fast, programmable writing
environment while Word remains the lingua franca of
academic review.

Our central claim is #strong[round trips should preserve
intent], not merely copy visible text.

== What survives the trip

- Headings, paragraphs, lists, links, captions, and table cells
- Comments, tracked edits, headers, footers, and footnotes

#table(
  columns: (1fr, auto),
  [Literal source-backed text], [#green[Ready]],
  [Formatting or structural edits], [#orange[Guarded]],
)

#figure(
  workflow,
  caption: [A conflict-safe advisor workflow],
)

The source remains the student's authority.#footnote[
  The source remains the student's authority.
]

#include "chapter.typ"`;

const chapterSource = `= Supporting evidence

Imported project files participate in the same transactional
review. Every source is validated before any file is replaced.`;

const sourceMap = { "main.typ": initialSource, "chapter.typ": chapterSource };
const originals = new Map();
const regions = [...document.querySelectorAll("[data-region]")];
regions.forEach((region) => originals.set(region.dataset.region, region.textContent.trim()));

const sourceEditor = document.querySelector("#source-editor");
const lineNumbers = document.querySelector("#line-numbers");
const cursorLine = document.querySelector("#cursor-line");
const cursorColumn = document.querySelector("#cursor-column");
const changeList = document.querySelector("#change-list");
const reviewTitle = document.querySelector("#review-title");
const reviewSummary = document.querySelector("#review-summary");
const applyButton = document.querySelector("#apply-button");
const toast = document.querySelector("#toast");
const commentCard = document.querySelector("#comment-card");
let activeFile = "main.typ";
let comment = null;
let formattingConflict = false;
let structuralConflict = false;

function setSource(value) {
  sourceMap[activeFile] = value;
  sourceEditor.value = value;
  renderLineNumbers();
}

function renderLineNumbers() {
  const lines = sourceEditor.value.split("\n").length;
  lineNumbers.innerHTML = Array.from({ length: lines }, (_, index) => `<li>${index + 1}</li>`).join("");
}

function updateCursor() {
  const before = sourceEditor.value.slice(0, sourceEditor.selectionStart);
  const lines = before.split("\n");
  cursorLine.textContent = lines.length;
  cursorColumn.textContent = lines.at(-1).length + 1;
}

function escapeHtml(value) {
  return value.replace(/[&<>"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[character]);
}

function getChanges() {
  return regions.flatMap((region) => {
    const before = originals.get(region.dataset.region);
    const after = region.textContent.trim();
    return before === after ? [] : [{ kind: region.dataset.region, before, after }];
  });
}

function markChangedRegions() {
  regions.forEach((region) => {
    region.classList.toggle("changed", region.textContent.trim() !== originals.get(region.dataset.region));
  });
}

function reviewChanges() {
  markChangedRegions();
  const changes = getChanges();
  const rows = changes.map(({ kind, before, after }) => `
    <div class="change-row ready">
      <span class="kind">${escapeHtml(kind)}</span>
      <span class="diff"><del>${escapeHtml(before)}</del> → <ins>${escapeHtml(after)}</ins></span>
      <span class="result">Ready</span>
    </div>`);

  if (comment) rows.push(`
    <div class="change-row comment">
      <span class="kind">comment</span>
      <span class="diff">Advisor · “${escapeHtml(comment)}”</span>
      <span class="result">Captured</span>
    </div>`);

  if (formattingConflict) rows.push(`
    <div class="change-row conflict">
      <span class="kind">format</span>
      <span class="diff">Word added bold formatting to a source-backed region</span>
      <span class="result">Conflict</span>
    </div>`);

  if (structuralConflict) rows.push(`
    <div class="change-row conflict">
      <span class="kind">structure</span>
      <span class="diff">Word story paragraph count changed: 9 → 10</span>
      <span class="result">Conflict</span>
    </div>`);

  const total = rows.length;
  const conflicts = Number(formattingConflict) + Number(structuralConflict);
  reviewTitle.textContent = total ? `${total} review ${total === 1 ? "event" : "events"} found` : "No advisor changes found";
  reviewSummary.textContent = conflicts ? `${changes.length} ready · ${conflicts} conflict${conflicts > 1 ? "s" : ""}` : `${changes.length} ready · 0 conflicts`;
  changeList.innerHTML = rows.join("") || `<div class="empty-state"><span>✓</span><p>The returned document matches its review baseline.</p></div>`;
  applyButton.disabled = changes.length === 0 || conflicts > 0;
  document.querySelector("#review-drawer").scrollIntoView({ behavior: "smooth", block: "nearest" });
}

function applyChanges() {
  if (applyButton.disabled) return;
  const changes = getChanges();
  let source = sourceMap["main.typ"];
  const replacements = {
    heading: [/A practical path to better collaboration/, null],
    paragraph: [/Typst gives authors a fast, programmable writing\nenvironment while Word remains the lingua franca of\nacademic review\./, null],
    inline: [/round trips should preserve\nintent/, null],
    list: [/Headings, paragraphs, lists, links, captions, and table cells/, null],
    table: [/Literal source-backed text/, null],
    caption: [/A conflict-safe advisor workflow/, null],
    footnote: [/The source remains the student's authority\./, null],
  };

  changes.forEach(({ kind, after }) => {
    if (!replacements[kind]) return;
    const literal = `#("${after.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}")`;
    replacements[kind][1] = literal;
    source = source.replace(replacements[kind][0], literal);
  });
  sourceMap["main.typ"] = source;
  if (activeFile === "main.typ") setSource(source);
  changes.forEach(({ kind, after }) => originals.set(kind, after));
  regions.forEach((region) => region.classList.remove("changed"));
  reviewTitle.textContent = "Applied safely to Typst";
  reviewSummary.textContent = `${changes.length} regions updated · 2 files verified`;
  changeList.innerHTML = `<div class="empty-state"><span>✓</span><p>The Typst source now contains identifier-free literal expressions. Export again to begin the next advisor cycle.</p></div>`;
  applyButton.disabled = true;
  showToast("Round-trip applied to Typst source");
}

function resetDemo() {
  sourceMap["main.typ"] = initialSource;
  sourceMap["chapter.typ"] = chapterSource;
  activeFile = "main.typ";
  document.querySelectorAll(".file-pill").forEach((button) => button.classList.toggle("is-active", button.dataset.file === activeFile));
  setSource(initialSource);
  const defaults = {
    heading: "A practical path to better collaboration",
    paragraph: "Typst gives authors a fast, programmable writing environment while Word remains the lingua franca of academic review.",
    inline: "round trips should preserve intent",
    list: "Headings, paragraphs, lists, links, captions, and table cells",
    table: "Literal source-backed text",
    caption: "A conflict-safe advisor workflow",
    footnote: "The source remains the student's authority.",
  };
  regions.forEach((region) => { region.textContent = defaults[region.dataset.region]; region.className = ""; });
  document.querySelector(".word-page h1").setAttribute("contenteditable", "true");
  document.querySelectorAll(".inserted-paragraph").forEach((node) => node.remove());
  comment = null;
  formattingConflict = false;
  structuralConflict = false;
  document.querySelector("#bold-button").classList.remove("is-active");
  commentCard.hidden = true;
  reviewTitle.textContent = "Make an advisor edit to begin";
  reviewSummary.textContent = "Waiting for changes";
  changeList.innerHTML = `<div class="empty-state"><span>↳</span><p>Edit any blue-outlined Word region, add a comment, or simulate a guarded formatting/structure change.</p></div>`;
  applyButton.disabled = true;
  showToast("Demo reset");
}

function showToast(message) {
  toast.textContent = message;
  toast.classList.add("show");
  window.clearTimeout(showToast.timeout);
  showToast.timeout = window.setTimeout(() => toast.classList.remove("show"), 2200);
}

sourceEditor.addEventListener("input", () => { sourceMap[activeFile] = sourceEditor.value; renderLineNumbers(); });
sourceEditor.addEventListener("keyup", updateCursor);
sourceEditor.addEventListener("click", updateCursor);
sourceEditor.addEventListener("scroll", () => { lineNumbers.scrollTop = sourceEditor.scrollTop; });
regions.forEach((region) => region.addEventListener("input", markChangedRegions));
document.querySelector("#review-button").addEventListener("click", reviewChanges);
applyButton.addEventListener("click", applyChanges);
document.querySelector("#reset-button").addEventListener("click", resetDemo);

document.querySelectorAll(".file-pill").forEach((button) => button.addEventListener("click", () => {
  sourceMap[activeFile] = sourceEditor.value;
  activeFile = button.dataset.file;
  document.querySelectorAll(".file-pill").forEach((item) => item.classList.toggle("is-active", item === button));
  setSource(sourceMap[activeFile]);
}));

document.querySelectorAll(".tab").forEach((tab) => tab.addEventListener("click", () => {
  document.querySelectorAll(".tab").forEach((item) => item.classList.toggle("is-active", item === tab));
  document.querySelectorAll(".view").forEach((view) => view.classList.remove("is-active"));
  document.querySelector(`#${tab.dataset.view}-view`).classList.add("is-active");
}));

document.querySelector("#theme-toggle").addEventListener("click", () => document.body.classList.toggle("dark"));
document.querySelector("#comment-button").addEventListener("click", () => { commentCard.hidden = false; commentCard.querySelector("textarea").focus(); });
document.querySelector("#cancel-comment").addEventListener("click", () => { commentCard.hidden = true; });
document.querySelector("#save-comment").addEventListener("click", () => {
  comment = commentCard.querySelector("textarea").value.trim();
  commentCard.hidden = true;
  showToast("Advisor comment attached to region");
  reviewChanges();
});

document.querySelector("#bold-button").addEventListener("click", (event) => {
  formattingConflict = !formattingConflict;
  event.currentTarget.classList.toggle("is-active", formattingConflict);
  const target = document.querySelector('[data-region="inline"]');
  target.classList.toggle("format-conflict", formattingConflict);
  target.style.fontWeight = formattingConflict ? "800" : "700";
  reviewChanges();
});

document.querySelector("#insert-button").addEventListener("click", () => {
  structuralConflict = !structuralConflict;
  const existing = document.querySelector(".inserted-paragraph");
  if (existing) existing.remove();
  if (structuralConflict) {
    const inserted = document.createElement("p");
    inserted.className = "inserted-paragraph";
    inserted.textContent = "Advisor inserted a new paragraph outside a mapped region.";
    document.querySelector(".word-page h3").before(inserted);
  }
  reviewChanges();
});

document.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key === "Enter") { event.preventDefault(); reviewChanges(); }
});

setSource(initialSource);
updateCursor();
