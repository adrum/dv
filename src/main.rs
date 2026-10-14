//! dv — a fast .NET SDK version manager.

mod archive;
mod cache;
mod config;
mod doctor;
mod exit;
mod globaljson;
mod install;
mod installs;
mod lock;
mod lookup;
mod net;
mod paths;
mod platform;
mod releases;
mod resolve;
mod roots;
mod selfupdate;
mod shims;
mod version;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::config::Config;
use crate::globaljson::RollForward;
use crate::resolve::{Request, Resolution};
use crate::version::{Selector, Version};

#[derive(Parser)]
#[command(
    name = "dv",
    version,
    about = "Install and switch .NET SDK versions in seconds",
    disable_help_subcommand = true
)]
struct Cli {
    /// Output format for the commands a script would call
    #[arg(long, value_enum, default_value_t = Format::Text, global = true)]
    format: Format,
    #[command(subcommand)]
    command: Command,
}

/// How a command reports its result.
///
/// The rule that makes `json` worth anything: **data on stdout, diagnostics on
/// stderr, always**. If a progress line or a warning can land in stdout, the
/// JSON is unparseable and a caller cannot tell a corrupt install from a
/// chatty one. So `json` also suppresses the human-facing chatter entirely —
/// it is a machine's view, not a decorated one.
#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    Text,
    Json,
}

impl Format {
    fn is_json(self) -> bool {
        self == Format::Json
    }

    /// Print a value as JSON, or run the human-facing branch.
    fn emit(self, value: serde_json::Value, text: impl FnOnce()) -> Result<()> {
        if self.is_json() {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            text();
        }
        Ok(())
    }
}

