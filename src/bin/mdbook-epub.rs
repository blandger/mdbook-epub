use ::env_logger;
#[macro_use]
extern crate log;
use ::mdbook;
use ::mdbook_epub;
use ::structopt;

use mdbook::renderer::RenderContext;
use mdbook::MDBook;
use std::path::PathBuf;
use std::process;
use structopt::StructOpt;

use mdbook_epub::Error;

fn main() {
    env_logger::init();
    info!("Booting EPUB generator...");
    let args = Args::from_args();
    debug!("generator args = {:?}", args);

    if let Err(e) = run(&args) {
        log::error!("{}", e);

        process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), Error> {
    // get a `RenderContext`, either from stdin (because we're used as a plugin)
    // or by instrumenting MDBook directly (in standalone mode).
    let md: MDBook;
    // let ctx: RenderContext = if args.standalone {
        let error = format!("book.toml root file is not found by a path {:?}",
                            &args.root.display());
        let mdbook = MDBook::load(&args.root).expect(&error);
        let destination = mdbook.build_dir_for("epub");
        debug!("EPUB book destination folder is : {:?}", destination.display());
        debug!("EPUB book config is : {:?}", mdbook.config);
        md = mdbook;
    let ctx: RenderContext = RenderContext::new(md.root.clone(), md.book.clone(), md.config.clone(), destination);
    // } else {
    //     serde_json::from_reader(io::stdin()).map_err(|_| Error::RenderContext)?
    // };

    mdbook_epub::generate(&ctx, md.clone_preprocessors())?;

    Ok(())
}

#[derive(Debug, Clone, StructOpt)]
struct Args {
/*    #[structopt(
        short = "s",
        long = "standalone",
        help = "Run standalone (i.e. not as a mdbook plugin)"
    )]
    standalone: bool,*/
    #[structopt(help = "The book to render.", parse(from_os_str), default_value = ".")]
    root: PathBuf,
}
