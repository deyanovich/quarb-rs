//! Command libraries (spec: Command Libraries). A library extends
//! the language with commands as an adapter extends it with
//! sources: a native unit, linked into the engine, whose commands
//! are declared before any query runs and reached by the library's
//! name as a prefix, `difflib:ratio("Tom")`, exactly as an
//! environment command carries its mount alias. A library needs no
//! mount: its commands are pure in the standard library's sense, so
//! a query using them runs the same wherever the library is linked
//! and is refused, naming the library, where it is not.
//!
//! The trait is the deliverable as much as any library: the
//! environment kinds (`python:`, `perl:`) will fill the same
//! synopsis for the commands they bind at mount time, and a native
//! library of DH algorithms the same again.

use crate::syntax::ast::{Arg, FnCall, Operand};
use crate::value::Value;

/// Which pipe a command rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipe {
    /// `| lib:cmd` — once per capsa, the topic the input.
    Plain,
    /// `@| lib:cmd` — once over the context, the stream the input.
    Agg,
    Both,
}

impl Pipe {
    pub fn admits(self, agg: bool) -> bool {
        match self {
            Pipe::Plain => !agg,
            Pipe::Agg => agg,
            Pipe::Both => true,
        }
    }
}

/// One operand of a command's synopsis, in order.
#[derive(Debug, Clone, Copy)]
pub struct OperandSpec {
    pub name: &'static str,
    pub doc: &'static str,
    /// The values the operand accepts, empty for any value.
    pub values: &'static [&'static str],
    pub required: bool,
}

/// One option of a command's synopsis: `-n` and `--count`, and
/// whether a value follows the `=`.
#[derive(Debug, Clone, Copy)]
pub struct OptionSpec {
    pub short: Option<&'static str>,
    /// The long name, written `--name`; the name the library sees.
    pub long: &'static str,
    pub takes_value: bool,
    pub doc: &'static str,
}

impl OptionSpec {
    fn matches(&self, written: &str) -> bool {
        written.strip_prefix("--") == Some(self.long) || self.short == Some(written)
    }

    fn spelling(&self) -> String {
        let mut s = String::new();
        if let Some(short) = self.short {
            s.push_str(short);
            s.push('/');
        }
        s.push_str("--");
        s.push_str(self.long);
        if self.takes_value {
            s.push_str("=…");
        }
        s
    }
}

/// Which parameter the pipe fills (spec: The input slot): an
/// operand by position (the first, by default), or an option by its
/// long name. `$_` among the operands or option values places the
/// input itself, and the slot is then left to what was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSlot {
    Operand(usize),
    /// The operand by position, or, when every operand is written,
    /// the receiver: the input is then an instance of the class the
    /// path names (a fitted estimator before `.predict(X)`).
    OperandOrSelf(usize),
    Option(&'static str),
    /// The context itself, read by the engine (the graph library's
    /// row-aligned aggregates): no operand's place is taken, and
    /// `$_` places nothing.
    Context,
}

impl InputSlot {
    /// The operand position the input fills, when it fills one.
    pub fn operand_index(&self) -> Option<usize> {
        match self {
            InputSlot::Operand(i) | InputSlot::OperandOrSelf(i) => Some(*i),
            InputSlot::Option(_) | InputSlot::Context => None,
        }
    }
}

/// A command's declaration: what the parser checks a call against,
/// what the manual prints, what the library receives. A library
/// that reflects the package it wraps builds one from the
/// function's own signature: an operand named `...` admits surplus
/// operands (`*args`, or defaulted positional parameters), an
/// option named `*` admits any option (`**kwargs`), and the gate
/// then leaves what it cannot check to the function.
#[derive(Debug, Clone, Copy)]
pub struct Synopsis {
    pub name: &'static str,
    pub doc: &'static str,
    pub pipes: Pipe,
    pub operands: &'static [OperandSpec],
    pub options: &'static [OptionSpec],
    pub input: InputSlot,
    /// What the command returns, for people (`float`, `list of
    /// strings`).
    pub returns: &'static str,
}

impl Synopsis {
    /// Whether surplus operands are admitted (an operand `...`).
    pub fn variadic(&self) -> bool {
        self.operands.last().is_some_and(|o| o.name == "...")
    }

    /// Whether any option is admitted (an option `*`).
    pub fn open(&self) -> bool {
        self.options.iter().any(|o| o.long == "*")
    }

