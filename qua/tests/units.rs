//! The unit table of the appendix Units and Prefixes, the quantity's
//! output form and literal, opaque units through the underscore,
//! the written-unit rules, and the `dimension`, `exchange` and
//! `scale` commands (spec: Physical Quantities, The Quantity
//! Commands).
use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"id":1,"size":"2 KiB","price":"12_EUR","rate":"3_EUR/kg","speed":"10 m/s","tag":"2nd","when":"2025-10-06"},
            {"id":2,"size":"1 MB","price":"8_USD","rate":"5_USD/lb","speed":"36 km/h","tag":"3D","when":"2024-01-01"},
            {"id":3,"size":"8 b","price":"soon","rate":"2_GBP/kg","speed":"1 kn","tag":"5 apples","when":"2025-11-02"}]}"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

/// The refusal's text.
fn refused(q: &str) -> String {
    match run(q) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

fn one(q: &str) -> String {
    let v = run(q).unwrap();
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

fn all(q: &str) -> String {
    run(q).unwrap().join(" | ")
}

#[test]
fn a_quantity_prints_as_magnitude_underscore_unit_and_pastes_back() {
    assert_eq!(one("= '5 km' | quantity"), "5_km");
    assert_eq!(one("= 5_km"), "5_km");
    assert_eq!(one("= 100_km/h | dimension"), "L/T");
    assert_eq!(one("= 12_EUR/kg | dimension"), "EUR/M");
    assert_eq!(one("= (5_km = '5 km')"), "true");
    assert_eq!(one("= (5_km = 5km)"), "true");
    assert_eq!(one("= 5_km + 500_m"), "5.5_km");
    // the underscore before digits stays the thousands separator
    assert_eq!(one("= 1_000 + 1"), "1001");
    // a span of time is the quantity of dimension T, in seconds
    assert_eq!(one("= 90_s | duration"), "90_s");
}

#[test]
fn the_bit_is_the_base_of_information() {
    assert_eq!(one("= '1 B' | convert(b)"), "8_b");
    assert_eq!(one("= '2 KiB' | convert(B)"), "2048_B");
    assert_eq!(one("= '1 kB' | convert(B)"), "1000_B");
    assert_eq!(one("= '1 Kib' | convert(b)"), "1024_b");
    // the long names are the same units, prefixes included
    assert_eq!(one("= 2_kB | convert(bit)"), "16000_bit");
    assert_eq!(one("= 16_bit | convert(B)"), "2_B");
    assert_eq!(one("= 2_B | convert(byte)"), "2_byte");
    assert_eq!(one("= 3_Mbit | convert(kbyte)"), "375_kbyte");
    assert_eq!(one("= 1_Kibit | convert(bit)"), "1024_bit");
    assert_eq!(one("= '4 bytes' | quantity | convert(bit)"), "32_bit");
    assert_eq!(one("= 2_кбайт | convert(bit)"), "16000_bit");
    assert_eq!(one("= '1 Mbps' | convert('kb/s')"), "1000_kb/s");
    // KB and Kb are refused, a millibyte is no unit: text stays text
    assert_eq!(one("= '5 KB' | quantity"), "null");
    assert_eq!(one("= '5 mB' | quantity"), "null");
    assert_eq!(one("= '5 KiB' | quantity | dimension"), "B");
}

#[test]
fn the_operand_of_a_conversion_must_be_a_known_unit() {
    // spec: The conversion (draft 161) — `KB` is no unit, and the
    // refusal says what the two readings are
    let e = refused("= 548_B | convert(KB)");
    assert!(e.contains("convert(KB): no unit 'KB'"), "{e}");
    assert!(e.contains("kB is 1000 B, KiB is 1024 B"), "{e}");
    assert_eq!(one("= 548_B | convert(kB)"), "0.548_kB");
    assert_eq!(one("= 548_B | convert(KiB) | round(3)"), "0.535_KiB");
    let e = refused("= 5 | unit(KB)");
    assert!(e.contains("unit(KB): no unit 'KB'"), "{e}");
    let e = refused("= 5_km | magnitude(KM)");
    assert!(
        e.contains("magnitude(KM): no unit 'KM' (the nearest: km)"),
        "{e}"
    );
    // a compound operand checks each factor
    let e = refused("= 3_EUR/kg | convert(EUR/lbs)");
    assert!(
        e.contains("convert(EUR/lbs): no unit 'lbs' (the nearest: lb)"),
        "{e}"
    );
    assert_eq!(
        one("= 3_EUR/kg | convert(EUR/lb) | round(3)"),
        "1.361_EUR/lb"
    );
    // the call operand is the pipe form
    let e = refused("= convert(548_B, KB)");
    assert!(e.contains("convert(KB): no unit 'KB'"), "{e}");
    assert_eq!(one("= convert(548_B, kB)"), "0.548_kB");
    // in a record item, where the silent null was found
    let e = refused("/rows/* | %(::id; size = convert(::size, KB))");
    assert!(e.contains("no unit 'KB'"), "{e}");
    // a known unit of another dimension stays null, as the spec says
    assert_eq!(one("= 548_B | convert(km)"), "null");
    assert_eq!(one("= 5_km | unit(kg)"), "null");
    // a value's unknown unit still reads as an opaque unit; the
    // operand alone must be known
    assert_eq!(one("= 3_apples/kg | dimension"), "apples/M");
    assert_eq!(one("= '12_apples' | quantity | unit"), "apples");
    let e = refused("= 3_apples | convert(apples)");
    assert!(e.contains("convert(apples): no unit 'apples'"), "{e}");
    // no operand: the base form, unchecked
    assert_eq!(one("= 5_km | convert"), "5000_m");
}

#[test]
fn a_fragment_body_spelling_a_unit_is_judged_at_the_splice() {
    // a definitions file parses without a dataset: the body's
    // operands wait for the splice site, which names the fragment
    let defs = quarb::parse_defs(
        "def &kilo: | convert(KB);\ndef &viakilo: | &kilo;\ndef &fine: | convert(kB);\n",
    )
    .unwrap();
    let run = quarb::Run::ambient();
    // (a body invoking the fragment defers once more, and the
    // refusal names the fragment the query invoked)
    for (q, name) in [
        ("= 548_B | &kilo", "kilo"),
        ("= 548_B | &viakilo", "viakilo"),
    ] {
        let e = quarb::Query::parse_with(q, &defs, &doc(), &run)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(&format!(
                "in expansion of '&{name}': convert(KB): no unit 'KB'"
            )),
            "{q}: {e}"
        );
    }
    let out = quarb::Query::parse_with("= 548_B | &fine", &defs, &doc(), &run)
        .unwrap()
        .run(&doc(), &run)
        .unwrap();
    assert_eq!(
        match out.result {
            QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect::<Vec<_>>(),
            _ => vec![],
        },
        ["0.548_kB"]
    );
}

#[test]
fn the_appendix_rows_read_with_their_factors() {
    assert_eq!(one("= '1 au' | convert(m)"), "149597870700_m");
    assert_eq!(one("= '1 ha' | convert('m^2')"), "10000_m^2");
    assert_eq!(one("= '1 kcal' | convert(J)"), "4184_J");
    assert_eq!(one("= '1 atm' | convert(Pa)"), "101325_Pa");
    assert_eq!(one("= '1 mph' | convert('mi/h')"), "1_mi/h");
    assert_eq!(one("= '1 kn' | convert('nmi/h')"), "1_nmi/h");
    assert_eq!(one("= '1 psi' | convert(Pa) | round"), "6895_Pa");
    assert_eq!(one("= '1 cup' | convert(floz)"), "8_floz");
    assert_eq!(one("= '2 st' | convert(lb)"), "28_lb");
    // the radian is dimensionless
    assert_eq!(one("= '2 rad' | dimension"), "1");
    assert_eq!(one("= (('3 rad' | quantity) > 2)"), "true");
}

#[test]
fn an_undeclared_name_is_an_opaque_unit_through_the_underscore() {
    assert_eq!(one("= 12_EUR"), "12_EUR");
    assert_eq!(one("= 12_EUR | dimension"), "EUR");
    assert_eq!(one("= '3_EUR/kg' | dimension"), "EUR/M");
    assert_eq!(one("= '5_apples' | quantity"), "5_apples");
    // its own kind: comparable and summable with itself only
    assert_eq!(one("= 12_EUR + 3_EUR"), "15_EUR");
    assert_eq!(one("= 12_EUR + 3_USD"), "null");
    assert_eq!(one("= (12_EUR < 13_EUR)"), "true");
    assert_eq!(one("= (12_EUR < 13_USD)"), "false");
    // never scaled: no prefix is stripped from an opaque name
    assert_eq!(one("= 1_kEUR | dimension"), "kEUR");
    // a currency symbol is a name
    assert_eq!(one("= 12_€ | dimension"), "€");
    // the common currencies are built in: spaced, glued, underscored,
    // and through their prefix aliases
    assert_eq!(one("= '12EUR' | quantity"), "12_EUR");
    assert_eq!(one("= '$12.50' | quantity"), "12.5_USD");
    assert_eq!(one("= '€12' | quantity"), "12_EUR");
    assert_eq!(one("= '¥ 500' | quantity"), "500_JPY");
    assert_eq!(one("= '12 EUR/kg' | quantity"), "12_EUR/kg");
    assert_eq!(one("= '12 EUR' | quantity | dimension"), "EUR");
    assert_eq!(
        one("= ('12 EUR' | quantity) + ('3 USD' | quantity)"),
        "null"
    );
    assert_eq!(one("= ('12 EUR' | quantity) | convert(USD)"), "null");
    assert_eq!(one("= '12 UAH' | quantity"), "12_UAH");
    assert_eq!(one("= '₦500' | quantity"), "500_NGN");
    assert_eq!(one("= '12 GEL' | quantity"), "12_GEL");
    assert_eq!(one("= '֏500' | quantity"), "500_AMD");
    assert_eq!(one("= '12 ISK' | quantity"), "12_ISK");
    assert_eq!(one("= '12 XCD' | quantity"), "12_XCD");
    // the second, retired, metal and fund codes are not units
    assert_eq!(one("= '12 VED' | quantity"), "null");
    assert_eq!(one("= '12 XAU' | quantity"), "null");
    assert_eq!(one("= '12 HRK' | quantity"), "null");
    assert_eq!(one("= '₸500' | quantity"), "500_KZT");
    assert_eq!(one("= '12 BYN' | quantity"), "12_BYN");
    assert_eq!(one("= '12 usd' | quantity"), "null");
    // spaced or glued, an undeclared name is text
    assert_eq!(all("/rows/*::tag | quantity"), "null | null | null");
    assert_eq!(one("= '2nd' | quantity"), "null");
    assert_eq!(one("= '5G' | quantity"), "null");
    assert_eq!(one("= '12 EUR' | quantity"), "12_EUR");
    assert_eq!(one("= '12€' | quantity"), "null");
    // the built-in units read either way
    assert_eq!(one("= '2km' | quantity"), "2_km");
    assert_eq!(one("= '2 km' | quantity"), "2_km");
}

#[test]
fn convert_keeps_an_opaque_factor_and_scales_the_rest() {
    assert_eq!(
        one("= 12_EUR/kg | convert('EUR/lb') | round(3)"),
        "5.443_EUR/lb"
    );
    assert_eq!(one("= 12_EUR/kg | convert('USD/lb')"), "null");
    assert_eq!(one("= 12_EUR | convert(EUR)"), "12_EUR");
}

#[test]
fn the_dimension_names_the_base_dimensions() {
    assert_eq!(one("= '10 m/s' | dimension"), "L/T");
    assert_eq!(one("= '36 km/h' | dimension"), "L/T");
    assert_eq!(one("= '1 J' | dimension"), "M*L^2/T^2");
    assert_eq!(one("= '1 N' | dimension"), "M*L/T^2");
    assert_eq!(one("= '1 K' | dimension"), "Θ");
    assert_eq!(one("= '1 mol/s' | dimension"), "N/T");
    assert_eq!(one("= 'soon' | dimension"), "null");
    assert_eq!(one("= 5 | dimension"), "null");
    assert_eq!(all("/rows/*::speed | dimension @| unique"), "L/T");
    assert_eq!(all("/rows/*::price | dimension"), "EUR | USD | null");
}

#[test]
fn the_exchange_follows_the_rates_record() {
    assert_eq!(one("= 12_EUR | exchange(%(EUR = 1.25_USD); USD)"), "15_USD");
    // the record is read in both directions, and chains
    assert_eq!(one("= 15_USD | exchange(%(EUR = 1.25_USD); EUR)"), "12_EUR");
    assert_eq!(
        one("= 10_GBP | exchange(%(EUR = 1.25_USD; GBP = 1.2_EUR); USD)"),
        "15_USD"
    );
    // buy and sell rates differ: a direct entry is taken as written
    // before the inverse of the opposite entry, and a record holding
    // both directions never inverts
    assert_eq!(
        one("= 12_USD | exchange(%(EUR = 1.08_USD; USD = 0.93_EUR); EUR)"),
        "11.16_EUR"
    );
    assert_eq!(
        one("= 12_EUR | exchange(%(EUR = 1.08_USD; USD = 0.93_EUR); USD)"),
        "12.96_USD"
    );
    assert_eq!(
        one("= 12_USD | exchange(%(USD = 0.93_EUR; EUR = 1.08_USD); EUR)"),
        "11.16_EUR"
    );
    // the inverse of a direct entry before a chain, however the
    // chain runs
    assert_eq!(
        one("= 12_USD | exchange(%(GBP = 1.25_USD; USD = 1.1_CHF; CHF = 0.8_GBP); GBP) | round(2)"),
        "9.6_GBP"
    );
    // among chains of one length, the one with the fewer inverted legs
    assert_eq!(
        one("= 10_GBP | exchange(%(EUR = 1.2_GBP; GBP = 1.25_EUR; USD = 0.8_EUR); USD) | round(3)"),
        "15.625_USD"
    );
    // without a target, the other side of the one pair
    assert_eq!(one("= 12_EUR | exchange(%(EUR = 1.25_USD))"), "15_USD");
    assert_eq!(one("= 15_USD | exchange(%(EUR = '1.25_USD'))"), "12_EUR");
    // the other factors are carried
    assert_eq!(
        one("= 12_EUR/kg | exchange(%(EUR = 1.25_USD); USD)"),
        "15_USD/kg"
    );
    assert_eq!(
        one("= 12_EUR/kg | exchange(%(EUR = 1.25_USD); USD) | convert('USD/lb') | round(3)"),
        "6.804_USD/lb"
    );
    // already in the target: passes through
    assert_eq!(one("= 12_USD | exchange(%(EUR = 1.25_USD); USD)"), "12_USD");
    // no opaque factor, two of them, or a rate the record does not reach: null
    assert_eq!(one("= 12_kg | exchange(%(EUR = 1.25_USD); USD)"), "null");
    assert_eq!(
        one("= 12_EUR/GBP | exchange(%(EUR = 1.25_USD); USD)"),
        "null"
    );
    assert_eq!(one("= 12_CHF | exchange(%(EUR = 1.25_USD); USD)"), "null");
    assert_eq!(one("= 'soon' | exchange(%(EUR = 1.25_USD); USD)"), "null");
    // several entries and no target: refused by name
    let err = run("= 12_EUR | exchange(%(EUR = 1.25_USD; GBP = 1.2_EUR))")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("exchange") && err.contains("needs a target"),
        "{err}"
    );
    // the rates may come from a register
    assert_eq!(
        all("/rows/*[::id = 1] | .r(%(EUR = 1.25_USD)) | ::price | exchange($.r; USD)"),
        "15_USD"
    );
}

