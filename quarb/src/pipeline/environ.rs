//! Environment commands: `ALIAS:name(operands; options)` run by the
//! environment the host mounted under the alias (spec: Environment
//! Commands). The text kinds frame as `sh` does: after `|` once per
//! capsa, the topic's text on stdin and stdout (one trailing newline
//! trimmed) the value; after `@|` once over the context, one line
//! per capsa on stdin and every stdout line a value, each carrying
//! the shared regulae.

use crate::pipeline::env::{CallFailure, Environment};
use crate::pipeline::*;

/// The call's operands and options as text, evaluated against
/// `node` under `scope`: the translation's text side.
fn words<H: Host>(
    call: &FnCall,
    host: &H,
    ext: &H::Ext,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> (Vec<String>, Vec<(String, Option<String>)>) {
    let text = |a: &Arg| -> Option<String> {
        match a {
            Arg::Lit(v) => Some(v.to_string()),
            Arg::Expr(e) => Some(operand_scalar(host, ext, e, trace, scope).to_string()),
            Arg::Range(a, b) => Some(format!(
                "{}..{}",
                a.map(|n| n.to_string()).unwrap_or_default(),
                b.map(|n| n.to_string()).unwrap_or_default()
            )),
            Arg::Named { .. } | Arg::Opt { .. } | Arg::Pairs { .. } => None,
        }
    };
    let operands: Vec<String> = call.args.iter().filter_map(text).collect();
    let options: Vec<(String, Option<String>)> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Opt { name, value } => Some((
                name.clone(),
                value.as_ref().map(|v| match v {
                    Operand::Lit(l) => l.to_string(),
                    e => operand_scalar(host, ext, e, trace, scope).to_string(),
                }),
            )),
            _ => None,
        })
        .collect();
    (operands, options)
}

/// The mounted environment a call names, or the refusal.
fn environment_of(call: &FnCall) -> Option<(Environment, String)> {
    let (alias, name) = crate::pipeline::env::split_env(&call.name)?;
    match run_option(|o| o.environment(alias).cloned()) {
        Some(env) => Some((env, name.to_string())),
        None => {
            record_refusal(format!(
                "environment '{alias}' is not mounted; mount it from the command line \\
                 ('{alias}=exec:DIR', '{alias}=shell:')"
            ));
            None
        }
    }
}

fn trim_one_newline(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

/// A library call's values: the operands in order (an expression
/// evaluating to several values is a list), the options by their
/// written names, and whether `$_` placed the input itself.
fn lib_values<H: Host>(
    call: &FnCall,
    host: &H,
    ext: &H::Ext,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> (Vec<Value>, Vec<(String, Option<Value>)>) {
    let eval = |e: &Operand| -> Value { operand_value(host, ext, e, trace, scope) };
    let operands: Vec<Value> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Lit(v) => Some(v.clone()),
            Arg::Expr(e) => Some(eval(e)),
            Arg::Range(a, b) => Some(Value::list(vec![
                a.map(Value::Int).unwrap_or(Value::Null),
                b.map(Value::Int).unwrap_or(Value::Null),
            ])),
            Arg::Named { .. } | Arg::Opt { .. } | Arg::Pairs { .. } => None,
        })
        .collect();
    let options = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Opt { name, value } => Some((
                name.clone(),
                value.as_ref().map(|v| match v {
                    Operand::Lit(l) => l.clone(),
                    e => eval(e),
                }),
            )),
            _ => None,
        })
        .collect();
    (operands, options)
}

/// What a node is to a library: the record of its name, its text
/// (`::`) and its properties — a snapshot the library sees as a
/// mapping, never a proxy into the arbor.
fn node_record<H: Host>(host: &H, ext: &H::Ext) -> Value {
    // A property named `text` fills the text field where the node
    // has no text of its own (a JSON object with a "text" member);
    // a node's own text always wins.
    let mut text = host.project(ext, &Projection::Property(None));
    let mut rest = Vec::new();
    for (k, v) in host.properties_of(ext) {
        if k == "text" {
            if matches!(text, Value::Null) {
                text = v;
            }
        } else if k != "name" {
            rest.push((k, v));
        }
    }
    let mut fields = vec![
        ("name".to_string(), host.node_scalar(ext)),
        ("text".to_string(), text),
    ];
    fields.extend(rest);
    Value::Record(fields)
}