    /// The one-line synopsis a refusal quotes:
    /// `| difflib:close_matches(candidates; -n=… --cutoff=…) → list of strings`.
    pub fn render(&self, prefix: &str) -> String {
        let pipe = match self.pipes {
            Pipe::Plain => "|",
            Pipe::Agg => "@|",
            Pipe::Both => "| or @|",
        };
        let mut fields: Vec<String> = Vec::new();
        for (i, op) in self.operands.iter().enumerate() {
            if op.name == "..." {
                fields.push("…".to_string());
                continue;
            }
            let slot = self.input.operand_index() == Some(i);
            let mut f = if slot {
                format!("[{} = $_]", op.name)
            } else if op.required {
                op.name.to_string()
            } else {
                format!("[{}]", op.name)
            };
            if !op.values.is_empty() {
                f.push_str(&format!(" ({})", op.values.join("|")));
            }
            fields.push(f);
        }
        if !self.options.is_empty() {
            fields.push(
                self.options
                    .iter()
                    .map(|o| {
                        if o.long == "*" {
                            return "--…".to_string();
                        }
                        let mut s = o.spelling();
                        if self.input == InputSlot::Option(o.long) {
                            s.push_str(" = $_");
                        }
                        s
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        format!(
            "{pipe} {prefix}:{}({}) → {}",
            self.name,
            fields.join("; "),
            self.returns
        )
    }
}

/// What a library command receives: the pipe it rides, the input
/// (none when `$_` placed it among the written operands), the
/// operands in order, and the options by their long name.
#[derive(Debug, Clone)]
pub struct LibCall {
    pub agg: bool,
    pub input: Option<Value>,
    pub operands: Vec<Value>,
    pub options: Vec<(String, Option<Value>)>,
}

/// Why a call gave nothing back: the call failed (an exception; null
/// after `|`, a refusal after `@|`), or the name resolved to nothing
/// (a refusal wherever it stands).
#[derive(Debug, Clone)]
pub enum LibFailure {
    Failed(String),
    Unknown(String),
}

impl LibFailure {
    pub fn message(&self) -> &str {
        match self {
            LibFailure::Failed(m) | LibFailure::Unknown(m) => m,
        }
    }
}

/// A command library: its prefix, its declared commands, and the
/// call. Pure by contract: a call reads nothing outside the values
/// it is given.
pub trait CommandLibrary: Send + Sync {
    /// The declared name, the prefix every command carries.
    fn prefix(&self) -> &str;
    /// Every command, as the manual lists them.
    fn commands(&self) -> &[Synopsis];
    /// Run `name` with `call`.
    fn call(&self, name: &str, call: LibCall) -> Result<Value, LibFailure>;

    /// The run-time requirements the library declares beyond its
    /// own code (a Python package and its dictionaries), as the
    /// recipes' requirements line names them.
    fn requirements(&self) -> &[&str] {
        &[]
    }

    /// Whether the requirements are met here; `Err` names what is
    /// missing, and a query using the library is refused with it
    /// before the run.
    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    /// The synopsis of `name`, when declared.
    fn synopsis(&self, name: &str) -> Option<&Synopsis> {
        self.commands().iter().find(|s| s.name == name)
    }

    /// The synopsis of `name` for the gate and the call: the
    /// declared one, or, in a library that reflects the package it
    /// wraps, the one read from that package's own signature. `Err`
    /// is the refusal when there is none: the declared commands by
    /// default, what the package has in a reflective library.
    fn resolve(&self, name: &str) -> Result<&Synopsis, String> {
        self.synopsis(name).ok_or_else(|| {
            let names: Vec<&str> = self.commands().iter().map(|s| s.name).collect();
            format!(
                "library '{}' has no command '{name}'; its commands: {}",
                self.prefix(),
                names.join(", ")
            )
        })
    }
}

/// Whether an operand or option value is the topic variable `$_`,
/// which places the input itself.
pub fn places_input(call: &FnCall) -> bool {
    call.args.iter().any(|a| match a {
        Arg::Expr(Operand::Topic) => true,
        Arg::Opt {
            value: Some(Operand::Topic),
            ..
        } => true,
        _ => false,
    })
}

/// Check a written call against the library's synopsis, before the
/// run: the command exists, rides this pipe, has no operand or
/// option outside the synopsis and no required operand missing.
/// The refusal quotes the synopsis.
pub fn check(lib: &dyn CommandLibrary, name: &str, call: &FnCall, agg: bool) -> Result<(), String> {
    let prefix = lib.prefix();
    let syn = lib.resolve(name)?;
    let shown = syn.render(prefix);
    if !syn.pipes.admits(agg) {
        return Err(format!(
            "'{prefix}:{name}' rides {}; written after '{}'; the synopsis: {shown}",
            match syn.pipes {
                Pipe::Plain => "the plain pipe",
                Pipe::Agg => "the aggregation pipe",
                Pipe::Both => "either pipe",
            },
            if agg { "@|" } else { "|" }
        ));
    }
    let written_operands = call
        .args
        .iter()
        .filter(|a| !matches!(a, Arg::Opt { .. }))
        .count();
    // The input slot takes one operand's place unless `$_` fills it
    // by hand, or every operand is written and the input may stand
    // as the receiver instead.
    let fixed = syn.operands.len() - usize::from(syn.variadic());
    let slot_takes = match syn.input {
        InputSlot::Operand(_) => !places_input(call),
        InputSlot::OperandOrSelf(_) => !places_input(call) && written_operands < fixed,
        InputSlot::Option(_) | InputSlot::Context => false,
    };
    let room = fixed.saturating_sub(usize::from(slot_takes));
    if written_operands > room && !syn.variadic() {
        return Err(format!(
            "'{prefix}:{name}' takes {room} operand{}, {written_operands} given; the synopsis: {shown}",
            if room == 1 { "" } else { "s" }
        ));
    }
    // Where the written operands land among the synopsis's: in
    // order, stepping over the input's slot when the pipe fills it.
    let slot = syn.input.operand_index().filter(|_| slot_takes);
    let filled: Vec<usize> = (0..written_operands)
        .map(|j| match slot {
            Some(s) if j >= s => j + 1,
            _ => j,
        })
        .collect();
    let option_names: Vec<String> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Opt { name: opt, .. } => syn
                .options
                .iter()
                .find(|o| o.matches(opt))
                .map(|o| o.long.replace('-', "_")),
            _ => None,
        })
        .collect();
    // A parameter reachable both ways (spec: "Reflected commands")
    // is filled once: as an operand or by name, not both.
    for &i in &filled {
        if let Some(op) = syn.operands.get(i)
            && option_names.iter().any(|n| n == op.name)
        {
            return Err(format!(
                "'{prefix}:{name}' takes '{}' once: it is operand {} and --{}= here; the synopsis: {shown}",
                op.name,
                i + 1,
                op.name.replace('_', "-")
            ));
        }
    }
    // A parameter without a default is filled one way or the other.
    let missing: Vec<&str> = syn
        .operands
        .iter()
        .enumerate()
        .filter(|(i, op)| {
            op.required
                && op.name != "..."
                && Some(*i) != slot
                && !filled.contains(i)
                && !option_names.iter().any(|n| n == op.name)
        })
        .map(|(_, op)| op.name)
        .collect();
    if !missing.is_empty() {
        let required = syn
            .operands
            .iter()
            .enumerate()
            .filter(|(i, op)| op.required && op.name != "..." && Some(*i) != slot)
            .count();
        return Err(format!(
            "'{prefix}:{name}' needs {required} operand{}, {written_operands} given ({} unfilled); the synopsis: {shown}",
            if required == 1 { "" } else { "s" },
            missing.join(", ")
        ));
    }
    for arg in &call.args {
        if let Arg::Opt { name: opt, value } = arg {
            let Some(spec) = syn.options.iter().find(|o| o.matches(opt)) else {
                if syn.open() {
                    // The function takes any keyword argument; it
                    // judges this one.
                    continue;
                }
                return Err(format!(
                    "'{prefix}:{name}' has no option '{opt}'; the synopsis: {shown}"
                ));
            };
            if spec.takes_value && value.is_none() {
                return Err(format!(
                    "option '{opt}' of '{prefix}:{name}' takes a value ('{opt}=…'); the synopsis: {shown}"
                ));
            }
            if !spec.takes_value && value.is_some() {
                return Err(format!(
                    "option '{opt}' of '{prefix}:{name}' is a switch and takes no value; the synopsis: {shown}"
                ));
            }
        }
    }
    Ok(())
}

/// The written options by their long names, for the library.
pub fn canonical_options(
    syn: &Synopsis,
    written: Vec<(String, Option<Value>)>,
) -> Vec<(String, Option<Value>)> {
    written
        .into_iter()
        .map(|(name, value)| {
            let long = syn
                .options
                .iter()
                .find(|o| o.matches(&name))
                .map(|o| o.long.to_string())
                .unwrap_or_else(|| name.trim_start_matches('-').to_string());
            (long, value)
        })
        .collect()
}
