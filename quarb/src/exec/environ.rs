//! Environment commands: `ALIAS:name(operands; options)` run by the
//! environment the host mounted under the alias (spec: Environment
//! Commands). The text kinds frame as `sh` does: after `|` once per
//! capsa, the topic's text on stdin and stdout (one trailing newline
//! trimmed) the value; after `@|` once over the context, one line
//! per capsa on stdin and every stdout line a value, each carrying
//! the shared regulae.

use super::*;
use crate::env::{CallFailure, Environment};

/// The call's operands and options as text, evaluated against
/// `node` under `scope`: the translation's text side.
fn words(
    call: &FnCall,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    scope: Scope<'_>,
) -> (Vec<String>, Vec<(String, Option<String>)>) {
    let text = |a: &Arg| -> Option<String> {
        match a {
            Arg::Lit(v) => Some(v.to_string()),
            Arg::Expr(e) => Some(operand_scalar(adapter, node, e, trace, scope).to_string()),
            Arg::Range(a, b) => Some(format!(
                "{}..{}",
                a.map(|n| n.to_string()).unwrap_or_default(),
                b.map(|n| n.to_string()).unwrap_or_default()
            )),
            Arg::Named { .. } | Arg::Opt { .. } => None,
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
                    e => operand_scalar(adapter, node, e, trace, scope).to_string(),
                }),
            )),
            _ => None,
        })
        .collect();
    (operands, options)
}

/// The mounted environment a call names, or the refusal.
fn environment_of(call: &FnCall) -> Option<(Environment, String)> {
    let (alias, name) = crate::env::split_env(&call.name)?;
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

/// `| ALIAS:name(…)` — once per capsa on the topic's text; a failed
/// call yields null, an unknown name refuses. `apply_command`'s arm.
pub(super) fn env_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    let Some((env, name)) = environment_of(call) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(caps.len());
    for (i, mut c) in caps.into_iter().enumerate() {
        let scope = c.scope(Some(i + 1), peers, outer);
        let ((operands, options), o) = capture(|| words(call, adapter, c.node, trace, scope));
        let argv = Environment::argv(&operands, &options);
        let input = effective_topic(&c, adapter).to_string();
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
pub(super) fn env_agg_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    let Some((env, name)) = environment_of(call) else {
        return Vec::new();
    };
    let seed = seed_capsa(&caps, adapter);
    let scope = seed.scope(None, peers, outer);
    let ((operands, options), o) = capture(|| words(call, adapter, seed.node, trace, scope));
    let argv = Environment::argv(&operands, &options);
    let mut origins = o;
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(Origin {
                node: c.node,
                command: command_now(),
            }),
        }
    }
    let input: String = caps
        .iter()
        .map(|c| crate::env::line_of(&effective_topic(c, adapter)))
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
    let node = seed.node;
    text.lines()
        .map(|line| Capsa {
            prov: Prov::leaf(origins.clone()),
            node,
            register: register.clone(),
            topic: Some(Value::Str(line.trim_end_matches('\r').to_string())),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            marks: Vec::new(),
            bindings: Vec::new(),
            arrived: Vec::new(),
        })
        .collect()
}