/// `| LIB:name(…)` — a command library's command once per capsa:
/// the topic the input (a node its record) unless `$_` places it.
fn lib_command<H: Host>(
    lib: &dyn crate::pipeline::library::CommandLibrary,
    name: &str,
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let syn = match lib.resolve(name) {
        Ok(s) => s,
        Err(why) => {
            record_refusal(why);
            return Vec::new();
        }
    };
    let places = crate::pipeline::library::places_input(call);
    let mut out = Vec::with_capacity(caps.len());
    let mut warned = false;
    for (i, mut c) in caps.into_iter().enumerate() {
        let scope = c.scope(Some(i + 1), peers, outer);
        let ((operands, options), o) = capture(|| lib_values(call, host, &c.ext, trace, scope));
        let input = (!places).then(|| match &c.topic {
            Some(t) => t.clone(),
            None => node_record(host, &c.ext),
        });
        let lib_call = crate::pipeline::library::LibCall {
            agg: false,
            input,
            operands,
            options: crate::pipeline::library::canonical_options(syn, options),
        };
        c.topic = Some(match lib.call(name, lib_call) {
            Ok(v) => v,
            Err(crate::pipeline::library::LibFailure::Failed(m)) => {
                // Null, as a failed shell command; the text once, on
                // the message channel.
                if !warned {
                    eprintln!("{}:{name}: {m}", lib.prefix());
                    warned = true;
                }
                Value::Null
            }
            Err(crate::pipeline::library::LibFailure::Unknown(m)) => {
                record_refusal(m);
                return Vec::new();
            }
        });
        let mut prov = c.prov.flat();
        prov.origins.union(&o);
        c.prov = prov;
        out.push(c);
    }
    out
}

/// `@| LIB:name(…)` — once over the context, the stream the input:
/// a returned list is one capsa per element, a scalar one capsa,
/// each carrying the shared regulae; a failed call refuses.
fn lib_agg_command<H: Host>(
    lib: &dyn crate::pipeline::library::CommandLibrary,
    name: &str,
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let syn = match lib.resolve(name) {
        Ok(s) => s,
        Err(why) => {
            record_refusal(why);
            return Vec::new();
        }
    };
    let seed = seed_capsa(&caps, host);
    let scope = seed.scope(None, peers, outer);
    let ((operands, options), o) = capture(|| lib_values(call, host, &seed.ext, trace, scope));
    let mut origins = o;
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(host.origin_at(&c.ext)),
        }
    }
    let places = crate::pipeline::library::places_input(call);
    // A stream of one library value, table or list (the frame a
    // previous command built, a list literal) crosses as that value,
    // not as a list around it, so either pipe continues a chain and
    // `= @(1; 2) @| np:log` is the sample, not a one-row matrix.
    let input = (!places).then(|| match caps.as_slice() {
        [one]
            if matches!(
                one.topic,
                Some(Value::Foreign(_) | Value::Table(_) | Value::List(_))
            ) =>
        {
            one.topic.clone().expect("matched")
        }
        _ => Value::list(
            caps.iter()
                .map(|c| match &c.topic {
                    Some(t) => t.clone(),
                    None => node_record(host, &c.ext),
                })
                .collect(),
        ),
    });
    let lib_call = crate::pipeline::library::LibCall {
        agg: true,
        input,
        operands,
        options: crate::pipeline::library::canonical_options(syn, options),
    };
    let value = match lib.call(name, lib_call) {
        Ok(v) => v,
        Err(f) => {
            record_refusal(format!("{}:{name}: {}", lib.prefix(), f.message()));
            return Vec::new();
        }
    };
    let register = shared_register(&caps);
    let items = match value {
        Value::List(_) => value.into_items().unwrap_or_default(),
        other => vec![other],
    };
    items
        .into_iter()
        .map(|v| Thread::<H::Ext> {
            prov: Prov::leaf(origins.clone()),
            register: register.clone(),
            topic: Some(v),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            ext: seed.ext.bare(),
        })
        .collect()
}

