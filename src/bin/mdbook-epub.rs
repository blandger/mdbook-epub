use ::env_logger;
#[macro_use]
extern crate log;
use ::mdbook;
use ::mdbook_epub;
use ::structopt;

use mdbook::renderer::RenderContext;
use mdbook::MDBook;
use std::path::PathBuf;
use std::{io, process};
use structopt::StructOpt;

use mdbook_epub::Error;

fn main() {
    env_logger::init();
    info!("Booting EPUB generator...");
    let args = Args::from_args();
    debug!("prepared generator args = {:?}", args);

    if let Err(e) = run(&args) {
        log::error!("{}", e);

        process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), Error> {
    debug!("run EPUB book build...");
    // get a `RenderContext`, either from stdin (because it's used as a plugin)
    // or by instrumenting MDBook directly
    let error = format!(
        "book.toml root file is not found by a path {:?}",
        &args.root.display()
    );
    let md = MDBook::load(&args.root).expect(&error);
    let ctx: RenderContext = if args.standalone {
        let destination = md.build_dir_for("epub");
        debug!(
            "EPUB book destination folder is : {:?}",
            destination.display()
        );
        debug!("EPUB book config is : {:?}", &md.config);
        RenderContext::new(md.root.clone(), md.book.clone(), md.config.clone(), destination)
    } else {
        println!("Running mdbook-epub as plugin...");
        serde_json::from_reader(io::stdin()).map_err(|_| Error::RenderContext)?
    };
    mdbook_epub::generate(&ctx, md.clone_preprocessors())?;

    info!(
        "Book is READY in directory: '{}'",
        ctx.destination.display()
    );

    Ok(())
}

#[derive(Debug, Clone, StructOpt)]
struct Args {
    #[structopt(
        short = "s",
        long = "standalone",
        parse(try_from_str),
        default_value = "true",
        help = "Run standalone (i.e. not as a mdbook plugin)"
    )]
    standalone: bool,
    #[structopt(help = "The book to render.", parse(from_os_str), default_value = ".")]
    root: PathBuf,
}
