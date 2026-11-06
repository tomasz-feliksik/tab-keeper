//! tab-keeper: list, close, group and arrange Chrome tabs across profiles.

mod client;
mod display;
mod frame;
mod host;
mod mcp;
mod ops;
mod policy;
mod profiles;
mod saved;
mod tabs;

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use ops::{Env, OpError, Report};

/// Exit status when something asked for did not happen, while the rest did.
const PARTIAL: u8 = 2;
const DEFAULT_SAVED_LIMIT: usize = 50;

/// Lets people and agents list, close, group and arrange Chrome tabs across profiles.
#[derive(Parser)]
#[command(name = "tab-keeper", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// Seconds to wait for each profile.
    #[arg(long, global = true, default_value_t = 10)]
    timeout: u64,
}

/// A protection a caller may knowingly override for one request.
#[derive(Clone, Copy, ValueEnum)]
enum Protection {
    Active,
    Pinned,
    Audible,
}

impl Protection {
    fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pinned => "pinned",
            Self::Audible => "audible",
        }
    }
}

fn names(allow: &[Protection]) -> Vec<String> {
    allow.iter().map(|p| p.name().to_owned()).collect()
}

#[derive(Subcommand)]
enum Command {
    /// The native messaging relay. Chrome runs this, not people.
    Host {
        /// The extension origin Chrome passes to a native host. Chrome has already
        /// checked it against the manifest's `allowed_origins`, so it is not used.
        origin: Option<String>,
    },
    /// Run the MCP server over stdio.
    Mcp {
        /// Read the policy from here instead of Chrome's own directories.
        #[arg(long)]
        policy_dir: Vec<PathBuf>,
    },
    /// Every tab in every running profile. Flags: Active, Pinned, Sound, Discarded.
    List(ListArgs),
    /// Connected profiles, and those known but not connected.
    Profiles,
    /// Unload idle tabs to free memory. They stay in the tab strip.
    Discard(DiscardArgs),
    /// Save tabs, then close them.
    Close {
        /// References printed by `list`.
        #[arg(required = true)]
        tabs: Vec<String>,
        /// Close tabs with this protection too.
        #[arg(long, value_delimiter = ',')]
        allow: Vec<Protection>,
    },
    /// Reopen a saved set in the same profile.
    Restore {
        record: String,
        /// Open a record that was already restored.
        #[arg(long)]
        again: bool,
    },
    /// What has been saved.
    Saved {
        #[arg(long, default_value_t = DEFAULT_SAVED_LIMIT)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Rearrange tabs inside a profile.
    Move {
        #[arg(required = true)]
        tabs: Vec<String>,
        /// The window to move to. The tab's own window if left out.
        #[arg(long)]
        window: Option<i64>,
        /// The position to move to, or -1 for the end.
        #[arg(long, allow_negative_numbers = true)]
        index: i64,
    },
    /// Create or extend a tab group, or manage saved groups.
    Group(GroupArgs),
    /// Take tabs out of a group, leaving them open.
    Ungroup {
        #[arg(required = true)]
        tabs: Vec<String>,
    },
    /// Manage bookmarks in a profile.
    #[command(subcommand)]
    Bookmark(BookmarkCommand),
}

#[derive(Args)]
struct ListArgs {
    /// Only this profile.
    #[arg(long)]
    profile: Option<String>,
    /// Tabs per page.
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Only tabs on this host or its subdomains.
    #[arg(long)]
    host: Option<String>,
    /// Only tabs with this in the title or URL.
    #[arg(long)]
    search: Option<String>,
    /// Only tabs idle for at least this long, like 2h.
    #[arg(long, value_parser = ops::parse_duration_ms)]
    idle_over: Option<u64>,
    /// Do not shorten titles and URLs.
    #[arg(long)]
    full: bool,
}

#[derive(Args)]
struct DiscardArgs {
    /// Specific tabs. References printed by `list`.
    tabs: Vec<String>,
    /// Every tab idle for at least this long, like 2h.
    #[arg(long, value_parser = ops::parse_duration_ms)]
    idle: Option<u64>,
    /// Only this profile.
    #[arg(long)]
    profile: Option<String>,
    /// Say what would happen, and do nothing.
    #[arg(long)]
    dry_run: bool,
    /// Unload tabs with this protection too. `TabDiscardingExceptions` always holds.
    #[arg(long, value_delimiter = ',')]
    allow: Vec<Protection>,
    /// Read the policy from here instead of Chrome's own directories.
    #[arg(long)]
    policy_dir: Vec<PathBuf>,
}

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct GroupArgs {
    #[command(subcommand)]
    saved: Option<SavedGroupCommand>,
    #[arg(required = true)]
    tabs: Vec<String>,
    /// Extends a group of this title in the profile if there is one.
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    color: Option<String>,
    /// Group tabs with this protection too. Grouping a pinned tab unpins it.
    #[arg(long, value_delimiter = ',')]
    allow: Vec<Protection>,
}

#[derive(Subcommand)]
enum SavedGroupCommand {
    /// Close a group to a named record.
    Save {
        name: String,
        /// The title of the open group to save.
        #[arg(long)]
        group: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, value_delimiter = ',')]
        allow: Vec<Protection>,
    },
    /// Open a saved group again.
    Open { name: String },
    /// Saved groups.
    List,
    /// Forget a saved group. This cannot be undone.
    Delete { name: String },
}

