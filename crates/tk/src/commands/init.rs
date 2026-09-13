//! `tk init` creates, validates, or recovers a Store Association (ADR-0053).
use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::git::discovery;
use clap::Args as ClapArgs;

/// Initialize the current repository's Store Association.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Explicitly attach a valid Store whose former ownership has been released.
    #[arg(long, value_name = "STORE_ID", conflicts_with = "new")]
    pub attach: Option<String>,
    /// Create a distinct Store, preserving all prior Stores.
    #[arg(long)]
    pub new: bool,
}

/// Publish a fresh Store before installing its repository-local pointer.
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let paths = discovery::discover_paths(deps.runner, deps.cwd)
        .map_err(|e| CommandError::failure(e.to_string()))?;
    let result = crate::store::initialize::initialize(
        deps.runner,
        deps.cwd,
        deps.clock,
        deps.rng,
        deps.data_root,
        &paths,
        match args.attach.as_deref() {
            Some(id) => crate::store::initialize::Mode::Attach(id),
            None if args.new => crate::store::initialize::Mode::New,
            None => crate::store::initialize::Mode::Plain,
        },
    );
    let (path, prefix) = match result.map_err(|e| resolver::open_error(&e))? {
        crate::store::initialize::Initialized::Created { path, missing } => {
            for id in missing {
                let _ = writeln!(
                    deps.stderr,
                    "Missing Store {id}: possible data loss; its ID was not recreated"
                );
            }
            (path, "Initialized Repository Store at ")
        }
        crate::store::initialize::Initialized::Recovery(report) => {
            return Err(recovery_report(&report));
        }
        crate::store::initialize::Initialized::Attached(path) => {
            (path, "Attached Repository Store at ")
        }
        crate::store::initialize::Initialized::Existing(path) => {
            (path, "Repository Store already initialized at ")
        }
    };
    let _ = writeln!(deps.stdout, "{prefix}{}", path.display());
    Ok(Exit::Ok)
}

fn recovery_report(report: &crate::store::recovery::Report) -> CommandError {
    use crate::store::recovery::Fact;
    use std::fmt::Write as _;
    let mut text = String::new();
    if let Some(error) = &report.pointer_error {
        let _ = writeln!(text, "{error}");
    }
    text.push_str("Store evidence requires recovery; data was preserved\n");
    for candidate in &report.candidates {
        let _ = writeln!(text, "Store {:?}:", candidate.id);
        for fact in &candidate.facts {
            match fact {
                Fact::Referenced => {
                    let _ = writeln!(text, "  referenced Store ID");
                }
                Fact::CurrentPath(path) => {
                    let _ = writeln!(
                        text,
                        "  associated current canonical path: {}",
                        path.display()
                    );
                }
                Fact::HistoricalPath(path) => {
                    let _ = writeln!(text, "  historical canonical path: {}", path.display());
                }
                Fact::RemoteUrl(url) => {
                    let _ = writeln!(text, "  exact Git remote URL: {url}");
                }
                Fact::MissingStore => {
                    let _ = writeln!(
                        text,
                        "  missing Store: possible data loss; restore it from backup"
                    );
                }
            }
        }
        match &candidate.available {
            Ok(()) => {
                let _ = writeln!(text, "  available: tk init --attach {}", candidate.id);
            }
            Err(reason) => {
                let _ = writeln!(
                    text,
                    "  unavailable: {reason}; restore or release ownership manually"
                );
            }
        }
    }
    text.push_str("Create a distinct Store, preserving prior Stores: tk init --new");
    CommandError::failure(text)
}
