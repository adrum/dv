//! `dv doctor` — say plainly what is wrong and what to do about it.
//!
//! The checks here exist because each corresponding failure is silent: a
//! shadowed shim directory, a shell that cached a command location before the
//! shims existed, a `DOTNET_ROOT` that sends global tools to a different
//! install, a `global.json` the host quietly ignores. None of them announce
//! themselves; all of them look like "dv doesn't work".

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::globaljson;
use crate::resolve::{self, Request};
use crate::version::Version;
use crate::{installs, lookup, net, paths, platform, roots, shims};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Problem,
}

impl Level {
    /// Stable machine name, for `--format json`. Unlike `tag`, this is
    /// interface and must not be reworded.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Problem => "problem",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok      ",
            Level::Warn => "warn    ",
            Level::Problem => "problem ",
        }
    }
}

pub struct Finding {
    pub level: Level,
    pub headline: String,
    /// What to do about it. Printed indented under the headline.
    pub advice: Option<String>,
}

impl Finding {
    fn ok(headline: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            headline: headline.into(),
            advice: None,
        }
    }

    fn warn(headline: impl Into<String>, advice: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            headline: headline.into(),
            advice: Some(advice.into()),
        }
    }

    fn problem(headline: impl Into<String>, advice: impl Into<String>) -> Self {
        Self {
            level: Level::Problem,
            headline: headline.into(),
            advice: Some(advice.into()),
        }
    }
}

pub fn run() -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let config = Config::load()?;

    findings.push(match platform::current() {
        Ok(rid) => Finding::ok(format!("platform {rid}")),
        Err(err) => Finding::problem(err.to_string(), "dv cannot install .NET on this platform"),
    });

    findings.push(home_check(&paths::home()?));
    findings.extend(path_checks()?);
    findings.push(dotnet_root_check()?);
    if let Some(finding) = global_tools_check()? {
        findings.push(finding);
    }

    let installed = installs::installed()?;
    if installed.is_empty() {
        findings.push(Finding::problem(
            "no .NET SDK installed",
            "run `dv install lts`",
        ));
    } else {
        let list: Vec<String> = installed.iter().map(ToString::to_string).collect();
        findings.push(Finding::ok(format!(
            "{} installed: {}",
            installed.len(),
            list.join(", ")
        )));
        findings.extend(install_checks(&installed)?);
        findings.extend(root_checks(&installed)?);
    }

    findings.extend(global_json_checks(&installed)?);
    findings.push(resolution_check(&config, &installed)?);
    Ok(findings)
}

fn home_check(home: &Path) -> Finding {
    if !home.exists() {
        return Finding::warn(
            format!("{} does not exist yet", home.display()),
            "it is created on the first `dv install`",
        );
    }
    let probe = home.join(".dv-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Finding::ok(format!("dv home {}", home.display()))
        }
        Err(err) => Finding::problem(
            format!("dv home {} is not writable ({err})", home.display()),
            "check its ownership, or point DV_HOME somewhere you own",
        ),
    }
}

