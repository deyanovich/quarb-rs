//! Environments: namespaces of commands the host mounts beside the
//! built-ins (spec: Environment Commands, Environment Mounts). An
//! environment command is `ALIAS:name(operands; options)`; the
//! prefix is required, and the mount is the opt-in — honoured only
//! from the host's side (`qua … go=exec:~/go/bin sh=shell:`), never
//! from query text.
//!
//! Every kind fills one contract: resolution (what a name denotes),
//! translation (operands as words in order, options first in the
//! callee's own form, the void option onto nothing) and framing
//! (after `|` once per capsa on the topic's text, stdout minus one
//! trailing newline as the value; after `@|` once over the context,
//! one line per capsa on stdin, every stdout line a value, each
//! carrying the shared regulae). The first kinds are the text ones,
//! `exec:` and `shell:`; `python:` is named here and refused until
//! the native framing lands.

use crate::ast::{Arg, FnCall};
use crate::value::Value;

/// A kind of environment: how its names resolve, how a call is
/// translated and framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvKind {
    /// `exec:DIR` — the programs of a directory, each a command
    /// under its file name; `exec:PROGRAM` — one program, a command
    /// under its own name.
    Exec,
    /// `shell:` — the user's shell (`$SHELL`, else `sh`): its
    /// functions and aliases beside the path. `--allow-shell` is
    /// `sh=shell:` under its older name.
    Shell,
}

/// One mounted environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// The prefix the pipeline uses (`sh` in `sh:tr`).
    pub alias: String,
    pub kind: EnvKind,
    /// The kind's target: a directory or a program for `exec:`,
    /// nothing for `shell:`.
    pub target: String,
}

/// The environment schemes, as a mount's `SCHEME:` prefix spells
/// them.
pub const SCHEMES: &[&str] = &["exec", "shell", "python"];

impl Environment {
    /// Whether a mount target names an environment rather than a
    /// source (`exec:…`, `shell:`, `python:…`).
    pub fn is_environment_target(target: &str) -> bool {
        target
            .split_once(':')
            .is_some_and(|(scheme, _)| SCHEMES.contains(&scheme))
    }

    /// Parse a host's mount `ALIAS=SCHEME:TARGET`. `None` when the
    /// argument is not an environment mount at all (a source);
    /// `Some(Err)` when it is one but malformed, or a kind not yet
    /// built.
    pub fn parse_mount(arg: &str) -> Option<crate::Result<Environment>> {
        let (alias, target) = arg.split_once('=')?;
        if !Self::is_environment_target(target) {
            return None;
        }
        Some(Self::parse(alias, target))
    }

    /// An environment from its alias and `SCHEME:TARGET`.
    pub fn parse(alias: &str, spec: &str) -> crate::Result<Environment> {
        let bad = |m: String| crate::QuarbError::parse(m);
        if !is_alias(alias) {
            return Err(bad(format!(
                "an environment's alias is a word ('{alias}' given): write 'ALIAS=SCHEME:TARGET'"
            )));
        }
        let (scheme, target) = spec.split_once(':').unwrap_or((spec, ""));
        let (target, _options) = target.split_once('?').unwrap_or((target, ""));
        match scheme {
            "exec" => {
                if target.is_empty() {
                    return Err(bad(format!(
                        "'{alias}=exec:' names no target: a directory of programs or one \
                         program ('{alias}=exec:~/go/bin')"
                    )));
                }
                let expanded = expand_home(target);
                if !std::path::Path::new(&expanded).exists() {
                    return Err(bad(format!(
                        "'{alias}=exec:{target}': no such directory or program"
                    )));
                }
                Ok(Environment {
                    alias: alias.to_string(),
                    kind: EnvKind::Exec,
                    target: expanded,
                })
            }
            "shell" => {
                if !target.is_empty() {
                    return Err(bad(format!(
                        "'{alias}=shell:' takes no target: it is the user's shell ('{target}' given)"
                    )));
                }
                Ok(Environment {
                    alias: alias.to_string(),
                    kind: EnvKind::Shell,
                    target: String::new(),
                })
            }
            "python" => Err(crate::QuarbError::unsupported(format!(
                "the python: environment ('{alias}=python:{target}'): the native framing \
                 arrives with the plugin round; exec: and shell: are built"
            ))),
            other => Err(bad(format!("'{other}:' is not an environment kind"))),
        }
    }

    /// The program the command `name` denotes, or the refusal that
    /// names the environment.
    fn resolve(&self, name: &str) -> crate::Result<Program> {
        match self.kind {
            EnvKind::Exec => {
                let path = std::path::Path::new(&self.target);
                if path.is_dir() {
                    let p = path.join(name);
                    if p.is_file() {
                        return Ok(Program::Path(p));
                    }
                    Err(crate::QuarbError::unsupported(format!(
                        "environment '{}' has no command '{name}' (exec:{})",
                        self.alias, self.target
                    )))
                } else if path.file_name().is_some_and(|f| f.to_string_lossy() == name)
                    || path
                        .file_stem()
                        .is_some_and(|f| f.to_string_lossy() == name)
                {
                    Ok(Program::Path(path.to_path_buf()))
                } else {
                    Err(crate::QuarbError::unsupported(format!(
                        "environment '{}' has one command, '{}' (exec:{}), not '{name}'",
                        self.alias,
                        path.file_stem().map(|f| f.to_string_lossy()).unwrap_or_default(),
                        self.target
                    )))
                }
            }
            EnvKind::Shell => Ok(Program::ShellWord(name.to_string())),
        }
    }

