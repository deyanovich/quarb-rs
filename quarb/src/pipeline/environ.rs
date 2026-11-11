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
            Arg::Lit(v) | Arg::Key(v) => Some(v.to_string()),
            Arg::Expr(e) => Some(operand_scalar(host, ext, e, trace, scope).to_string()),
            Arg::Range(a, b) => Some(format!(
                "{}..{}",
                a.map(|n| n.to_string()).unwrap_or_default(),
                b.map(|n| n.to_string()).unwrap_or_default()
            )),
            Arg::Named { .. } | Arg::Opt { .. } | Arg::Pairs { .. } | Arg::Hole(_) => None,
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
                 ('{alias}=exec:DIR', '{alias}=shell:', '{alias}=python:MODULE.py')"
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
            Arg::Lit(v) | Arg::Key(v) => Some(v.clone()),
            Arg::Expr(e) => Some(eval(e)),
            Arg::Range(a, b) => Some(Value::list(vec![
                a.map(Value::Int).unwrap_or(Value::Null),
                b.map(Value::Int).unwrap_or(Value::Null),
            ])),
            Arg::Named { .. } | Arg::Opt { .. } | Arg::Pairs { .. } | Arg::Hole(_) => None,
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
    Value::Record(fields.into())
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
    let mut out = Vec::with_capacity(caps.len());
    let mut warned = false;
    for (i, mut c) in caps.into_iter().enumerate() {
        let scope = c.scope(Some(i + 1), peers, outer);
        let ((operands, options), o) = capture(|| lib_values(call, host, &c.ext, trace, scope));
        // the syntax library builds like a node: a node reference, or
        // the cursor record standing at one, reaches it as the quarq
        // of the node's form (design paper: the constructors)
        let operands = if lib.prefix() == "syntax" {
            // `$_` placed as an operand is the thread's own node
            let positional = call
                .args
                .iter()
                .filter(|a| matches!(a, Arg::Lit(_) | Arg::Key(_) | Arg::Expr(_) | Arg::Range(..)));
            positional
                .zip(operands)
                .map(|(a, v)| match a {
                    Arg::Expr(Operand::Topic | Operand::Cursor) => host
                        .node_form(&c.ext)
                        .map(Value::quarq_form)
                        .unwrap_or_else(|| node_quarq(host, v)),
                    // a path operand reaches the nodes' forms, one
                    // quarq each (`syntax:node($_; /*:.b)` takes a
                    // list of quarqs; `syntax:node($_; ^/q//predicate)`
                    // the nodes themselves)
                    Arg::Expr(Operand::Rel {
                        steps,
                        anchor,
                        projection: None,
                    }) => {
                        let reached = crate::pipeline::run::out_of_row_reach(|| {
                            host.rel_threads(&c.ext, steps, anchor, None, trace, scope)
                        });
                        let forms: Vec<Value> = reached
                            .iter()
                            .filter_map(|(x, _, _)| host.node_form(x).map(Value::quarq_form))
                            .collect();
                        if reached.is_empty() {
                            node_quarq(host, v)
                        } else if forms.len() < reached.len() {
                            record_refusal(format!(
                                "syntax:{name}: a node the path reaches has no form (the expansion \
                                 root, a rest parameter or a data node); a child is a reflected \
                                 node or a quarq"
                            ));
                            Value::Null
                        } else {
                            Value::list(forms)
                        }
                    }
                    _ => node_quarq(host, v),
                })
                .collect()
        } else {
            operands
        };
        let takes = crate::pipeline::library::takes_input(syn, call, operands.len());
        let input = takes.then(|| match &c.topic {
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
            // a constructor's refusal is the query's (design paper:
            // the constructors — refused naming the kind)
            Err(crate::pipeline::library::LibFailure::Failed(m)) if lib.prefix() == "syntax" => {
                record_refusal(format!("syntax:{name}: {m}"));
                Value::Null
            }
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
    let scope = seed.scope(Some(crate::pipeline::run::group_ordinal()), peers, outer);
    let ((operands, options), o) = capture(|| lib_values(call, host, &seed.ext, trace, scope));
    let mut origins = o;
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(host.origin_at(&c.ext)),
        }
    }
    let takes = crate::pipeline::library::takes_input(syn, call, operands.len());
    // A stream of one library value, table or list (the frame a
    // previous command built, a list literal) crosses as that value,
    // not as a list around it, so either pipe continues a chain and
    // `= @(1; 2) @| np:log` is the sample, not a one-row matrix.
    let input = takes.then(|| match caps.as_slice() {
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
    if let Some(("graph", name)) = crate::pipeline::env::split_env(&call.name) {
        record_refusal(format!(
            "graph:{name} reads the context's nodes and the edges among them: write it after '@|' on a node context"
        ));
        return Vec::new();
    }
    // the engine's own syntax library (design paper: the
    // constructors), which every build carries
    if let Some(("syntax", name)) = crate::pipeline::env::split_env(&call.name) {
        let lib: &dyn crate::pipeline::library::CommandLibrary = &crate::syntax::construct::SYNTAX;
        return lib_command(lib, name, call, caps, host, trace, outer, peers);
    }
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
        let ((mut operands, options), o) = capture(|| words(call, host, &c.ext, trace, scope));
        // Headless, the first operand is the stdin slot (spec: The
        // stdin slot): a text callee reads it on its standard input.
        let input = if call.headless && !operands.is_empty() {
            operands.remove(0)
        } else {
            effective_topic(&c, host).to_string()
        };
        let argv = Environment::argv(&operands, &options);
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
    if let Some((prefix, name)) = crate::pipeline::env::split_env(&call.name) {
        // the engine's own library: row-aligned over the context,
        // never a call over values (spec: The graph library)
        if prefix == "graph" {
            return graph_library_command(name, call, caps, host, trace, outer, peers);
        }
        if prefix == "syntax" {
            let lib: &dyn crate::pipeline::library::CommandLibrary =
                &crate::syntax::construct::SYNTAX;
            return lib_agg_command(lib, name, call, caps, host, trace, outer, peers);
        }
        if let Some(lib) =
            run_option(|o| o.libraries.iter().find(|l| l.prefix() == prefix).cloned())
        {
            return lib_agg_command(lib.as_ref(), name, call, caps, host, trace, outer, peers);
        }
    }
    let Some((env, name)) = environment_of(call) else {
        return Vec::new();
    };
    let seed = seed_capsa(&caps, host);
    let scope = seed.scope(Some(crate::pipeline::run::group_ordinal()), peers, outer);
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

/// A node reference — or the cursor record `%(topic; position)`
/// standing at one, or a list of either — as the quarq of the
/// node's form, where the arbor is a reflected query; any other
/// value as it is.
fn node_quarq<H: Host>(host: &H, v: Value) -> Value {
    let of_node = |n: &crate::value::NodeRef| {
        host.node_form(&host.ext_at(crate::NodeId(n.id)))
            .map(Value::quarq_form)
    };
    match &v {
        Value::Node(n) => of_node(n).unwrap_or(v),
        Value::Record(r) => match r.get("position") {
            Some(Value::Node(n)) if r.get("topic").is_some() => of_node(n).unwrap_or(v),
            _ => v,
        },
        Value::List(items) if items.iter().any(|i| matches!(i, Value::Node(_))) => {
            Value::list(items.iter().map(|i| node_quarq(host, i.clone())).collect())
        }
        _ => v,
    }
}
