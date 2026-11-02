//! The signature notation of the inventory (spec: Signatures, draft
//! 203): one braced form per command, `f{a}(b; c?; -k|--kind
//! --from=PATH --weight=UNIT?)`. The braced parameter is the stdin
//! slot: the pipe fills it when the command is piped, the first
//! operand fills it at the head of an expression. The parenthesis
//! holds the operands by position, then the option segment. `?`
//! marks an optional operand (never before a required one), `…` zero
//! or more (`x; x…` one or more); a flag is always optional; a valued
//! option is required written bare and optional with `?`; the short
//! spelling stands beside the long one, `-k|--kind`. A nullary
//! command has no braced slot (`now()`); a reducer's slot is the
//! stream (`count{stream}(-o|--of=expr?)`). Both call spellings and
//! the refusal text derive from the one form.

/// One parsed form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    /// The stdin slot's name, when the command has one.
    pub slot: Option<String>,
    pub operands: Vec<Param>,
    pub options: Vec<OptSpec>,
}

/// One positional operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// `c?`
    pub optional: bool,
    /// `c…`
    pub variadic: bool,
}

/// One option of the segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptSpec {
    /// `--kind`
    pub long: Option<String>,
    /// `-k`
    pub short: Option<String>,
    /// The value's placeholder (`PATH`, `always|structures|off`);
    /// `None` for a flag.
    pub value: Option<String>,
    /// A valued option written bare is required; `?` makes it
    /// optional. A flag is never required.
    pub required: bool,
}

impl OptSpec {
    /// Whether an option as written (`-k`, `--kind`) is this one.
    pub fn answers_to(&self, written: &str) -> bool {
        self.long.as_deref() == Some(written) || self.short.as_deref() == Some(written)
    }

    /// The spelling for a message: `-k|--kind`, `--from=PATH`.
    pub fn spelled(&self) -> String {
        let names: Vec<&str> = [self.short.as_deref(), self.long.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let mut s = names.join("|");
        if let Some(v) = &self.value {
            s.push('=');
            s.push_str(v);
            if !self.required {
                s.push('?');
            }
        }
        s
    }
}

impl Signature {
    /// The operands a call must write.
    pub fn min_operands(&self) -> usize {
        self.operands
            .iter()
            .filter(|p| !p.optional && !p.variadic)
            .count()
    }

    /// The operands a call may write at most; `None` where a `…`
    /// leaves it open.
    pub fn max_operands(&self) -> Option<usize> {
        if self.operands.iter().any(|p| p.variadic) {
            None
        } else {
            Some(self.operands.len())
        }
    }

    /// The option an option as written names.
    pub fn option(&self, written: &str) -> Option<&OptSpec> {
        self.options.iter().find(|o| o.answers_to(written))
    }

    /// The form as the notation writes it.
    pub fn text(&self) -> String {
        let mut s = self.name.clone();
        if let Some(slot) = &self.slot {
            s.push('{');
            s.push_str(slot);
            s.push('}');
        }
        s.push('(');
        let mut fields: Vec<String> = self
            .operands
            .iter()
            .map(|p| {
                let mut f = p.name.clone();
                if p.variadic {
                    f.push('…');
                } else if p.optional {
                    f.push('?');
                }
                f
            })
            .collect();
        if !self.options.is_empty() {
            fields.push(
                self.options
                    .iter()
                    .map(OptSpec::spelled)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        s.push_str(&fields.join("; "));
        s.push(')');
        s
    }
}

/// An inventory cell: one form, or genuinely different signatures
/// separated by ` / `.
pub fn parse_cell(cell: &str) -> Result<Vec<Signature>, String> {
    cell.split(" / ").map(parse_form).collect()
}

/// One form of the notation.
pub fn parse_form(form: &str) -> Result<Signature, String> {
    let form = form.trim();
    let name_end = form
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(form.len());
    let name = &form[..name_end];
    if name.is_empty() {
        return Err(format!(
            "'{form}': a signature opens with the command's name"
        ));
    }
    let mut rest = &form[name_end..];
    let mut slot = None;
    if let Some(after) = rest.strip_prefix('{') {
        let Some(close) = after.find('}') else {
            return Err(format!("'{form}': the braced slot is not closed"));
        };
        let word = after[..close].trim();
        if word.is_empty() || !word.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(format!("'{form}': the braced slot is one word"));
        }
        slot = Some(word.to_string());
        rest = &after[close + 1..];
    }
    let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) else {
        return Err(format!(
            "'{form}': the parentheses are always written, empty for a command without operands"
        ));
    };
    let fields: Vec<&str> = inner.split(';').map(str::trim).collect();
    let fields: Vec<&str> = if fields == [""] { Vec::new() } else { fields };
    let mut operands = Vec::new();
    let mut options = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        if field.is_empty() {
            return Err(format!("'{form}': an empty field"));
        }
        if field.starts_with('-') {
            if i + 1 != fields.len() {
                return Err(format!("'{form}': the option segment stands last"));
            }
            for word in field.split_whitespace() {
                let opt = parse_option(word).map_err(|e| format!("'{form}': {e}"))?;
                for seen in &options {
                    let seen: &OptSpec = seen;
                    if seen.long.is_some() && seen.long == opt.long {
                        return Err(format!(
                            "'{form}': '{}' is declared twice",
                            opt.long.as_deref().unwrap_or("")
                        ));
                    }
                    if seen.short.is_some() && seen.short == opt.short {
                        return Err(format!(
                            "'{form}': the short option '{}' is declared twice",
                            opt.short.as_deref().unwrap_or("")
                        ));
                    }
                }
                options.push(opt);
            }
            continue;
        }
        let (text, optional, variadic) = if let Some(t) = field.strip_suffix('…') {
            (t, false, true)
        } else if let Some(t) = field.strip_suffix('?') {
            (t, true, false)
        } else {
            (*field, false, false)
        };
        if text.is_empty() || !text.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(format!("'{form}': '{field}' is no operand name"));
        }
        if operands.iter().any(|p: &Param| p.variadic) {
            return Err(format!(
                "'{form}': '{field}' follows a '…' operand, which stands last"
            ));
        }
        if !optional && !variadic && operands.iter().any(|p: &Param| p.optional) {
            return Err(format!(
                "'{form}': the required operand '{field}' follows an optional one"
            ));
        }
        operands.push(Param {
            name: text.to_string(),
            optional,
            variadic,
        });
    }
    Ok(Signature {
        name: name.to_string(),
        slot,
        operands,
        options,
    })
}

