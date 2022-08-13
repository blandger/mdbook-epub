use std::{
    collections::HashMap,
    fmt::{self, Debug, Formatter},
    fs::File,
    io::{Read, Write},
    iter,
    path::PathBuf,
};

use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use handlebars::{Handlebars, RenderError};
use mdbook::book::{BookItem, Chapter};
use mdbook::preprocess::{PreprocessorContext, Preprocessor};
use mdbook::renderer::{RenderContext};
use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

use crate::config::Config;
use crate::resources::{self, Asset};
use crate::Error;
use crate::DEFAULT_CSS;

/// The actual EPUB book renderer.
pub struct Generator<'a> {
    ctx: &'a RenderContext,
    builder: EpubBuilder<ZipLibrary>,
    config: Config,
    hbs: Handlebars<'a>,
    assets: HashMap<String, Asset>,
    preprocess_ctx: PreprocessorContext,
    preprocessors: Vec<Box<dyn Preprocessor>>,
}

impl<'a> Generator<'a> {
    pub fn new(ctx: &'a RenderContext, preprocessors: Vec<Box<dyn Preprocessor>>) -> Result<Generator<'a>, Error> {
        let builder = EpubBuilder::new(ZipLibrary::new()?)?;
        let config = Config::from_render_context(ctx)?;

        let mut hbs = Handlebars::new();
        hbs.register_template_string("index", config.template()?)
            .map_err(|_| Error::TemplateParse)?;

        let preprocess_ctx = PreprocessorContext::new(
            ctx.root.clone(),
            ctx.config.clone(),
            "epub-builder".to_string(), // renderer.name().to_string()
        );

        Ok(Generator {
            builder,
            ctx,
            config,
            hbs,
            assets: HashMap::new(),
            preprocess_ctx,
            preprocessors,
        })
    }

    fn populate_metadata(&mut self) -> Result<(), Error> {
        self.builder.metadata("generator", "mdbook-epub")?;

        if let Some(title) = self.ctx.config.book.title.clone() {
            self.builder.metadata("title", title)?;
        } else {
            warn!("No `title` attribute found yet all EPUB documents should have a title");
        }

        if let Some(desc) = self.ctx.config.book.description.clone() {
            self.builder.metadata("description", desc)?;
        }

        if !self.ctx.config.book.authors.is_empty() {
            self.builder
                .metadata("author", self.ctx.config.book.authors.join(", "))?;
        }

        self.builder.metadata("generator", env!("CARGO_PKG_NAME"))?;

        if let Some(lang) = self.ctx.config.book.language.clone() {
            self.builder.metadata("lang", lang)?;
        } else {
            self.builder.metadata("lang", "en")?;
        }

        Ok(())
    }

    pub fn generate<W: Write>(mut self, writer: W) -> Result<(), Error> {
        info!("Generating the EPUB book");

        self.populate_metadata()?;
        self.find_assets()?;
        self.generate_chapters()?;

        self.add_cover_image()?;
        self.embed_stylesheets()?;
        self.additional_assets()?;
        self.additional_resources()?;
        self.builder.generate(writer)?;
        info!("Generating the EPUB book - DONE !");
        Ok(())
    }

    /// Find assets for adding to the document later. For remote linked assets, they would be
    /// rendered differently in the document by provided information of assets.
    fn find_assets(&mut self) -> Result<(), Error> {
        let error = String::from("Failed finding/fetch resource taken from content? Look up content for possible error...");
        // resources::find can emit very unclear error based on internal MD content,
        // so let's give a tip to user in error message
        let assets = resources::find(self.ctx).map_err(|e| {
            error!("{}", error);
            e
        })?;
        self.assets.extend(assets);
        Ok(())
    }

    fn generate_chapters(&mut self) -> Result<(), Error> {
        debug!("Rendering Chapters");

        for item in &self.ctx.book.sections {
            if let BookItem::Chapter(ch) = &mut item.clone() {
                trace!("Adding chapter \"{}\"", ch);
                self.add_chapter(ch)?;
            }
        }

        Ok(())
    }

    fn add_chapter(&mut self, ch: &mut Chapter) -> Result<(), Error> {
        for renderer in &self.preprocessors {
            renderer.preprocess_chapter(&self.preprocess_ctx, ch)?;
        }
        trace!("{}", &ch.content);
        let rendered = self.render_chapter(&ch)?;

        // let chapter = ch.borrow();
        let chapter = ch;
        let content_path = chapter.path.as_ref().ok_or_else(|| {
            Error::ContentFileNotFound(format!(
                "Content file was not found for Chapter {}",
                &chapter.name
            ))
        })?;
        trace!(
            "add a chapter {:?} by a path = {:?}",
            chapter.name,
            content_path
        .clone());
        let path = content_path.clone().with_extension("html").display().to_string();
        let mut content = EpubContent::new(path, rendered.as_bytes())
            .title(format!("{}", chapter));

        let level = chapter.number.as_ref().map(|n| n.len() as i32 - 1).unwrap_or(0);
        content = content.level(level);

        self.builder.add_content(content)?;

        // second pass to actually add the sub-chapters
        for sub_item in &chapter.sub_items {
            if let BookItem::Chapter(sub_ch) = &mut sub_item.clone() {
                trace!("add sub-item = {:?}", sub_ch.name);
                self.add_chapter(sub_ch)?;
            }
        }

        Ok(())
    }

    pub fn new_cmark_parser(text: &str) -> Parser<'_, '_> {
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_FOOTNOTES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TASKLISTS);
        Parser::new_ext(text, opts)
    }

