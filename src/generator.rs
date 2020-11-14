use std::{fmt::{self, Debug, Formatter},
          fs::File,
          io::{Read, Write},
          iter,
          path::{PathBuf},
};
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use handlebars::{Handlebars, RenderError};
use mdbook::book::{BookItem, Chapter};
use mdbook::preprocess::{PreprocessorContext, Preprocessor};
use mdbook::renderer::{RenderContext};
use pulldown_cmark::{ Options, html, Parser };

use crate::config::Config;
use crate::DEFAULT_CSS;
use crate::resources::{self, Asset};

use super::Error;

/// The actual EPUB book renderer.
pub struct Generator<'a> {
    ctx: &'a RenderContext,
    builder: EpubBuilder<ZipLibrary>,
    config: Config,
    hbs: Handlebars<'a>,
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

        self.builder
            .metadata("generator", env!("CARGO_PKG_NAME"))?;
        self.builder.metadata("lang", "en")?;

        Ok(())
    }

    pub fn generate<W: Write>(mut self, writer: W) -> Result<(), Error> {
        info!("Generating the EPUB book");

        self.populate_metadata()?;
        self.generate_chapters()?;

        self.add_cover_image()?;
        self.embed_stylesheets()?;
        self.additional_assets()?;
        self.additional_resources()?;
        self.builder.generate(writer)?;
        info!("Generating the EPUB book - DONE !");
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
        let content_path = chapter.path.as_ref()
            .ok_or_else(|| Error::ContentFileNotFound(
                format!("Content file was not found for Chapter {}", &chapter.name)))?;
        trace!("add a chapter {:?} by a path = {:?}", chapter.name, content_path.clone());
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

    /// Render the chapter into its fully formed HTML representation.
    fn render_chapter(&self, ch: &Chapter) -> Result<String, RenderError> {

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_FOOTNOTES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);

        let mut body = String::new();
        html::push_html(&mut body, Parser::new_ext(&ch.content, options));

        let css_path = ch.path.as_ref()
            .ok_or_else(|| RenderError::new(format!("No CSS found by a path =  = {:?}", ch.path)))?;

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

        let stylesheet = self
            .generate_stylesheet()?;
        self.builder.stylesheet(stylesheet.as_slice())?;

        Ok(())
    }

    fn additional_assets(&mut self) -> Result<(), Error> {
        debug!("Embedding additional assets");

        let error = String::from("Failed finding/fetch resource taken from content? Look up content for possible error...");
        // resources::find can emit very unclear error based on internal MD content,
        // so let's give a tip to user in error message
        let assets = resources::find(self.ctx).expect(&error);

        for asset in assets {
            debug!("Embedding asset : {}", asset.filename.display());
            self.load_asset(&asset)?;
        }

        Ok(())
    }

    fn additional_resources(&mut self) -> Result<(), Error> {
        debug!("Embedding additional resources");

        for path in self.config.additional_resources.iter() {
            debug!("Embedding resource: {:?}", path);

            let full_path: PathBuf;
            if let Ok(full_path_internal) = path.canonicalize() { // try process by 'path only' first
                debug!("Found resource by a path = {:?}", full_path_internal);
                full_path = full_path_internal; // OK
            } else {
                debug!("Failed to find resource by path, trying to compose 'root + src + path'...");
                // try process by using 'root + src + path'
                let full_path_composed = self.ctx.root.join(self.ctx.config.book.src.clone()).join(path);
                debug!("Try embed resource by a path = {:?}", full_path_composed);
                if let Ok(full_path_src) = full_path_composed.canonicalize() {
                    full_path = full_path_src; // OK
                } else {
                    // try process by using 'root + path' finally
                    let mut error = format!("Failed to find resource file by 'root + src + path' = {:?}", full_path_composed);
                    warn!("{:?}", error);
                    debug!("Failed to find resource, trying to compose by 'root + path' only...");
                    let full_path_composed = self.ctx.root.join(path);
                    error = format!("Failed to find resource file by a root + path = {:?}", full_path_composed);
                    full_path = full_path_composed.canonicalize().expect(&error);
                }
            }
            let mt = mime_guess::from_path(&full_path).first_or_octet_stream();

            let content = File::open(&full_path).map_err(|_| Error::AssetOpen)?;
            debug!("Adding resource: {:?} / {:?} ", path, mt.to_string());
            self.builder.add_resource(&path, content, mt.to_string())?;
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
                let full_path_composed = self.ctx.root.join(self.ctx.config.book.src.clone()).join(path);
                debug!("Try cover image by a path = {:?}", full_path_composed);
                let error = format!("Failed to find cover image by full path-name = {:?}", full_path_composed);
                full_path = full_path_composed.canonicalize().expect(&error);
            }
            let mt = mime_guess::from_path(&full_path).first_or_octet_stream();

            let content = File::open(&full_path).map_err(|_| Error::AssetOpen)?;
            debug!("Adding cover image: {:?} / {:?} ", path, mt.to_string());
            self.builder.add_cover_image(&path, content, mt.to_string())?;
        }

        Ok(())
    }

    fn load_asset(&mut self, asset: &Asset) -> Result<(), Error> {
        let content = File::open(&asset.location_on_disk).map_err(|_| Error::AssetOpen)?;

        let mt = asset.mimetype.to_string();

        self.builder.add_resource(&asset.filename, content, mt)?;

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
            let mut f = File::open(&additional_css).map_err(|_| Error::CssOpen(additional_css.clone()))?;
            f.read_to_end(&mut stylesheet).map_err(|_| Error::StylesheetRead)?;
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
