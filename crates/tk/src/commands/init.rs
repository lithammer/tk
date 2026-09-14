//! `tk init` creates, validates, or recovers a Store Association (ADR-0053).
use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::git::discovery;
use crate::store::initialize::{self, Initialized, Mode};
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

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let paths = discovery::discover_paths(deps.runner, deps.cwd)
        .map_err(|e| CommandError::failure(e.to_string()))?;
    let result = initialize::initialize(
        deps.runner,
        deps.cwd,
        deps.clock,
        deps.rng,
        deps.data_root,
        &paths,
        initialize::Options {
            observe: deps.migration_boundary,
            mode: match args.attach.as_deref() {
                Some(id) => Mode::Attach(id),
                None if args.new => Mode::New,
                None => Mode::Plain,
            },
        },
    );
    let (path, prefix) = match result.map_err(|e| resolver::open_error(&e))? {
        Initialized::Migrated { from, path } => {
            let id = path
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy();
            let _ = writeln!(
                deps.stdout,
                "Migrated Repository Store {id} from {} to {}",
                from.display(),
                path.parent().unwrap().display()
            );
            return Ok(Exit::Ok);
        }
        Initialized::Created { path, missing } => {
            for id in missing {
                let _ = writeln!(
                    deps.stderr,
                    "Missing Store {id}: possible data loss; its ID was not recreated"
                );
            }
            (path, "Initialized Repository Store at ")
        }
        Initialized::Recovery(report) => {
            return Err(recovery_report(&report));
        }
        Initialized::Attached(path) => (path, "Attached Repository Store at "),
        Initialized::Existing(path) => (path, "Repository Store already initialized at "),
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