    /// Render the chapter into its fully formed HTML representation.
    fn render_chapter(&self, ch: &Chapter) -> Result<String, RenderError> {

        let mut body = String::new();
        let p = Generator::new_cmark_parser(&ch.content);
        let mut converter = EventQuoteConverter::new(self.config.curly_quotes);
        let events = p.map(|event| converter.convert(event));

        html::push_html(&mut body, events);

        let css_path = ch.path.as_ref().ok_or_else(|| {
            RenderError::new(format!("No CSS found by a path =  = {:?}", ch.path))
        })?;

        let stylesheet_path = css_path
            .parent()
            .expect("All chapters have a parent")
            .components()
            .map(|_| "..")
            .chain(iter::once("stylesheet.css"))
            .collect::<Vec<_>>()
            .join("/");

        let ctx = json!({ "title": ch.name, "body": body, "stylesheet": stylesheet_path });

        self.hbs.render("index", &ctx)
    }

    /// Generate the stylesheet and add it to the document.
    fn embed_stylesheets(&mut self) -> Result<(), Error> {
        debug!("Embedding stylesheets");

        let stylesheet = self.generate_stylesheet()?;
        self.builder.stylesheet(stylesheet.as_slice())?;

        Ok(())
    }

    fn additional_assets(&mut self) -> Result<(), Error> {
        debug!("Embedding additional assets");

        // TODO: have a list of Asset URLs and try to download all of them (in parallel?)
        // to a temporary location.
        for asset in self.assets.values() {
            debug!("Embedding asset : {}", asset.filename.display());
            let content = File::open(&asset.location_on_disk).map_err(|_| Error::AssetOpen)?;

            let mt = asset.mimetype.to_string();

            self.builder.add_resource(&asset.filename, content, mt)?;
        }
        Ok(())
    }

    fn additional_resources(&mut self) -> Result<(), Error> {
        debug!("Embedding additional resources");

        for path in self.config.additional_resources.iter() {
            debug!("Embedding resource: {:?}", path);

            let full_path: PathBuf;
            if let Ok(full_path_internal) = path.canonicalize() {
                // try process by 'path only' first
                debug!("Found resource by a path = {:?}", full_path_internal);
                full_path = full_path_internal; // OK
            } else {
                debug!("Failed to find resource by path, trying to compose 'root + src + path'...");
                // try process by using 'root + src + path'
                let full_path_composed = self
                    .ctx
                    .root
                    .join(self.ctx.config.book.src.clone())
                    .join(path);
                debug!("Try embed resource by a path = {:?}", full_path_composed);
                if let Ok(full_path_src) = full_path_composed.canonicalize() {
                    full_path = full_path_src; // OK
                } else {
                    // try process by using 'root + path' finally
                    let mut error = format!(
                        "Failed to find resource file by 'root + src + path' = {:?}",
                        full_path_composed
                    );
                    warn!("{:?}", error);
                    debug!("Failed to find resource, trying to compose by 'root + path' only...");
                    let full_path_composed = self.ctx.root.join(path);
                    error = format!(
                        "Failed to find resource file by a root + path = {:?}",
                        full_path_composed
                    );
                    full_path = full_path_composed.canonicalize().expect(&error);
                }
            }
            let mt = mime_guess::from_path(&full_path).first_or_octet_stream();

            let content = File::open(&full_path).map_err(|_| Error::AssetOpen)?;
            debug!("Adding resource: {:?} / {:?} ", path, mt.to_string());
            self.builder.add_resource(path, content, mt.to_string())?;
        }

        Ok(())
    }