#[derive(Subcommand)]
enum Command {
    /// Install an SDK: 10, 10.0, 10.0.4xx, 10.0.402, latest or lts — or, with
    /// no version, whatever global.json here asks for
    Install {
        /// Version or line to install
        version: Option<String>,
        /// Reinstall even when the same archive is already installed
        #[arg(long)]
        force: bool,
    },
    /// Remove an installed SDK
    Uninstall {
        /// Version or line to remove
        version: String,
    },
    /// List installed SDKs
    List(ListArgs),
    /// Pin an SDK in global.json in the current directory
    Pin {
        /// Version or line to pin
        version: String,
    },
    /// Show or set the SDK used when nothing pins one
    Default {
        /// Version or line to make the default; omit to show the current one
        version: Option<String>,
    },
    /// Print the binary that would run here, and nothing else
    Which {
        /// Command to resolve
        #[arg(default_value = "dotnet")]
        command: String,
    },
    /// Run a command under the resolved SDK
    #[command(alias = "exec")]
    Run {
        /// dotnet, dnx, a global tool, or anything on PATH
        command: String,
        /// Arguments passed through untouched
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the SDK version that would be used here, and where it came from
    Resolve {
        /// Print nothing unless global.json or the environment pins a version
        #[arg(long)]
        pinned_only: bool,
        /// Also print what chose it
        #[arg(long)]
        source: bool,
    },
    /// Print shell setup (add `eval "$(dv init zsh)"` to your profile)
    Init {
        /// zsh, bash, sh, fish or powershell
        #[arg(default_value = DEFAULT_SHELL)]
        shell: String,
        /// Also emit an optional hook that switches version on cd
        #[arg(long)]
        hook: bool,
    },
    /// Rebuild the shims and install roots
    Rehash,
    /// Inspect or clear the download cache
    #[command(subcommand)]
    Cache(CacheCommand),
    /// Check the installation and report anything that would fail silently
    Doctor,
    /// Manage dv itself
    #[command(subcommand)]
    #[command(name = "self")]
    Zelf(SelfCommand),
}

/// The shell `dv init` assumes when it is not told.
const DEFAULT_SHELL: &str = if cfg!(windows) { "powershell" } else { "zsh" };

#[derive(Args)]
struct ListArgs {
    /// List what can be installed, alongside what is installed
    #[arg(long)]
    remote: bool,
    /// With --remote, include end-of-life channels and every published patch
    #[arg(long)]
    all: bool,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Show what the cache holds
    Status,
    /// Remove cached archives no installed SDK came from
    Prune,
    /// Remove every cached archive
    Clear,
}

#[derive(Subcommand)]
enum SelfCommand {
    /// Replace this binary with the newest published build
    Update {
        /// Reinstall even when this is already the newest build
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            // Diagnostics on stderr, always: stdout belongs to the data a
            // caller is parsing, even when the command fails.
            eprintln!("dv: {err:#}");
            ExitCode::from(exit::code_for(&err))
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let format = cli.format;
    match cli.command {
        Command::Install { version, force } => cmd_install(version.as_deref(), force, format),
        Command::Uninstall { version } => cmd_uninstall(&version, format),
        Command::List(args) => cmd_list(args.remote, args.all, format),
        Command::Pin { version } => cmd_pin(&version, format),
        Command::Default { version } => cmd_default(version.as_deref(), format),
        Command::Which { command } => cmd_which(&command, format),
        Command::Run { command, args } => cmd_run(&command, &args),
        Command::Resolve {
            pinned_only,
            source,
        } => cmd_resolve(pinned_only, source, format),
        Command::Init { shell, hook } => cmd_init(&shell, hook),
        Command::Rehash => cmd_rehash(),
        Command::Cache(command) => cmd_cache(command),
        Command::Doctor => cmd_doctor(format),
        Command::Zelf(SelfCommand::Update { force }) => {
            selfupdate::update(force)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Resolve using the machine's real state.
fn current_resolution() -> Result<Resolution> {
    let config = Config::load()?;
    let installed = installs::installed()?;
    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let environment = std::env::var(resolve::ENVIRONMENT).ok();
    resolve::resolve(&Request {
        cwd: &cwd,
        installed: &installed,
        default: config.default.as_deref(),
        environment: environment.as_deref(),
    })
}

fn cmd_install(version: Option<&str>, force: bool, format: Format) -> Result<ExitCode> {
    let target = match version {
        Some(version) => install::Target::Selector(version.parse()?),
        None => install::Target::Request(requested_here()?),
    };
    let outcome = install::install(&target, force)?;
    let root = installs::root(&outcome.version)?;
    format.emit(
        serde_json::json!({
            "version": outcome.version.to_string(),
            "changed": outcome.changed,
            "root": root.display().to_string(),
        }),
        || {
            if outcome.changed {
                println!("installed .NET SDK {}", outcome.version);
            } else {
                println!(
                    ".NET SDK {} is already installed — `dv install {} --force` reinstalls it",
                    outcome.version, outcome.version
                );
            }
        },
    )?;
    if outcome.changed && !format.is_json() {
        print_path_advice()?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What the `global.json` here asks for, for a bare `dv install`.
fn requested_here() -> Result<globaljson::SdkRequest> {
    let cwd = std::env::current_dir().context("could not read the current directory")?;
    match globaljson::nearest(&cwd)? {
        Some(file) => match file.request {
            Ok(request) if request.is_pin() => Ok(request),
            Ok(_) => bail!(
                "{} does not name an SDK version — say which to install, as in \
                 `dv install lts`",
                file.path.display()
            ),
            Err(reason) => bail!("{} is not usable: {reason}", file.path.display()),
        },
        None => bail!(
            "no global.json here or above to read a version from — say which to install, \
             as in `dv install lts`"
        ),
    }
}

/// Everything a fresh install needs to hear, and nothing it does not.
fn print_path_advice() -> Result<()> {
    let status = shims::path_status("dotnet")?;
    if !status.on_path {
        println!();
        if cfg!(windows) {
            println!("dv's shims are not on PATH yet. Add this to your PowerShell profile:");
            println!("  dv init powershell | Out-String | Invoke-Expression");
            println!("then open a new terminal. `dv doctor` will confirm it.");
            return Ok(());
        }
        println!("dv's shims are not on PATH yet. Add this to your shell profile:");
        println!("  eval \"$(dv init zsh)\"");
        println!("then open a new shell. `dv doctor` will confirm it.");
        return Ok(());
    }
    if !status.shadowed_by.is_empty() {
        let owners: Vec<String> = status
            .shadowed_by
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        println!();
        println!(
            "warning: {} comes before dv's shims on PATH and owns the name `dotnet`.",
            owners.join(", ")
        );
        println!("Run `dv doctor` for the fix.");
        return Ok(());
    }
    // Shells cache command locations, so `dotnet` may still resolve to a path
    // recorded before the shim existed — or to nothing at all.
    if cfg!(windows) {
        println!("If `dotnet` still points somewhere else, open a new terminal.");
    } else {
        println!("If `dotnet` still points somewhere else, run `hash -r` or open a new shell.");
    }
    Ok(())
}

fn cmd_uninstall(version: &str, format: Format) -> Result<ExitCode> {
    let selector: Selector = version.parse()?;
    let installed = installs::installed()?;
    let Some(target) = selector.best(&installed) else {
        return Err(exit::Failed::new(
            exit::NOT_INSTALLED,
            format!(".NET SDK {selector} is not installed — `dv list` shows what is"),
        ));
    };
    let _lock = lock::acquire(
        &paths::versions_dir()?,
        &target.to_string(),
        &format!("installing .NET SDK {target}"),
    )?;
    installs::remove(&target)?;
    roots::sync(&[])?;
    shims::sync()?;

    let remaining = installs::installed()?;
    format.emit(
        serde_json::json!({ "version": target.to_string(), "removed": true }),
        || {
            println!("removed .NET SDK {target}");
            if remaining.is_empty() {
                println!("no .NET SDK installed now — `dv install lts` gets one back");
            }
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Patches per feature band shown by `dv list --remote` without `--all`.
const REMOTE_PATCHES_PER_BAND: usize = 2;

fn cmd_list(remote: bool, all: bool, format: Format) -> Result<ExitCode> {
    let installed = installs::installed()?;
    let active = current_resolution().ok();
    let is_active = |version: &Version| {
        active
            .as_ref()
            .is_some_and(|resolution| resolution.version == *version)
    };
    let installed_json: Vec<serde_json::Value> = installed
        .iter()
        .rev()
        .map(|version| {
            serde_json::json!({
                "version": version.to_string(),
                "active": is_active(version),
                "path": installs::root(version).ok().map(|p| p.display().to_string()),
            })
        })
        .collect();
    let active_json = active.as_ref().map(|resolution| {
        serde_json::json!({
            "version": resolution.version.to_string(),
            "source": resolution.source.kind(),
            "source_path": resolution.source.path().map(|p| p.display().to_string()),
        })
    });

    if remote {
        return cmd_list_remote(all, format, &installed, installed_json, active_json);
    }

    if installed.is_empty() {
        format.emit(
            serde_json::json!({ "installed": [], "active": null }),
            || println!("no .NET SDK installed — run `dv install lts`"),
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    // Marking the active one is the reason this command exists.
    format.emit(
        serde_json::json!({ "installed": installed_json, "active": active_json }),
        || {
            for version in installed.iter().rev() {
                if is_active(version) {
                    let source = active.as_ref().expect("active is some").source.to_string();
                    println!("* {version}  ({source})");
                } else {
                    println!("  {version}");
                }
            }
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

/// What can be installed, with what is installed, in one answer.
///
/// One call rather than two because a caller drawing a version picker needs
/// both, and two calls can disagree about a version installed in between.
fn cmd_list_remote(
    all: bool,
    format: Format,
    installed: &[Version],
    installed_json: Vec<serde_json::Value>,
    active_json: Option<serde_json::Value>,
) -> Result<ExitCode> {
    let config = Config::load()?;
    let rid = platform::current()?;
    let index = releases::fetch_index(&config.releases_url())?;

    // Supported channels, plus any channel something installed belongs to —
    // an installed SDK on an end-of-life channel is still worth showing next
    // to its newer patches.
    let mut channels: Vec<&releases::Channel> = index
        .channels
        .iter()
        .filter(|channel| {
            all || !channel.is_eol()
                || installed
                    .iter()
                    .any(|version| channel.number() == Some(version.channel()))
        })
        .collect();
    channels.sort_by_key(|channel| std::cmp::Reverse(channel.number()));

    let mut available: Vec<(releases::Sdk, &releases::Channel)> = Vec::new();
    for channel in &channels {
        // One channel whose metadata cannot be read should not hide every
        // other channel from a version picker. Installing stays strict: a
        // wrong answer there is worse than an error.
        let sdks = match releases::fetch_channel(channel) {
            Ok(sdks) => sdks,
            Err(err) if exit::code_for(&err) != exit::NETWORK => {
                eprintln!("dv: skipping the {} channel: {err:#}", channel.version);
                continue;
            }
            Err(err) => return Err(err),
        };
        for sdk in sdks {
            if sdk.archive_for(&rid).is_some() {
                available.push((sdk, channel));
            }
        }
    }
    available.sort_by(|a, b| b.0.version.cmp(&a.0.version));

    // Trim the listing, never the catalogue: older patches stay installable
    // by exact version, and anything installed is shown regardless of age —
    // hiding a version the user is standing on would read as it having been
    // withdrawn.
    let versions: Vec<Version> = available
        .iter()
        .map(|(sdk, _)| sdk.version.clone())
        .collect();
    let shown: Vec<Version> = if all {
        versions.clone()
    } else {
        let mut shown = version::newest_per_band(&versions, REMOTE_PATCHES_PER_BAND);
        for version in installed {
            if versions.contains(version) && !shown.contains(version) {
                shown.push(version.clone());
            }
        }
        shown
    };
    let hidden = versions.len() - shown.len();

    // JSON always carries every patch of the channels fetched: the trimming
    // exists to keep a terminal readable, and handing a caller a subset would
    // make it miss versions it can perfectly well install.
    format.emit(
        serde_json::json!({
            "platform": rid,
            "available": available.iter().map(|(sdk, channel)| serde_json::json!({
                "version": sdk.version.to_string(),
                "installed": installed.contains(&sdk.version),
                "prerelease": sdk.version.is_prerelease(),
                "channel": sdk.channel,
                "support_phase": channel.support_phase,
                "release_type": channel.release_type,
                "eol_date": channel.eol_date,
                "release_date": sdk.release_date,
                "runtime_version": sdk.runtime_version,
            })).collect::<Vec<_>>(),
            "installed": installed_json,
            "active": active_json,
        }),
        || {
            let mut current_channel = String::new();
            for (sdk, channel) in &available {
                if !shown.contains(&sdk.version) {
                    continue;
                }
                if sdk.channel != current_channel {
                    current_channel = sdk.channel.clone();
                    println!(
                        "{}{} ({} {})",
                        if current_channel == available[0].0.channel {
                            ""
                        } else {
                            "\n"
                        },
                        channel.version,
                        channel.release_type.to_uppercase(),
                        channel.support_phase,
                    );
                }
                let marker = if installed.contains(&sdk.version) {
                    "  (installed)"
                } else {
                    ""
                };
                println!("  {}{marker}", sdk.version);
            }
            if hidden > 0 {
                println!(
                    "\n{hidden} older {} not shown — `dv list --remote --all` lists them, \
                     and any of them installs by exact version",
                    if hidden == 1 { "patch" } else { "patches" }
                );
            }
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

/// The `global.json` an SDK selector becomes.
///
/// An exact version pins exactly — the reason to type one out. A line pins
/// the line, with the roll-forward policy that means "newest of it": `10.0`
/// accepts any 10.0 SDK, `10.0.4xx` any patch in that feature band.
fn pin_for(selector: &Selector, installed: &[Version]) -> Result<(Version, RollForward)> {
    let (base, policy) = match selector {
        Selector::Exact(version) => return Ok((version.clone(), RollForward::Disable)),
        Selector::Band(major, minor, band) => (
            format!("{major}.{minor}.{band}00"),
            RollForward::LatestPatch,
        ),
        Selector::Channel(major, minor) => {
            (format!("{major}.{minor}.100"), RollForward::LatestFeature)
        }
        Selector::Major(major) => (format!("{major}.0.100"), RollForward::LatestMinor),
        Selector::Lts => match selector.best(installed) {
            Some(version) => (
                format!("{}.0.100", version.major()),
                RollForward::LatestMinor,
            ),
            None => bail!(
                "no LTS SDK is installed to pin — pin a version such as `dv pin 10.0`, or \
                 `dv install lts` first"
            ),
        },
        Selector::Latest => bail!(
            "`latest` is what happens without a pin — pin a version or line instead, \
             as in `dv pin 10.0`"
        ),
    };
    let mut version: Version = base.parse()?;
    // A line with only a preview installed: the preview sorts below the line's
    // first release, so pinning the release number would exclude the very SDK
    // that is here. Pin from the preview instead.
    if let Some(best) = selector.best(installed)
        && best.is_prerelease()
        && best < version
    {
        version = best;
    }
    Ok((version, policy))
}

fn cmd_pin(version: &str, format: Format) -> Result<ExitCode> {
    let selector: Selector = version.parse()?;
    let installed = installs::installed()?;
    let (pinned, policy) = pin_for(&selector, &installed)?;

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let path = cwd.join(globaljson::FILE);
    let shadowed = if path.is_file() {
        None
    } else {
        cwd.parent().and_then(globaljson::find)
    };
    let dropped_comments = globaljson::write_pin(&path, &pinned, policy)?;

    let request = globaljson::SdkRequest {
        version: Some(pinned.clone()),
        roll_forward: policy,
        allow_prerelease: true,
    };
    let satisfied = request.select(&installed);
    format.emit(
        serde_json::json!({
            "path": path.display().to_string(),
            "version": pinned.to_string(),
            "roll_forward": policy.name(),
            "installed": satisfied.as_ref().map(ToString::to_string),
        }),
        || {
            println!(
                "pinned {pinned} (rollForward: {policy}) in {}",
                globaljson::FILE
            );
            if dropped_comments {
                println!("note: comments in {} could not be kept", globaljson::FILE);
            }
            if let Some(parent) = &shadowed {
                println!(
                    "note: this {} now shadows {} — anything else in it, such as msbuild-sdks, \
                     no longer applies here",
                    globaljson::FILE,
                    parent.display()
                );
            }
            if satisfied.is_none() {
                println!("no installed SDK satisfies it yet — run `dv install` here");
            }
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_default(version: Option<&str>, format: Format) -> Result<ExitCode> {
    let mut config = Config::load()?;
    let Some(version) = version else {
        format.emit(
            serde_json::json!({ "default": config.default }),
            || match &config.default {
                Some(default) => println!("{default}"),
                None => println!("no default set — dv falls back to the newest installed SDK"),
            },
        )?;
        return Ok(ExitCode::SUCCESS);
    };

    let selector: Selector = version.parse()?;
    config.default = Some(selector.to_string());
    config.save()?;
    let installed = installs::installed()?;
    let satisfied = selector.best(&installed);
    format.emit(
        serde_json::json!({
            "default": selector.to_string(),
            "installed": satisfied.as_ref().map(ToString::to_string),
        }),
        || {
            println!("default is now {selector}");
            if satisfied.is_none() {
                println!(".NET SDK {selector} is not installed yet — run `dv install {selector}`");
            }
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_which(command: &str, format: Format) -> Result<ExitCode> {
    // Read-only, always: resolve and print, never install, never create state.
    // Shims depend on this, and so do scripts asking "what would run here?".
    let resolution = current_resolution()?;
    let located = locate(&resolution.version, command)?;
    format.emit(
        serde_json::json!({
            "command": command,
            "path": located.path.display().to_string(),
            "version": resolution.version.to_string(),
            "from": located.from.as_str(),
        }),
        || println!("{}", located.path.display()),
    )?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_run(command: &str, args: &[String]) -> Result<ExitCode> {
    let resolution = current_resolution()?;
    let located = locate(&resolution.version, command)?;
    let root = installs::root(&resolution.version)?;

    let mut process = std::process::Command::new(&located.path);
    process.args(args);

    // App hosts — global tools, `dotnet run`'s output, anything built with
    // UseAppHost — find their runtime through DOTNET_ROOT, and prefer the
    // architecture-specific variable when one is set. Both point at the
    // resolved root, so what the SDK builds runs on runtimes dv installed.
    process.env("DOTNET_ROOT", &root);
    if let Some(arch) = platform::root_variable_arch() {
        process.env(format!("DOTNET_ROOT_{arch}"), &root);
    }
    // Windows-only legacy: without this, an old host also searches the
    // machine-wide install and can pick an SDK dv did not choose.
    process.env("DOTNET_MULTILEVEL_LOOKUP", "0");

    if located.from != From::Version {
        // A tool found outside the root. Two things keep everything it starts
        // on the same SDK: the root goes first on PATH, so a bare `dotnet` it
        // runs lands on the real host rather than back in a shim; and the
        // version is pinned in the environment, so anything that does reach a
        // shim resolves the same way rather than re-deriving it from a
        // directory it may have changed into.
        let mut entries = vec![root.clone()];
        entries.extend(lookup::entries());
        process.env("PATH", std::env::join_paths(entries)?);
        process.env(resolve::ENVIRONMENT, resolution.version.to_string());
    }

    hand_over(process).with_context(|| format!("could not run {}", located.path.display()))
}

/// Become the command: exec, so signals, exit codes and job control belong to
/// it rather than to a dv process sitting in the middle.
#[cfg(unix)]
fn hand_over(mut process: std::process::Command) -> std::io::Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    Err(process.exec())
}

/// Windows has no exec, so dv stays in the middle and does as little there as
/// it can: wait for the command, then leave with its exit code.
#[cfg(windows)]
fn hand_over(mut process: std::process::Command) -> std::io::Result<ExitCode> {
    const CTRL_BREAK_EVENT: u32 = 1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    // Ctrl+C and Ctrl+Break reach every process attached to the console, the
    // command included. What they mean is the command's decision — `dotnet
    // watch` survives one — so dv claims to have handled them and keeps
    // waiting. A handler function rather than the "ignore Ctrl+C" flag,
    // because that flag is inherited and the command would ignore it too.
    unsafe extern "system" fn leave_it_to_the_command(event: u32) -> i32 {
        (event <= CTRL_BREAK_EVENT) as i32
    }
    // SAFETY: the handler is a plain function that touches no state.
    unsafe { SetConsoleCtrlHandler(Some(leave_it_to_the_command), 1) };

    let status = process.status()?;
    // Not ExitCode, which is a u8: Windows exit codes are 32 bits wide, and a
    // crash is reported as one that does not fit.
    std::process::exit(status.code().unwrap_or(1))
}

/// Where a located command came from. The names are interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum From {
    /// The resolved SDK's install root.
    Version,
    /// A global tool, from `dotnet tool install -g`.
    Tool,
    /// Somewhere else on PATH.
    Path,
}

impl From {
    fn as_str(self) -> &'static str {
        match self {
            From::Version => "version",
            From::Tool => "tool",
            From::Path => "path",
        }
    }
}

struct Located {
    path: PathBuf,
    from: From,
}

/// Where `command` lives: the resolved root first, then global tools, then
/// PATH.
///
/// The fallbacks are what make `dv run dotnet-ef` and `dv run my-script` work:
/// dv has no business owning those tools, but it does own which SDK and
/// runtimes they find, and that is the part people get wrong.
fn locate(version: &Version, command: &str) -> Result<Located> {
    let path = installs::root(version)?.join(installs::command_file(command));
    if path.is_file() {
        return Ok(Located {
            path,
            from: From::Version,
        });
    }

    if let Some(path) = lookup::find_in(&paths::global_tools_dir()?, command) {
        return Ok(Located {
            path,
            from: From::Tool,
        });
    }

    if let Some(path) = lookup::on_path_excluding_shims(command)? {
        return Ok(Located {
            path,
            from: From::Path,
        });
    }

    if installs::is_installed(version)?
        && !installs::root(version)?
            .join(installs::command_file("dotnet"))
            .is_file()
    {
        bail!(
            "the install root for .NET SDK {version} is incomplete — run `dv rehash` to \
             rebuild it"
        );
    }
    let available = installs::commands(version)?;
    bail!(
        "`{command}` is not provided by .NET SDK {version}, is not a global tool, and is \
         not on PATH.\n.NET SDK {version} provides: {}",
        if available.is_empty() {
            format!("nothing — the install looks broken, try `dv install {version} --force`")
        } else {
            available.join(", ")
        }
    )
}

/// Print the resolved version for scripts and for the shell hook.
///
/// Read-only, like `which`. `--pinned-only` prints nothing when the version
/// comes from a fallback rather than a pin: the hook exports what this prints,
/// and exporting a fallback would freeze it, outranking the `global.json` of
/// every directory the shell later moves into.
fn cmd_resolve(pinned_only: bool, source: bool, format: Format) -> Result<ExitCode> {
    let resolution = match current_resolution() {
        Ok(resolution) => resolution,
        // Nothing resolves here. For the hook that means "clear the override",
        // which is a silent success rather than an error the shell must cope
        // with on every prompt.
        Err(_) if pinned_only => {
            if format.is_json() {
                println!("null");
            }
            return Ok(ExitCode::SUCCESS);
        }
        Err(err) => return Err(err),
    };

    let pinned = resolution.source.is_pin();
    if pinned_only && !pinned {
        if format.is_json() {
            println!("null");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `source` is the field that answers "why am I getting this version?"
    // without a second command, which is the question a caller actually has.
    format.emit(
        serde_json::json!({
            "version": resolution.version.to_string(),
            "binary": locate(&resolution.version, "dotnet").ok().map(|l| l.path.display().to_string()),
            "root": installs::root(&resolution.version)?.display().to_string(),
            "source": resolution.source.kind(),
            "source_path": resolution.source.path().map(|p| p.display().to_string()),
            "pinned": pinned,
        }),
        || {
            if source {
                println!("{} ({})", resolution.version, resolution.source);
            } else {
                println!("{}", resolution.version);
            }
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_init(shell: &str, hook: bool) -> Result<ExitCode> {
    match shell {
        "zsh" | "bash" | "sh" | "fish" => {}
        // PATH only. The shims resolve the version for every command without
        // the hook, and a hook that goes wrong takes the whole shell with it.
        "powershell" | "pwsh" if hook => bail!(
            "dv has no cd-time hook for {shell} — the shims switch version per directory \
             without one, so `dv init {shell}` is all it needs"
        ),
        "powershell" | "pwsh" => {}
        other => {
            bail!("dv has no init snippet for `{other}` — try zsh, bash, sh, fish or powershell")
        }
    }
    print!(
        "{}",
        shims::init_snippet(shell, &paths::shims_dir()?, &paths::combined_root()?)
    );
    if hook {
        let dv = std::env::current_exe().context("could not determine dv's own path")?;
        print!("{}", shims::hook_snippet(shell, &dv));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_cache(command: CacheCommand) -> Result<ExitCode> {
    let entries = cache::entries()?;
    match command {
        CacheCommand::Status => {
            if entries.is_empty() {
                println!("cache is empty ({})", paths::cache_dir()?.display());
                return Ok(ExitCode::SUCCESS);
            }
            for entry in &entries {
                let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
                let use_marker = if entry.in_use { "  (in use)" } else { "" };
                println!("{:>10}  {name}{use_marker}", net::megabytes(entry.bytes));
            }
            println!(
                "\n{} in {} — `dv cache prune` removes the {} nothing is installed from",
                net::megabytes(cache::total_bytes(&entries)),
                paths::cache_dir()?.display(),
                entries.iter().filter(|entry| !entry.in_use).count(),
            );
        }
        CacheCommand::Prune => {
            let (removed, freed) = cache::remove(true)?;
            println!(
                "removed {removed} cached archives, freed {}",
                net::megabytes(freed)
            );
        }
        CacheCommand::Clear => {
            // Safe by construction: a cached archive is only ever a download
            // shortcut, and every installed tree stays where it is.
            let (removed, freed) = cache::remove(false)?;
            println!(
                "removed {removed} cached archives, freed {}",
                net::megabytes(freed)
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_rehash() -> Result<ExitCode> {
    roots::sync(&[])?;
    let written = shims::sync()?;
    if written.is_empty() {
        println!("no shims written — no .NET SDK installed yet");
    } else {
        println!("install roots rebuilt; shims: {}", written.join(", "));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_doctor(format: Format) -> Result<ExitCode> {
    let findings = doctor::run()?;
    let (report, healthy) = doctor::report(&findings);
    format.emit(
        serde_json::json!({
            "healthy": healthy,
            "findings": findings
                .iter()
                .map(|finding| serde_json::json!({
                    "level": finding.level.as_str(),
                    "headline": finding.headline,
                    "advice": finding.advice,
                }))
                .collect::<Vec<_>>(),
        }),
        || print!("{report}"),
    )?;
    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn an_exact_pin_is_exact() {
        let (version, policy) = pin_for(&"10.0.402".parse().unwrap(), &[]).unwrap();
        assert_eq!(version, v("10.0.402"));
        assert_eq!(policy, RollForward::Disable);
    }

    #[test]
    fn a_line_pin_rolls_forward_within_the_line() {
        let installed = [v("9.0.310"), v("10.0.113"), v("10.0.402")];
        let check = |selector: &str, version: &str, policy: RollForward| {
            let (pinned, rolled) = pin_for(&selector.parse().unwrap(), &installed).unwrap();
            assert_eq!((pinned, rolled), (v(version), policy), "{selector}");
        };
        check("10.0.4xx", "10.0.400", RollForward::LatestPatch);
        check("10.0", "10.0.100", RollForward::LatestFeature);
        check("10", "10.0.100", RollForward::LatestMinor);
        check("lts", "10.0.100", RollForward::LatestMinor);

        // Each pin accepts what it was made from.
        for selector in ["10.0.4xx", "10.0", "10"] {
            let (pinned, policy) = pin_for(&selector.parse().unwrap(), &installed).unwrap();
            let request = globaljson::SdkRequest {
                version: Some(pinned),
                roll_forward: policy,
                allow_prerelease: true,
            };
            assert_eq!(
                request.select(&installed),
                Some(v("10.0.402")),
                "{selector}"
            );
        }
    }

    #[test]
    fn a_line_with_only_a_preview_pins_from_the_preview() {
        let installed = [v("10.0.402"), v("11.0.100-rc.2.26504.105")];
        let (pinned, policy) = pin_for(&"11.0".parse().unwrap(), &installed).unwrap();
        assert_eq!(pinned, v("11.0.100-rc.2.26504.105"));
        let request = globaljson::SdkRequest {
            version: Some(pinned),
            roll_forward: policy,
            allow_prerelease: true,
        };
        assert_eq!(
            request.select(&installed),
            Some(v("11.0.100-rc.2.26504.105"))
        );
    }

    #[test]
    fn latest_is_not_a_pin() {
        assert!(pin_for(&Selector::Latest, &[v("10.0.402")]).is_err());
    }
}