#[test]
fn a_quantity_compares_only_within_its_dimension() {
    // A quantity is equal, and ordered, only to a quantity of its
    // own dimension (spec: Relations): a bare number carries no
    // dimension, so it is never equal to, above or below a
    // dimensioned value, time included. The unit text beside the
    // quantity still pairs within the dimension.
    assert_eq!(all("/rows/*[(::size | quantity) > 1]::id"), "");
    assert_eq!(all("/rows/*[(::size | quantity) < 5]::id"), "");
    assert_eq!(all("/rows/*[(::size | quantity) > 1_KiB]::id"), "1 | 2");
    assert_eq!(one("= (42_km > 40)"), "false");
    assert_eq!(one("= (42_km = 42)"), "false");
    assert_eq!(one("= (42_km = 42000_m)"), "true");
    assert_eq!(one("= (42_km > 40_km)"), "true");
    assert_eq!(one("= (60_s = 60)"), "false");
    assert_eq!(one("= (60_s = 'PT1M')"), "true");
    assert_eq!(one("= (30_GBP = 30)"), "false");
    // The keys of unique, sort and group carry the dimension too:
    // 30_s and PT30S are one value, the rest four apart.
    assert_eq!(
        one("= @(30; 30_GBP; 30_km; 30_s; 'PT30S') | unique | count"),
        "4"
    );
}