    fn add_cover_image(&mut self) -> Result<(), Error> {
        debug!("Adding cover image...");

        if let Some(ref path) = self.config.cover_image {
            let full_path: PathBuf;
            if let Ok(full_path_internal) = path.canonicalize() {
                debug!("Found resource by a path = {:?}", full_path_internal);
                full_path = full_path_internal;
            } else {
                debug!("Failed to find resource, trying to compose path...");
                let full_path_composed = self
                    .ctx
                    .root
                    .join(self.ctx.config.book.src.clone())
                    .join(path);
                debug!("Try cover image by a path = {:?}", full_path_composed);
                let error = format!(
                    "Failed to find cover image by full path-name = {:?}",
                    full_path_composed
                );
                full_path = full_path_composed.canonicalize().expect(&error);
            }
            let mt = mime_guess::from_path(&full_path).first_or_octet_stream();

            let content = File::open(&full_path).map_err(|_| Error::AssetOpen)?;
            debug!("Adding cover image: {:?} / {:?} ", path, mt.to_string());
            self.builder
                .add_cover_image(path, content, mt.to_string())?;
        }

        Ok(())
    }

    /// Concatenate all provided stylesheets into one long stylesheet.
    fn generate_stylesheet(&self) -> Result<Vec<u8>, Error> {
        let mut stylesheet = Vec::new();

        if self.config.use_default_css {
            stylesheet.extend(DEFAULT_CSS.as_bytes());
        }

        for additional_css in &self.config.additional_css {
            debug!("generating stylesheet: {:?}", &additional_css);
            let full_path: PathBuf;
            if let Ok(full_path_internal) = additional_css.canonicalize() {
                debug!("Found stylesheet by a path = {:?}", full_path_internal);
                full_path = full_path_internal;
            } else {
                debug!("Failed to find stylesheet, trying to compose path...");
                let full_path_composed = self.ctx.root.join(additional_css);
                debug!("Try stylesheet by a path = {:?}", full_path_composed);
                let error = format!(
                    "Failed to find stylesheet by full path-name = {:?}",
                    full_path_composed
                );
                full_path = full_path_composed.canonicalize().expect(&error);
            }
            let mut f = File::open(&full_path).map_err(|_| Error::CssOpen(full_path.clone()))?;
            f.read_to_end(&mut stylesheet)
                .map_err(|_| Error::StylesheetRead)?;
        }
        debug!("found style(s) = [{}]", stylesheet.len());
        Ok(stylesheet)
    }
}

impl<'a> Debug for Generator<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("Generator")
            .field("ctx", &self.ctx)
            .field("builder", &self.builder)
            .field("config", &self.config)
            .finish()
    }
}

/// From `mdbook/src/utils/mod.rs`, where this is a private struct.
struct EventQuoteConverter {
    enabled: bool,
    convert_text: bool,
}

impl EventQuoteConverter {
    fn new(enabled: bool) -> Self {
        EventQuoteConverter {
            enabled,
            convert_text: true,
        }
    }

    fn convert<'a>(&mut self, event: Event<'a>) -> Event<'a> {
        if !self.enabled {
            return event;
        }

        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                self.convert_text = false;
                event
            }
            Event::End(Tag::CodeBlock(_)) => {
                self.convert_text = true;
                event
            }
            Event::Text(ref text) if self.convert_text => {
                Event::Text(CowStr::from(convert_quotes_to_curly(text)))
            }
            _ => event,
        }
    }
}

fn convert_quotes_to_curly(original_text: &str) -> String {
    // We'll consider the start to be "whitespace".
    let mut preceded_by_whitespace = true;

    original_text
        .chars()
        .map(|original_char| {
            let converted_char = match original_char {
                '\'' => {
                    if preceded_by_whitespace {
                        '‘'
                    } else {
                        '’'
                    }
                }
                '"' => {
                    if preceded_by_whitespace {
                        '“'
                    } else {
                        '”'
                    }
                }
                _ => original_char,
            };

            preceded_by_whitespace = original_char.is_whitespace();

            converted_char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    #[should_panic]
    fn find_assets_with_wrong_src_dir() {
        let json = ctx_with_template(
            "# Chapter 1\n\n",
            "nosuchsrc",
            tempdir::TempDir::new("mdbook-epub").unwrap().path(),
        )
        .to_string();
        let ctx = RenderContext::from_json(json.as_bytes()).unwrap();
        let mut g = Generator::new(&ctx).unwrap();
        g.find_assets().unwrap();
    }

    fn ctx_with_template(content: &str, source: &str, destination: &Path) -> serde_json::Value {
        json!({
            "version": mdbook::MDBOOK_VERSION,
            "root": "tests/dummy",
            "book": {"sections": [{
                "Chapter": {
                    "name": "Chapter 1",
                    "content": content,
                    "number": [1],
                    "sub_items": [],
                    "path": "chapter_1.md",
                    "parent_names": []
                }}], "__non_exhaustive": null},
            "config": {
                "book": {"authors": [], "language": "en", "multilingual": false,
                    "src": source, "title": "DummyBook"},
                "output": {"epub": {"curly-quotes": true}}},
            "destination": destination
        })
    }
}
