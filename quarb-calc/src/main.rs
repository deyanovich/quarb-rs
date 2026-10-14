//! `qc` — the Quarb calculator: `qua` with every adapter off. The
//! same source and the same code paths as the full command, built
//! without the sources it could open; a value rule tested here is
//! the rule the full build runs.
fn main() -> anyhow::Result<()> {
    qua::calc_main()
}
