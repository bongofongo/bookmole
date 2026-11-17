# epubkit

A bounded, read-only EPUB 2/3 reader over `zip` and `quick-xml`.

```rust
let mut book = epubkit::Package::open("book.epub")?;
println!("{:?}", book.metadata().main_title());
let cover = book.cover();

// The table of contents: the EPUB 3 navigation document, else the NCX.
if let Some(toc) = book.toc() {
    for (depth, entry) in toc.iter() {
        println!("{}{} -> {:?}", "  ".repeat(depth), entry.label, entry.target());
    }
}
// Or each document whole, with its landmarks and page list.
let nav = book.nav();
let ncx = book.ncx();

// A chapter's bytes, or its text in blocks (headings, paragraphs, …).
let doc = book.spine_document(0)?;
for block in book.spine_text(0)? {
    println!("{:?}: {}", block.kind, block.text);
}
```

The package document is parsed once at open; resources, the cover, the
tables of contents and chapter text are read on demand. Every read is bounded
by `Limits` (entry count, per-entry and total decompressed bytes, and for any
markup document its size, depth, attributes per element and node count). No
DTD is processed, and no entity is expanded beyond XML's five, HTML 4's 253
named ones and numeric references. Nothing is extracted to disk. The package
document and NCX are read as strict XML; navigation documents and chapters
are read leniently, as the XHTML real books carry. Problems a book can
survive are collected as typed warnings on `package.warnings()`.