/// `| ALIAS:name(…)` — once per capsa on the topic's text; a failed
/// call yields null, an unknown name refuses. `apply_command`'s arm.
pub(crate) fn env_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    // A linked library's prefix wins over a mount of the same name
    // (the host refuses such a mount).
    if let Some((prefix, name)) = crate::pipeline::env::split_env(&call.name)
        && let Some(lib) =
            run_option(|o| o.libraries.iter().find(|l| l.prefix() == prefix).cloned())
    {
        return lib_command(lib.as_ref(), name, call, caps, host, trace, outer, peers);
    }
    let Some((env, name)) = environment_of(call) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(caps.len());
    for (i, mut c) in caps.into_iter().enumerate() {
        let scope = c.scope(Some(i + 1), peers, outer);
        let ((operands, options), o) = capture(|| words(call, host, &c.ext, trace, scope));
        let argv = Environment::argv(&operands, &options);
        let input = effective_topic(&c, host).to_string();
        c.topic = Some(match env.call(&name, &argv, &input) {
            Ok(text) => Value::Str(trim_one_newline(text)),
            Err(CallFailure::Failed(_)) => Value::Null,
            Err(CallFailure::Unknown(m)) => {
                record_refusal(m);
                return Vec::new();
            }
        });
        let mut prov = c.prov.flat();
        prov.origins.union(&o);
        c.prov = prov;
        out.push(c);
    }
    out
}

/// `@| ALIAS:name(…)` — once over the context: one line per capsa on
/// stdin, every stdout line a capsa carrying the shared regulae; a
/// failed call refuses, the whole stream having failed.
/// `apply_command`'s arm.
pub(crate) fn env_agg_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    if let Some((prefix, name)) = crate::pipeline::env::split_env(&call.name)
        && let Some(lib) =
            run_option(|o| o.libraries.iter().find(|l| l.prefix() == prefix).cloned())
    {
        return lib_agg_command(lib.as_ref(), name, call, caps, host, trace, outer, peers);
    }
    let Some((env, name)) = environment_of(call) else {
        return Vec::new();
    };
    let seed = seed_capsa(&caps, host);
    let scope = seed.scope(None, peers, outer);
    let ((operands, options), o) = capture(|| words(call, host, &seed.ext, trace, scope));
    let argv = Environment::argv(&operands, &options);
    let mut origins = o;
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(host.origin_at(&c.ext)),
        }
    }
    let input: String = caps
        .iter()
        .map(|c| crate::pipeline::env::line_of(&effective_topic(c, host)))
        .collect::<Vec<_>>()
        .join("\n");
    let text = match env.call(&name, &argv, &input) {
        Ok(text) => text,
        Err(f) => {
            record_refusal(f.message().to_string());
            return Vec::new();
        }
    };
    let register = shared_register(&caps);
    let node = seed.ext.bare();
    text.lines()
        .map(|line| Thread::<H::Ext> {
            prov: Prov::leaf(origins.clone()),
            register: register.clone(),
            topic: Some(Value::Str(line.trim_end_matches('\r').to_string())),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            ext: node.clone(),
        })
        .collect()
}

/// Run one shell command for the `sh(...)` command: `cmd` under
/// `sh -c`, the topic's text on stdin, stdout (one trailing newline
/// trimmed) as the result. A failing spawn or exit status is `None`
/// (the command nulls it); stderr is inherited so diagnostics stay
/// visible.
pub(crate) fn run_shell(cmd: &str, topic: Option<&Value>) -> Option<Value> {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(v) = topic
        && let Some(stdin) = child.stdin.take()
    {
        let mut text = v.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        // Fed from a thread of its own, SIGPIPE blocked there (see
        // `env::feed_stdin`): the parent drains stdout meanwhile,
        // and a command that stops reading early cannot kill the
        // process.
        crate::pipeline::env::feed_stdin(stdin, text);
    }
    // Any stdin handle not moved into the writer thread (the
    // topicless case) is dropped so the child still sees EOF.
    drop(child.stdin.take());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    Some(Value::Str(text))
}