fn path_checks() -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let status = shims::path_status("dotnet")?;

    if !status.on_path {
        findings.push(Finding::problem(
            format!("{} is not on PATH", status.shims.display()),
            if cfg!(windows) {
                format!(
                    "add it, in front of everything else — in your PowerShell profile:\n      \
                     dv init powershell | Out-String | Invoke-Expression\n    \
                     or add this directory to the front of your user PATH:\n      {}",
                    status.shims.display()
                )
            } else {
                format!(
                    "add it, in front of everything else:\n      eval \"$(dv init zsh)\"\n    \
                     or add this line to your shell profile:\n      export PATH=\"{}:$PATH\"",
                    status.shims.display()
                )
            },
        ));
    } else if status.shadowed_by.is_empty() {
        findings.push(Finding::ok(format!(
            "{} comes first on PATH",
            status.shims.display()
        )));
    } else {
        let owners: Vec<String> = status
            .shadowed_by
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        findings.push(Finding::problem(
            format!(
                "another dotnet comes before dv's shims: {}",
                owners.join(", ")
            ),
            format!(
                "move {} earlier in PATH — `eval \"$(dv init zsh)\"` at the end of your \
                 profile does that",
                status.shims.display()
            ),
        ));
    }

    // What the shell would actually run, which is the question users are
    // really asking. A stale shell hash shows up here as a path that does not
    // exist, or an old one.
    match lookup::on_path("dotnet") {
        Some(found) if shims::is_dv_shim(&found)? => {
            findings.push(Finding::ok(format!(
                "dotnet on PATH is dv's shim ({})",
                found.display()
            )));
        }
        Some(found) => findings.push(Finding::warn(
            format!("dotnet on PATH is not a dv shim ({})", found.display()),
            if cfg!(windows) {
                "dv will not control that dotnet — fix PATH order, then open a new terminal"
            } else {
                "dv will not control that dotnet — fix PATH order, then run `hash -r`"
            },
        )),
        None => findings.push(Finding::warn(
            "no dotnet on PATH",
            if cfg!(windows) {
                "install an SDK with `dv install lts`, then open a new terminal"
            } else {
                "install an SDK with `dv install lts`, then run `hash -r` (shells cache \
                 command locations, so a shell open since before the install may still \
                 miss it)"
            },
        )),
    }

    Ok(findings)
}

/// `DOTNET_ROOT` decides where global tools and app-host executables look for
/// a runtime. Pointing it anywhere else sends them to an install dv does not
/// manage — they work, against runtimes dv cannot see or update.
fn dotnet_root_check() -> Result<Finding> {
    let combined = paths::combined_root()?;
    let advice = format!(
        "`dv init` sets it — or export DOTNET_ROOT=\"{}\" yourself",
        combined.display()
    );
    Ok(match std::env::var_os("DOTNET_ROOT") {
        None => Finding::warn(
            "DOTNET_ROOT is not set, so global tools and app hosts look for a system-wide .NET",
            advice,
        ),
        Some(value) if shims::same_dir(Path::new(&value), &combined) => Finding::ok(format!(
            "DOTNET_ROOT is dv's combined root ({})",
            combined.display()
        )),
        Some(value) => Finding::warn(
            format!(
                "DOTNET_ROOT is {}, so global tools and app hosts use that install, not dv's",
                Path::new(&value).display()
            ),
            advice,
        ),
    })
}

fn global_tools_check() -> Result<Option<Finding>> {
    let tools = paths::global_tools_dir()?;
    if !tools.is_dir() {
        return Ok(None);
    }
    let on_path = lookup::entries()
        .iter()
        .any(|entry| shims::same_dir(entry, &tools));
    Ok(Some(if on_path {
        Finding::ok(format!(
            "global tools directory {} is on PATH",
            tools.display()
        ))
    } else {
        Finding::warn(
            format!("global tools directory {} is not on PATH", tools.display()),
            "tools from `dotnet tool install -g` are only reachable by name with it on \
                 PATH — add it, or run them with `dv run <tool>`, which looks there"
                .to_string(),
        )
    }))
}

fn install_checks(installed: &[Version]) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for version in installed {
        match installs::read_record(version)? {
            None => findings.push(Finding::warn(
                format!("{version}: no install record"),
                format!(
                    "dv cannot tell what this was installed from — \
                     `dv install {version} --force` re-records it"
                ),
            )),
            Some(record) => {
                let changed = installs::changed_files(version, &record)?;
                if !changed.is_empty() {
                    findings.push(Finding::problem(
                        format!("{version}: changed since install — {}", changed.join(", ")),
                        format!("reinstall with `dv install {version} --force`"),
                    ));
                }
                let cached = paths::cache_dir()?.join(&record.file);
                if cached.is_file() && net::digest_file(&cached)?.sha512 != record.sha512 {
                    findings.push(Finding::problem(
                        format!(
                            "{version}: cached {} does not match its record",
                            record.file
                        ),
                        format!(
                            "delete {} and run `dv install {version} --force`",
                            cached.display()
                        ),
                    ));
                }
            }
        }
    }
    if findings.is_empty() {
        findings.push(Finding::ok("installed SDKs look intact"));
    }
    Ok(findings)
}