/// `-k|--kind`, `--from=PATH`, `--weight=UNIT?`, `-p|--pretty=a|b`.
fn parse_option(word: &str) -> Result<OptSpec, String> {
    let (names, value) = match word.find('=') {
        Some(eq) => (&word[..eq], Some(&word[eq + 1..])),
        None => (word, None),
    };
    let (value, required) = match value {
        None => (None, false),
        Some(v) => match v.strip_suffix('?') {
            Some(v) => (Some(v), false),
            None => (Some(v), true),
        },
    };
    if let Some(v) = value
        && v.is_empty()
    {
        return Err(format!("'{word}': the value's placeholder is missing"));
    }
    if value.is_none() && names.ends_with('?') {
        return Err(format!(
            "'{word}': a flag is always optional, never written with '?'"
        ));
    }
    let mut long = None;
    let mut short = None;
    for name in names.split('|') {
        if let Some(l) = name.strip_prefix("--") {
            if l.is_empty()
                || !l
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!("'{word}': '{name}' is no option name"));
            }
            if long.replace(name.to_string()).is_some() {
                return Err(format!("'{word}': two long spellings"));
            }
        } else if let Some(s) = name.strip_prefix('-') {
            let mut chars = s.chars();
            if !(chars.next().is_some_and(char::is_alphabetic) && chars.next().is_none()) {
                return Err(format!(
                    "'{word}': a short option is one letter, '{name}' given"
                ));
            }
            if short.replace(name.to_string()).is_some() {
                return Err(format!("'{word}': two short spellings"));
            }
        } else {
            return Err(format!("'{word}': '{name}' is no option"));
        }
    }
    Ok(OptSpec {
        long,
        short,
        value: value.map(str::to_string),
        required,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_examples_of_the_notation_parse() {
        let s = parse_form("f{a}(b; c?; -k|--kind --from=PATH --weight=UNIT?)").unwrap();
        assert_eq!(s.name, "f");
        assert_eq!(s.slot.as_deref(), Some("a"));
        assert_eq!(s.min_operands(), 1);
        assert_eq!(s.max_operands(), Some(2));
        assert_eq!(s.options.len(), 3);
        let kind = s.option("-k").unwrap();
        assert_eq!(kind.long.as_deref(), Some("--kind"));
        assert!(kind.value.is_none() && !kind.required);
        let from = s.option("--from").unwrap();
        assert_eq!(from.value.as_deref(), Some("PATH"));
        assert!(from.required && from.short.is_none());
        let weight = s.option("--weight").unwrap();
        assert!(!weight.required);
        assert_eq!(
            s.text(),
            "f{a}(b; c?; -k|--kind --from=PATH --weight=UNIT?)"
        );

        let now = parse_form("now()").unwrap();
        assert!(now.slot.is_none() && now.operands.is_empty() && now.options.is_empty());
        let count = parse_form("count{stream}(-o|--of=expr?)").unwrap();
        assert_eq!(count.slot.as_deref(), Some("stream"));
        assert_eq!(count.max_operands(), Some(0));
        let lev = parse_form("levenshtein{text}(other)").unwrap();
        assert_eq!((lev.min_operands(), lev.max_operands()), (1, Some(1)));
        let group = parse_form("group{stream}(key; key…)").unwrap();
        assert_eq!((group.min_operands(), group.max_operands()), (1, None));
        let tree = parse_form(
            "tree{context}(expr…; -d|--depth=n? -k|--kind -t|--traits -p|--props -r|--record -s|--sample=n?)",
        )
        .unwrap();
        assert_eq!((tree.min_operands(), tree.max_operands()), (0, None));
        assert!(!tree.option("--depth").unwrap().required);
        // alternatives after the first `=`
        let session = parse_form("session(-p|--pretty=always|structures|off?)").unwrap();
        assert_eq!(
            session.option("--pretty").unwrap().value.as_deref(),
            Some("always|structures|off")
        );
        // a cell of two signatures
        assert_eq!(parse_cell("f(a) / f(a; b; c)").unwrap().len(), 2);
    }

    #[test]
    fn the_notation_refuses_what_it_rules_out() {
        for (form, why) in [
            ("round", "parentheses"),
            ("f{a}", "parentheses"),
            ("f{}(a)", "one word"),
            ("f(a?; b)", "follows an optional"),
            ("f(a…; b)", "stands last"),
            ("f(-k|--kind; b)", "option segment stands last"),
            ("f(a; -k|--kind?)", "always optional"),
            ("f(a; -kk|--kind)", "one letter"),
            ("f(a; -k|--kind -k|--key)", "declared twice"),
            ("f(a; --kind --kind=n)", "declared twice"),
            ("f(a; --from=)", "placeholder"),
            ("f(a; b;)", "empty field"),
        ] {
            let e = parse_form(form).unwrap_err();
            assert!(e.contains(why), "{form}: {e}");
        }
    }
}