#[test]
fn a_computed_quantity_displays_in_the_first_operands_unit() {
    assert_eq!(one("= '3 MB' + '2 MB'"), "5_MB");
    assert_eq!(one("= 1_kW + 350_W"), "1.35_kW");
    assert_eq!(one("= 350_W + 1_kW"), "1350_W");
    assert_eq!(one("= 2_KiB - 1_kB"), "1.0234375_KiB");
    assert_eq!(
        all("/rows/*[::id <= 2]::size | quantity @| sum"),
        "978.5625_KiB"
    );
    assert_eq!(one("= 2_km * 3"), "6_km");
}

#[test]
fn scale_picks_the_prefix_that_brings_the_magnitude_to_one() {
    assert_eq!(one("= 1400000_B | scale"), "1.4_MB");
    assert_eq!(one("= 1400000_B | scale(binary) | round(3)"), "1.335_MiB");
    assert_eq!(one("= 0.002_m | scale"), "2_mm");
    assert_eq!(one("= 2500_kg | scale"), "2.5_Mg");
    assert_eq!(one("= 3_mi | scale"), "3_mi");
    assert_eq!(one("= 12_EUR | scale"), "12_EUR");
    assert_eq!(one("= '0.5 kB' | scale"), "500_B");
    assert_eq!(one("= 'soon' | scale"), "null");
}