fn root_checks(installed: &[Version]) -> Result<Vec<Finding>> {
    let mut broken = Vec::new();
    let mut check = |label: String, root: &Path| -> Result<()> {
        if !roots::host(root).is_file() {
            broken.push(format!("{label} (no dotnet host)"));
        } else {
            let missing = roots::missing_entries(root)?;
            if !missing.is_empty() {
                broken.push(format!("{label} (missing {})", missing.join(", ")));
            }
        }
        Ok(())
    };
    for version in installed {
        check(version.to_string(), &installs::root(version)?)?;
    }
    check("combined".to_string(), &paths::combined_root()?)?;

    Ok(vec![if broken.is_empty() {
        Finding::ok("install roots are complete")
    } else {
        Finding::problem(
            format!("install roots are incomplete: {}", broken.join("; ")),
            "run `dv rehash` to rebuild them",
        )
    }])
}

fn global_json_checks(installed: &[Version]) -> Result<Vec<Finding>> {
    let cwd = std::env::current_dir()?;
    let Some(file) = globaljson::nearest(&cwd)? else {
        return Ok(Vec::new());
    };
    let mut findings = Vec::new();
    let request = match &file.request {
        Err(reason) => {
            findings.push(Finding::warn(
                format!("{} is ignored by dotnet: {reason}", file.path.display()),
                "the host falls back to the newest SDK when it cannot read global.json — \
                 fix the file, or `dv pin <version>` rewrites its sdk section",
            ));
            return Ok(findings);
        }
        Ok(request) => request,
    };

    let raw = std::fs::read_to_string(&file.path).unwrap_or_default();
    if raw.contains("\"paths\"") {
        findings.push(Finding::warn(
            format!("{} sets sdk.paths", file.path.display()),
            "dotnet searches those directories for an SDK instead of, or before, the one \
             dv resolved — include \"$host$\" in the list to let dv's SDK be found",
        ));
    }

    // An override that global.json does not accept makes the host refuse to
    // start: dv runs an install root holding only the overriding SDK.
    if let Ok(raw) = std::env::var(resolve::ENVIRONMENT)
        && !raw.trim().is_empty()
        && request.is_pin()
        && let Ok(selector) = raw.parse::<crate::version::Selector>()
        && let Some(version) = selector.best(installed)
        && !request.matches(&version)
    {
        findings.push(Finding::problem(
            format!(
                "{} selects {version}, which {} does not accept ({})",
                resolve::ENVIRONMENT,
                file.path.display(),
                request.describe()
            ),
            format!(
                "dotnet will refuse to start here — unset {} or change it",
                resolve::ENVIRONMENT
            ),
        ));
    }
    Ok(findings)
}

fn resolution_check(config: &Config, installed: &[Version]) -> Result<Finding> {
    let cwd = std::env::current_dir()?;
    let environment = std::env::var(resolve::ENVIRONMENT).ok();
    let request = Request {
        cwd: &cwd,
        installed,
        default: config.default.as_deref(),
        environment: environment.as_deref(),
    };
    Ok(match resolve::resolve(&request) {
        Ok(resolution) => Finding::ok(format!(
            "here, dotnet is SDK {} (from {})",
            resolution.version, resolution.source
        )),
        Err(err) => Finding::problem(format!("here, dotnet does not resolve: {err}"), "see above"),
    })
}

/// Render findings, and whether anything is actually broken.
pub fn report(findings: &[Finding]) -> (String, bool) {
    let mut out = String::new();
    let mut healthy = true;
    for finding in findings {
        let _ = writeln!(out, "{}{}", finding.level.tag(), finding.headline);
        if let Some(advice) = &finding.advice {
            let _ = writeln!(out, "        {advice}");
        }
        if finding.level == Level::Problem {
            healthy = false;
        }
    }
    (out, healthy)
}
