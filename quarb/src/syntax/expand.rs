//! What the parser asks of the layer that can evaluate: a macro's
//! expansion (its body run over its bindings, and over the dataset
//! when it is data-aware), and the register usage of a fragment,
//! which the graph reads off the query's reflection. The parser
//! holds an expander and never evaluates anything itself.
use crate::syntax::ast::{Command, Operand, Query};
use crate::value::Value;

/// A macro parameter binding for the expansion arbor: one argument
/// form, or the collected remainder (`@rest`).
pub(crate) enum MacroBinding {
    One(Operand),
    Rest(Vec<Operand>),
}

pub(crate) struct RegUsage {
    pub(crate) pushes: std::collections::BTreeMap<String, usize>,
    pub(crate) recalls: std::collections::BTreeMap<String, usize>,
}

pub(crate) trait Expander {
    /// The unit table a quantity literal is read with: the source's
    /// declared units over the built-in table.
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        crate::value::quantity::scale_expr(expr)
    }
    /// The unit names the dataset adds to the built-in table (a
    /// model's declarations, a document's own units), for the
    /// operand of `convert(U)` and `unit(U)`; None when no dataset
    /// is at hand to ask, and the operand is judged at the splice
    /// site instead (a definitions file parses without one).
    fn unit_names(&self) -> Option<Vec<String>> {
        None
    }
    fn usage_of_query(&self, q: &Query) -> RegUsage;
    fn usage_of_operand(&self, op: &Operand) -> RegUsage;
    fn usage_of_commands(&self, commands: &[Command]) -> RegUsage;
    /// The register names a macro's arguments invite.
    fn operand_invites(&self, op: &Operand) -> std::collections::BTreeSet<String>;
    /// A macro's expansion: the texts its body yields over the
    /// bindings (and the dataset, when data-aware). An error says
    /// why it could not expand, in the parser's words.
    fn expand(
        &self,
        name: &str,
        body: &Query,
        bindings: &[(String, MacroBinding)],
        data_aware: bool,
    ) -> crate::Result<Vec<Value>>;
}