#[derive(Subcommand)]
enum BookmarkCommand {
    /// Add one bookmark, or a set of tabs, optionally into a new folder.
    Add {
        tabs: Vec<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        url: Option<String>,
        /// A new folder to put them in.
        #[arg(long)]
        folder: Option<String>,
        /// An existing folder's ID.
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Bookmarks, a page at a time.
    List {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, default_value_t = DEFAULT_SAVED_LIMIT)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Save a bookmark or folder, then remove it.
    Remove {
        id: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Put back a removed bookmark or folder.
    Restore { record: String },
}

/// `$TAB_KEEPER_BROWSER_DIRS`, or the Chrome-family directories under `~/.config`.
fn browser_dirs() -> Vec<PathBuf> {
    if let Some(listed) = std::env::var_os("TAB_KEEPER_BROWSER_DIRS") {
        return std::env::split_paths(&listed).collect();
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let Some(config) = config else {
        return Vec::new();
    };
    [
        "google-chrome",
        "google-chrome-beta",
        "google-chrome-unstable",
        "chromium",
    ]
    .iter()
    .map(|browser| config.join(browser))
    .collect()
}

fn environment(timeout: Duration) -> Result<Env, OpError> {
    Ok(Env {
        runtime_dir: host::runtime_dir(std::env::var_os("XDG_RUNTIME_DIR"))?,
        state_home: saved::state_home(
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )?,
        browser_dirs: browser_dirs(),
        timeout,
        now_ms: ops::now_ms(),
    })
}

fn default_policy_dirs(given: Vec<PathBuf>) -> Vec<PathBuf> {
    if given.is_empty() {
        policy::DEFAULT_POLICY_DIRS
            .iter()
            .map(PathBuf::from)
            .collect()
    } else {
        given
    }
}

fn run(command: Command, env: &Env) -> Result<Report, OpError> {
    match command {
        Command::Host { .. } | Command::Mcp { .. } => {
            unreachable!("handled before an operation is chosen")
        }
        Command::List(args) => ops::list(
            env,
            &ops::ListOptions {
                profile: args.profile,
                limit: args.limit,
                offset: args.offset,
                host: args.host,
                search: args.search,
                idle_over_ms: args.idle_over,
                full: args.full,
            },
        ),
        Command::Profiles => ops::list_profiles(env),
        Command::Discard(args) => ops::discard(
            env,
            &ops::DiscardOptions {
                idle_ms: args.idle,
                refs: args.tabs,
                profile: args.profile,
                dry_run: args.dry_run,
                allow: names(&args.allow),
                policy_dirs: default_policy_dirs(args.policy_dir),
            },
        ),
        Command::Close { tabs, allow } => ops::close(env, &tabs, &names(&allow)),
        Command::Restore { record, again } => ops::restore(env, &record, again),
        Command::Saved { limit, offset } => ops::list_saved(env, limit, offset),
        Command::Move {
            tabs,
            window,
            index,
        } => ops::move_tabs(
            env,
            &ops::MoveOptions {
                refs: tabs,
                window,
                index,
            },
        ),
        Command::Group(args) => run_group(args, env),
        Command::Ungroup { tabs } => ops::ungroup(env, &tabs),
        Command::Bookmark(command) => run_bookmark(command, env),
    }
}

fn run_group(args: GroupArgs, env: &Env) -> Result<Report, OpError> {
    match args.saved {
        None => ops::group(
            env,
            &ops::GroupOptions {
                refs: args.tabs,
                title: args.title,
                color: args.color,
                allow: names(&args.allow),
            },
        ),
        Some(SavedGroupCommand::Save {
            name,
            group,
            profile,
            allow,
        }) => ops::save_group(env, &name, profile.as_deref(), &group, &names(&allow)),
        Some(SavedGroupCommand::Open { name }) => ops::open_group(env, &name),
        Some(SavedGroupCommand::List) => ops::list_groups_saved(env),
        Some(SavedGroupCommand::Delete { name }) => ops::delete_group(env, &name),
    }
}

fn run_bookmark(command: BookmarkCommand, env: &Env) -> Result<Report, OpError> {
    match command {
        BookmarkCommand::Add {
            tabs,
            title,
            url,
            folder,
            parent,
            profile,
        } => ops::add_bookmarks(
            env,
            &ops::BookmarkAdd {
                profile,
                title,
                url,
                refs: tabs,
                folder,
                parent,
            },
        ),
        BookmarkCommand::List {
            profile,
            limit,
            offset,
        } => ops::list_bookmarks(
            env,
            &ops::BookmarkListOptions {
                profile,
                limit,
                offset,
            },
        ),
        BookmarkCommand::Remove { id, profile } => {
            ops::remove_bookmark(env, profile.as_deref(), &id)
        }
        BookmarkCommand::Restore { record } => ops::restore_bookmark(env, &record),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Host { .. }) {
        return match host::run_from_environment() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                report_error(&error);
                ExitCode::FAILURE
            }
        };
    }
    let timeout = Duration::from_secs(cli.timeout);
    if let Command::Mcp { policy_dir } = cli.command {
        return serve_mcp(timeout, default_policy_dirs(policy_dir));
    }
    let outcome = environment(timeout).and_then(|env| run(cli.command, &env));
    match outcome {
        Ok(report) => {
            if cli.json {
                println!("{}", report.value);
            } else if !report.human.is_empty() {
                println!("{}", report.human);
            }
            if report.complete {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(PARTIAL)
            }
        }
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}

fn serve_mcp(timeout: Duration, policy_dirs: Vec<PathBuf>) -> ExitCode {
    let served = environment(timeout)
        .map_err(|error| Box::new(error) as Box<dyn Error>)
        .and_then(|env| {
            mcp::Server { env, policy_dirs }
                .serve(std::io::stdin().lock(), std::io::stdout().lock())
                .map_err(Into::into)
        });
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(error.as_ref());
            ExitCode::FAILURE
        }
    }
}

/// Stderr only: in host mode, stdout belongs to Chrome.
fn report_error(error: &dyn Error) {
    eprintln!("tab-keeper: {error}");
    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }
}