    /// The argument words a call translates to: operands first as
    /// their text, then — on the callee's side first — the options
    /// in shell form (`--name=value`, `-x value`), the void option
    /// onto nothing. The operands come evaluated: this is the text
    /// side of the translation.
    pub fn argv(operands: &[String], options: &[(String, Option<String>)]) -> Vec<String> {
        let mut words = Vec::new();
        for (name, value) in options {
            match value {
                Some(v) if name.starts_with("--") => words.push(format!("{name}={v}")),
                Some(v) => {
                    words.push(name.clone());
                    words.push(v.clone());
                }
                None => words.push(name.clone()),
            }
        }
        words.extend(operands.iter().cloned());
        words
    }

    /// Run the command once with `argv` and `stdin`, giving stdout.
    /// An unknown name is a resolution failure (127 from the shell
    /// counts as one); a nonzero exit is a failed call. Standard
    /// error passes through.
    pub fn call(&self, name: &str, argv: &[String], stdin: &str) -> Result<String, CallFailure> {
        let program = self.resolve(name).map_err(|e| CallFailure::Unknown(e.to_string()))?;
        let mut command = match &program {
            Program::Path(p) => {
                let mut c = std::process::Command::new(p);
                c.args(argv);
                c
            }
            Program::ShellWord(word) => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                let mut c = std::process::Command::new(shell);
                // `"$0" "$@"`: the word is the command the shell
                // resolves (a function exported through the
                // environment, an alias, a program on the path), the
                // argv its words, never re-parsed as a command line.
                c.arg("-c").arg("\"$0\" \"$@\"").arg(word).args(argv);
                c
            }
        };
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                CallFailure::Unknown(format!(
                    "environment '{}': cannot run '{name}': {e}",
                    self.alias
                ))
            })?;
        if let Some(pipe) = child.stdin.take() {
            let mut text = stdin.to_string();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            feed_stdin(pipe, text);
        }
        let out = child.wait_with_output().map_err(|e| {
            CallFailure::Failed(format!(
                "environment '{}': '{name}' did not finish: {e}",
                self.alias
            ))
        })?;
        if out.status.code() == Some(127) && matches!(program, Program::ShellWord(_)) {
            return Err(CallFailure::Unknown(format!(
                "environment '{}' has no command '{name}' (shell: command not found)",
                self.alias
            )));
        }
        if !out.status.success() {
            return Err(CallFailure::Failed(format!(
                "environment '{}': '{name}' failed ({})",
                self.alias, out.status
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Why a call gave nothing back: the name resolved to nothing (a
/// refusal wherever it stands), or the call failed (null after the
/// plain pipe, a refusal after the aggregation pipe).
#[derive(Debug, Clone)]
pub enum CallFailure {
    Unknown(String),
    Failed(String),
}

impl CallFailure {
    pub fn message(&self) -> &str {
        match self {
            CallFailure::Unknown(m) | CallFailure::Failed(m) => m,
        }
    }
}

/// Feed `text` to a child's standard input from a thread of its
/// own, so the parent drains the child's output meanwhile (a child
/// that echoes more than a pipe buffer would otherwise deadlock),
/// and the thread drops the pipe at the end, giving the child EOF.
/// A child that stops reading early (`head`, a program that fails)
/// closes the pipe under the writer: with SIGPIPE blocked on this
/// thread the write fails with EPIPE and the thread ends, where the
/// default disposition a Unix filter host restores (qua's, for its
/// own stdout) would kill the whole process silently.
pub(crate) fn feed_stdin(mut pipe: std::process::ChildStdin, text: String) {
    use std::io::Write;
    std::thread::spawn(move || {
        #[cfg(unix)]
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGPIPE);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let _ = pipe.write_all(text.as_bytes());
    });
}

/// What a name resolved to.
enum Program {
    Path(std::path::PathBuf),
    ShellWord(String),
}

/// An alias: a letter or `_`, then letters, digits, `_`, `-`.
fn is_alias(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// The environment prefix and command name of a call, when the call
/// is an environment command (`sh:tr` → `("sh", "tr")`).
pub fn split_env(name: &str) -> Option<(&str, &str)> {
    let (alias, cmd) = name.split_once(':')?;
    (!alias.is_empty() && !cmd.is_empty()).then_some((alias, cmd))
}

/// A call's options as the text side of translation sees them:
/// the name as written and the value's text, literal options only
/// (expression values are evaluated by the caller and substituted
/// before this point).
pub fn literal_options(call: &FnCall) -> Vec<(String, Option<String>)> {
    call.args
        .iter()
        .filter_map(|a| match a {
            Arg::Opt { name, value } => Some((
                name.clone(),
                value.as_ref().map(|v| match v {
                    crate::ast::Operand::Lit(l) => l.to_string(),
                    other => crate::unparse::operand_text(other),
                }),
            )),
            _ => None,
        })
        .collect()
}

/// The text one value takes on a text environment's standard input
/// after `@|`: a scalar as its text, a record or a list as its JSON
/// on one line.
pub fn line_of(v: &Value) -> String {
    match v {
        Value::Record(_) | Value::List(_) => v.to_json(),
        other => other.to_string(),
    }
}