#[test]
fn the_year_is_written_with_its_dash() {
    assert_eq!(all("/rows/*[::when <<= 2025-]::id"), "1 | 3");
    assert_eq!(one("= (('1812' | period) = 1812-)"), "true");
    assert_eq!(one("= ((1812 | period) = 1812)"), "false");
    assert_eq!(one("= 1812 | period"), "1812");
    assert_eq!(one("= 2025- | period"), "2025");
    assert_eq!(one("= 2025- | period | grain"), "year");
    // a bare integer is epoch seconds where an instant is expected
    assert_eq!(one("= (0 = (1970-01-01T00:00:00Z | datetime))"), "true");
}

#[test]
fn type_names_the_value_and_a_tables_columns() {
    assert_eq!(one("= 5 | type"), "int");
    assert_eq!(one("= 5.5 | type"), "float");
    assert_eq!(one("= 'x' | type"), "text");
    assert_eq!(one("= 5_km | type"), "quantity");
    assert_eq!(one("= null | type"), "null");
    assert_eq!(one("= @(1; 2) | type"), "list");
    assert_eq!(one("= %(a = 1) | type"), "record");
    assert_eq!(one("= 2025- | period | type"), "period");
    assert_eq!(
        one(r#"= #(@(n; s; q); %(1; "x"; 5_km); %(2; "y"; 3_m)) | type"#),
        r#"@("int"; "text"; "quantity")"#
    );
    assert_eq!(
        one(r#"= #(@(n; s; q); %(1; "x"; 5_km); %(2; "y"; 3_m)) | dimension"#),
        r#"@(null; null; "L")"#
    );
    let err = run("= 5 | types").unwrap_err().to_string();
    assert!(err.contains("retired") && err.contains("'type'"), "{err}");
}

#[test]
fn a_span_of_time_is_the_quantity_of_dimension_t() {
    assert_eq!(one("= 90_s | type"), "duration");
    assert_eq!(one("= 90_s | dimension"), "T");
    assert_eq!(one("= 1_h + 30_min"), "1.5_h");
    assert_eq!(one("= 350_s + 1_min"), "410_s");
    assert_eq!(one("= 30 | days"), "30_d");
    assert_eq!(one("= 30 | days | hours"), "720");
    assert_eq!(one("= (1_w = 7_d)"), "true");
    assert_eq!(one("= '5 hours' | quantity"), "5_hours");
    assert_eq!(one("= '1.5 h' | duration"), "1.5_h");
    assert_eq!(one("= 'PT1M30S' | duration"), "1min 30s");
    assert_eq!(one("= '5d 3h' | duration"), "5d 3h");
    assert_eq!(one("= 90_s | duration"), "90_s");
    // A bare number has no durational reading: time is a dimension
    // like the others (spec: the durational coercion).
    assert_eq!(one("= 90 | duration"), "null");
    assert_eq!(one("= 'soon' | duration"), "null");
    // instant arithmetic: a span out, a span in
    assert_eq!(
        one("= (2026-07-12T09:00:00Z | datetime) - (2026-07-12T07:30:00Z | datetime)"),
        "1h 30min"
    );
    assert_eq!(
        one("= (2026-07-12T07:30:00Z | datetime) + 90_min"),
        "2026-07-12T09:00:00Z"
    );
    assert_eq!(one("= 2_h * 3"), "6_h");
    assert_eq!(one("= 3_h div 2"), "1.5_h");
    // zero is falsy
    assert_eq!(one("= (0_s || false)"), "false");
    assert_eq!(one("= (1_s || false)"), "true");
}

#[test]
fn scale_walks_the_time_ladder_and_dfmt_writes_two_components() {
    assert_eq!(one("= 5400_s | scale"), "1.5_h");
    assert_eq!(one("= 90_s | scale"), "1.5_min");
    assert_eq!(one("= 0.002_s | scale"), "2_ms");
    assert_eq!(one("= 0.000002_s | scale"), "2_us");
    assert_eq!(one("= 1209600_s | scale"), "14_d");
    assert_eq!(one("= 90_s | dfmt"), "1min 30s");
    assert_eq!(one("= 5400_s | dfmt"), "1h 30min");
    assert_eq!(one("= 45_s | dfmt"), "45s");
    assert_eq!(one("= 8_d | dfmt"), "8d");
    assert_eq!(one("= 1.5_h | dfmt"), "1h 30min");
    assert_eq!(one("= 5400_s | dfmt(iso)"), "PT1H30M");
    assert_eq!(one("= 'x' | dfmt"), "null");
    assert_eq!(one("= 2.5_h | dfmt"), "2h 30min");
    assert_eq!(one("= 0.0015_s | duration"), "0.0015_s");
    assert_eq!(one("= 0.0015_s | dfmt"), "1ms 500us");
    assert_eq!(one("= 0_s | duration"), "0_s");
    assert_eq!(one("= 6912000_s | duration"), "6912000_s");
    assert_eq!(one("= 6912000_s | dfmt"), "80d");
    assert_eq!(one("= '1h 30min' | duration | hours"), "1.5");
    assert_eq!(one("= 1h30min | duration | minutes"), "90");
    assert_eq!(
        one("= (2026-07-12T09:00:00Z | datetime) - (2026-07-11T07:30:00Z | datetime) | json"),
        "\"1d 1h 30min\""
    );
    // what dfmt writes reads back
    assert_eq!(one("= 5400_s | dfmt | duration"), "1h 30min");
    assert_eq!(one("= 8_d | dfmt | duration | days"), "8");
}

#[test]
fn unit_magnitude_and_the_base_form() {
    assert_eq!(one("= 5_km | unit"), "km");
    assert_eq!(one("= 12_EUR/kg | unit"), "EUR/kg");
    assert_eq!(one("= 5_km | convert(mi) | unit"), "mi");
    assert_eq!(one("= 5 | unit"), "null");
    assert_eq!(one("= 5 | unit(km)"), "5_km");
    assert_eq!(one("= 5_km | unit(m)"), "5000_m");
    assert_eq!(one("= 5_km | unit(kg)"), "null");
    assert_eq!(all("/rows/*::id | unit(B)"), "1_B | 2_B | 3_B");
    assert_eq!(one("= 5_km | magnitude"), "5");
    assert_eq!(one("= 1.5_h | magnitude"), "1.5");
    assert_eq!(one("= 1_km | magnitude(m)"), "1000");
    assert_eq!(one("= 1_km | magnitude(kg)"), "null");
    assert_eq!(one("= 'x' | magnitude"), "null");
    assert_eq!(one("= 5_km | convert"), "5000_m");
    assert_eq!(one("= 60_km/h | convert | round(3)"), "16.667_m/s");
    assert_eq!(one("= 2_KiB | convert"), "16384_b");
    assert_eq!(one("= 1.5_h | convert"), "5400_s");
    assert_eq!(one("= 'x' | convert"), "null");
}

#[test]
fn micro_and_omega_read_and_prefixes_are_exact() {
    // µ (U+00B5) and μ (U+03BC) are the prefix u; Ω and the ohm sign are ohm
    assert_eq!(one("= 5_µm | convert(m)"), "5e-06_m");
    assert_eq!(one("= 5_μm | convert(mm)"), "0.005_mm");
    assert_eq!(one("= 5_μm"), "5_um");
    assert_eq!(one("= 5_kΩ | convert(ohm)"), "5000_ohm");
    assert_eq!(one("= 5_\u{2126} | convert(V/A)"), "5_V/A");
    assert_eq!(one("= 5_kΩ"), "5_kohm");
    assert_eq!(one("= 5_kΩ | unit"), "kohm");
    assert_eq!(one("= 5_µm | unit"), "um");
    assert_eq!(one("= 5_кг | unit"), "kg");
    // a prefix is one power of ten applied once
    assert_eq!(one("= 5_um | convert(m)"), "5e-06_m");
    assert_eq!(one("= 0.005_s | scale"), "5_ms");
}

#[test]
fn a_bare_compound_unit_is_one_operand() {
    assert_eq!(one("= 5_m/s | convert(km/h)"), "18_km/h");
    assert_eq!(one("= 5_m/s | convert(\"km/h\")"), "18_km/h");
    assert_eq!(one("= 9.81_m/s^2 | unit"), "m/s^2");
    assert_eq!(one("= 3_kg*m/s^2 | convert(N)"), "3_N");
    assert_eq!(one("= 36_km/h | magnitude(m/s)"), "10");
    assert_eq!(one("= 5_ohm | unit(V/A)"), "5_V/A");
}

/// `duration(FROM; TO)` is the duration from one instant to another
/// (spec: Duration): TO − FROM, negative when TO precedes FROM, null
/// when either does not coerce; the topic fills FROM in command
/// position, and the bare cast stays.
#[test]
fn a_duration_between_two_instants() {
    assert_eq!(one("= duration('1775-12-16'; '1817-07-18')"), "15189d");
    assert_eq!(one("= '1775-12-16' | duration('1817-07-18')"), "15189d");
    assert_eq!(one("= '1775-12-16' | td('1817-07-18')"), "15189d");
    assert_eq!(one("= duration('1817-07-18'; '1775-12-16')"), "-15189d");
    assert_eq!(
        one("= duration('1775-12-16'; '1817-07-18') | days"),
        "15189"
    );
    assert_eq!(
        one("= duration('2024-03-05T09:00:00Z'; '2024-03-05T10:30:00Z')"),
        "1h 30min"
    );
    assert_eq!(one("= duration(0; 90)"), "1min 30s");
    assert_eq!(one("= duration('x'; '1817-07-18')"), "null");
    // in command position with both arguments, the topic is only the
    // capsa the expressions read
    assert_eq!(
        one("/rows/*[1] | duration(::when; '2025-10-07') | days"),
        "1"
    );
    assert_eq!(
        one("/rows/*[2] | duration(::when; '2024-01-02') | hours"),
        "24"
    );
    assert_eq!(one("= duration('1775-12-16'; 'x')"), "null");
    assert_eq!(one("= '1h 30min' | duration"), "1h 30min");
}

/// The mixed span form a Russian output locale prints reads back as
/// a duration under any locale: the GOST designations read always.
#[test]
fn a_cyrillic_mixed_span_reads_back() {
    assert_eq!(one("= '1ч 30мин' | duration"), "1h 30min");
    assert_eq!(one("= '1 ч 30 мин' | duration"), "1h 30min");
    assert_eq!(one("= '1h 30min' | duration"), "1h 30min");
    assert_eq!(one("= '1ч 30мин' | duration | minutes"), "90");
    assert_eq!(one("= '2сут 3ч' | dfmt"), "2d 3h");
}

/// dfmt(LOCALE) writes the mixed form in the locale's duration
/// designations (spec: Duration Designations by Locale); bare dfmt
/// stays Latin under every environment; every designation reads
/// back under any locale.
#[test]
fn dfmt_writes_a_locales_designations() {
    assert_eq!(one("= 443100_s | duration | dfmt"), "5d 3h 5min");
    assert_eq!(one("= 443100_s | duration | dfmt(en)"), "5d 3h 5min");
    assert_eq!(one("= 443100_s | duration | dfmt(fr)"), "5 j 3 h 5 min");
    assert_eq!(
        one("= 443100_s | duration | dfmt('fr-CA')"),
        "5 j 3 h 5 min"
    );
    assert_eq!(
        one("= 443100_s | duration | dfmt(de)"),
        "5 Tg. 3 Std. 5 Min."
    );
    assert_eq!(one("= 443100_s | duration | dfmt(es)"), "5 d 3 h 5 min");
    assert_eq!(one("= 443100_s | duration | dfmt(ru)"), "5 сут 3 ч 5 мин");
    assert_eq!(one("= 443100_s | duration | dfmt(zh)"), "5天 3小时 5分钟");
    assert_eq!(one("= 443100_s | duration | dfmt(ja)"), "5 日 3 時間 5 分");
    assert_eq!(one("= 443100_s | duration | dfmt(tr)"), "5 gün 3 sa. 5 dk.");
    // word designations take the plural form of the count
    assert_eq!(one("= 90000_s | duration | dfmt(it)"), "1 giorno 1 h");
    assert_eq!(one("= 172800_s | duration | dfmt(it)"), "2 giorni");
    assert_eq!(one("= 86400_s | duration | dfmt(pt)"), "1 dia");
    assert_eq!(one("= 432000_s | duration | dfmt(pt)"), "5 dias");
    assert_eq!(one("= 86400_s | duration | dfmt(ar)"), "يوم");
    assert_eq!(one("= 172800_s | duration | dfmt(ar)"), "يومان");
    assert_eq!(one("= 259200_s | duration | dfmt(ar)"), "3 أيام");
    assert_eq!(one("= 1.5_s | duration | dfmt(ar)"), "1 ث 500 ملي ث");
    assert_eq!(one("= 0_s | duration | dfmt(de)"), "0 Sek.");
    assert_eq!(one("= 443100_s | duration | dfmt(iso)"), "P5DT3H5M");
    let err = refuse("= 90_s | duration | dfmt(xx)");
    assert!(
        err.contains("no duration designations for locale \"xx\""),
        "{err}"
    );
    // every designation reads back, under any locale
    for text in [
        "5 j 3 h 5 min",
        "5 Tg. 3 Std. 5 Min.",
        "5天3小时5分钟",
        "5 日 3 時間 5 分",
        "5 gün 3 sa. 5 dk.",
        "5 dias 3 h 5 min",
        "5 giorni 3 h 5 min",
        "5 أيام 3 س 5 د",
        "5 сут 3 ч 5 мин",
    ] {
        assert_eq!(
            one(&format!("= '{text}' | duration")),
            "5d 3h 5min",
            "{text}"
        );
    }
    assert_eq!(one("= 'يوم' | duration"), "1d");
    assert_eq!(one("= 'يومان 3 س' | duration"), "2d 3h");
    assert_eq!(one("= '1 giorno' | duration | hours"), "24");
}

#[test]
fn two_texts_that_coerce_compare_by_magnitude() {
    assert_eq!(one("= (\"800 g\" > \"1 kg\")"), "false");
    assert_eq!(one("= (\"2.5 kg\" > \"1 kg\")"), "true");
    assert_eq!(one("= (\"800 g\" = \"0.8 kg\")"), "true");
    assert_eq!(one("= (\"900 г\" > \"1 кг\")"), "false");
    // a different dimension, or no unit at all, compares as text
    assert_eq!(one("= (\"800 g\" > \"1 m\")"), "true");
    assert_eq!(one("= (\"abc\" > \"abd\")"), "false");
    assert_eq!(one("= (\"10\" > \"9\")"), "true");
}
