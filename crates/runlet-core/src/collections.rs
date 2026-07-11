//! The first-class `list` and `dict` collection value-utils for the `QuickJS` sandbox.
//!
//! `list` (a table of records) and `dict` (one record) are the always-on collection globals beside
//! `$`/`$std.decimal`/`$std.datetime`/`$std.text`: callable factories (`list(input)` / `dict(input)`) that
//! wrap a value as an immutable collection with chainable, `snake_case`, **field-name-first** methods
//! (no callbacks). The names are the SQL / Shopify-Liquid vocabulary the ERP / e-commerce audience
//! already half-knows.
//!
//! Pure JS (`js/list.js` + `js/dict.js`) — no `__sys` bridge and no new Rust math: the only value
//! they reach past `Array`/`Object` is the already-injected `$std.decimal` util, which `list`'s column
//! aggregates (`sum`/`avg`/`min`/`max`) use so a currency column is summed EXACTLY, never as a float.
//! Both are injected under **both** profiles: they touch no clock, no randomness, and no ambient
//! authority, so the deterministic sanitizer (`js/determinism.js`) removes nothing from them (which
//! is also why neither exposes a random-order verb). `list.group_by` returns a `dict` and
//! `dict.entries`/`keys`/`values` return a `list`; each resolves the other global at call time, so
//! injection order between the two is flexible — but both must follow `decimal::inject_decimal`.

use std::error::Error;

use rquickjs::{Ctx, Value as JsValue};

/// `list` JS wrapper — loaded from `src/js/list.js` at compile time. Reads `$std.decimal` at
/// call time (for aggregates) and `$std.dict` (for `group_by`).
pub(crate) const LIST_WRAPPER: &str = include_str!("js/list.js");
/// `dict` JS wrapper — loaded from `src/js/dict.js` at compile time. Reads `$std.list` at call
/// time (for `keys`/`values`/`entries`).
pub(crate) const DICT_WRAPPER: &str = include_str!("js/dict.js");

/// Injects the `list` and `dict` globals.
///
/// Must run after `$std.decimal` is present (list aggregates compose over it); order between
/// `list` and `dict` is free (each resolves the other at call time).
///
/// # Errors
///
/// Returns an error if either JS eval fails.
pub fn inject_collections(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let dict_val: JsValue<'_> = qctx.eval(DICT_WRAPPER)?;
    drop(dict_val);
    let list_val: JsValue<'_> = qctx.eval(LIST_WRAPPER)?;
    drop(list_val);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Behavioral tests for the `list`/`dict` value-utils, driven end-to-end through the QuickJS
    //! engine (the surface is pure JS, so there is no Rust `dispatch` to unit-test directly). Each
    //! test injects `Decimal` (aggregates depend on it) + the collection wrappers and evals an
    //! assertion expression.

    use rquickjs::{Context, Runtime};

    use super::inject_collections;
    use crate::decimal::inject_decimal;

    /// The deterministic-profile sanitizer, evaled to prove the collections survive it untouched.
    const DETERMINISM: &str = include_str!("js/determinism.js");

    /// Bootstraps `$std` (the wrappers now populate it, not `globalThis`), injects `decimal` + the
    /// collections, then mirrors `decimal`/`list`/`dict` back to bare globals so these behavioral
    /// expressions read naturally. In production the engine does the bootstrap + projection; here
    /// the harness stands in for it. When `money` is `true`, `$`/`money` are injected + projected too.
    fn inject(qctx: &rquickjs::Ctx<'_>, money: bool) {
        qctx.eval::<(), _>("globalThis.$std = {};")
            .expect("bootstrap std");
        inject_decimal(qctx).expect("inject decimal");
        if money {
            crate::money::inject_money(qctx, None).expect("inject money");
        }
        inject_collections(qctx).expect("inject collections");
        // Mirror the utils these tests reference by bare name; `Decimal` keeps its historical global
        // spelling here even though the canonical namespace path is `$std.decimal`.
        qctx.eval::<(), _>(
            "globalThis.Decimal = $std.decimal; globalThis.list = $std.list; \
             globalThis.dict = $std.dict;",
        )
        .expect("project collections");
        if money {
            qctx.eval::<(), _>("globalThis.$ = $std.money; globalThis.money = $std.money;")
                .expect("project money");
        }
    }

    /// Inject `Decimal` + the collections and eval a JS expression that yields a string.
    fn run(expr: &str) -> String {
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        ctx.with(|qctx| {
            inject(&qctx, false);
            qctx.eval::<String, _>(expr).expect("eval")
        })
    }

    /// Like [`run`], but also injects `$`/`money` (after `Decimal`) so the `list` verbs can be
    /// exercised over real `money` values.
    fn run_money(expr: &str) -> String {
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        ctx.with(|qctx| {
            inject(&qctx, true);
            qctx.eval::<String, _>(expr).expect("eval")
        })
    }

    // ---- list: unwrap / interop / access --------------------------------

    #[test]
    fn list_unwrap_and_tojson() {
        assert_eq!(run(r"JSON.stringify(list([1,2]).to_array())"), "[1,2]");
        assert_eq!(run(r"JSON.stringify(list([1,2]))"), "[1,2]");
    }

    #[test]
    fn list_positional_access_and_iteration() {
        assert_eq!(run(r#"String(list(["a","b"]).get(1))"#), "b");
        assert_eq!(run(r#"String(list(["a","b"]).at(-1))"#), "b");
        assert_eq!(run(r#"String(list(["a","b"]).len())"#), "2");
        assert_eq!(
            run(r#"JSON.stringify([...list(["a","b"])])"#),
            r#"["a","b"]"#
        );
    }

    #[test]
    fn list_first_last_and_empty() {
        assert_eq!(run(r"String(list([10,20,30]).first())"), "10");
        assert_eq!(run(r"String(list([10,20,30]).last())"), "30");
        assert_eq!(run(r"String(list([]).first())"), "null");
    }

    #[test]
    fn list_transforms_do_not_mutate_receiver() {
        // sort_by returns a new list; the original ordering is unchanged.
        assert_eq!(
            run(
                r"(function(){var l=list([3,1,2]);var s=JSON.stringify(l.sort_by().to_array());return s+'|'+JSON.stringify(l.to_array());})()"
            ),
            "[1,2,3]|[3,1,2]"
        );
    }

    // ---- list: field-name-first shaping ---------------------------------

    #[test]
    fn list_where_sort_column_unique() {
        assert_eq!(
            run(r#"String(list([{s:"paid"},{s:"open"},{s:"paid"}]).where({s:"paid"}).len())"#),
            "2"
        );
        assert_eq!(
            run(r"JSON.stringify(list([{p:3},{p:1},{p:2}]).sort_by('p').column('p').to_array())"),
            "[1,2,3]"
        );
        assert_eq!(
            run(
                r"JSON.stringify(list([{p:3},{p:1},{p:2}]).sort_by('p','desc').column('p').to_array())"
            ),
            "[3,2,1]"
        );
        assert_eq!(
            run(r#"JSON.stringify(list([{email:"a"},{email:"b"}]).column("email").to_array())"#),
            r#"["a","b"]"#
        );
        assert_eq!(
            run(r"JSON.stringify(list([1,2,2,3]).unique().to_array())"),
            "[1,2,3]"
        );
        assert_eq!(
            run(r"String(list([{id:1},{id:1},{id:2}]).unique_by('id').len())"),
            "2"
        );
    }

    #[test]
    fn list_group_by_bridges_to_dict() {
        // group_by returns a dict of lists; look up a group and confirm its size + membership.
        assert_eq!(
            run(
                r#"var g=list([{r:"x",n:1},{r:"y",n:2},{r:"x",n:3}]).group_by("r");String(g.get("x").len())+'|'+String(g.get("y").len())"#
            ),
            "2|1"
        );
        assert_eq!(
            run(
                r#"JSON.stringify(list([{r:"x",n:1},{r:"y",n:2},{r:"x",n:3}]).group_by("r").get("x").column("n").to_array())"#
            ),
            "[1,3]"
        );
    }

    // ---- list: exact-Decimal aggregates ---------------------------------

    #[test]
    fn list_sum_is_exact_currency() {
        // The whole point: 0.1 + 0.2 must be exactly 0.3, never 0.30000000000000004.
        assert_eq!(
            run(r#"list([{t:"0.1"},{t:"0.2"}]).sum("t").toString()"#),
            "0.3"
        );
    }

    #[test]
    fn list_avg_min_max_return_decimals() {
        assert_eq!(run(r"list([{n:1},{n:2},{n:3}]).avg('n').toString()"), "2");
        assert_eq!(run(r"list([{n:1},{n:2},{n:3}]).min('n').toString()"), "1");
        assert_eq!(run(r"list([{n:1},{n:2},{n:3}]).max('n').toString()"), "3");
    }

    #[test]
    fn list_count_number_and_empty_aggregates() {
        assert_eq!(run(r"String(list([1,2,3]).count())"), "3");
        assert_eq!(run(r"String(list([]).avg('n'))"), "null");
        assert_eq!(run(r"String(list([]).min('n'))"), "null");
        // empty sum is Decimal(0).
        assert_eq!(run(r"list([]).sum('n').toString()"), "0");
    }

    #[test]
    fn list_aggregates_skip_non_numeric_and_absent() {
        // "abc", true, missing field, and null are skipped; only 1 and "2" count.
        assert_eq!(
            run(r#"list([{n:1},{n:"abc"},{n:true},{x:9},{n:null},{n:"2"}]).sum("n").toString()"#),
            "3"
        );
    }

    // ---- list: value-util interop (money/decimal wrappers) --------------

    #[test]
    fn list_sum_money_returns_money_preserving_currency() {
        // A money column sums to a money value (currency kept), not a bare Decimal, and never 0.
        assert_eq!(
            run_money(r#"list([{t:$("0.10","USD")},{t:$("0.20","USD")}]).sum("t").format()"#),
            "$0.30"
        );
        assert_eq!(
            run_money(r#"list([{t:$("0.10","USD")},{t:$("0.20","USD")}]).sum("t").currency()"#),
            "USD"
        );
    }

    #[test]
    fn list_sum_mixed_currency_throws() {
        assert_eq!(
            run_money(
                r#"(function(){try{list([{t:$("1","USD")},{t:$("1","EUR")}]).sum("t");return "no-throw";}catch(e){return "threw";}})()"#
            ),
            "threw"
        );
    }

    #[test]
    fn list_min_max_money_preserve_currency() {
        // min/max over a money column return money values (currency kept), not bare decimals.
        assert_eq!(
            run_money(
                r#"var m=list([{t:$("5","USD")},{t:$("2","USD")}]).min("t");m.to_string()+" "+m.currency()"#
            ),
            "2 USD"
        );
        assert_eq!(
            run_money(
                r#"var m=list([{t:$("5","USD")},{t:$("2","USD")}]).max("t");m.to_string()+" "+m.currency()"#
            ),
            "5 USD"
        );
    }

    #[test]
    fn list_sort_by_money_is_numeric_not_lexical() {
        // Lexically "100.00" < "19.99" < "5.00"; numerically the order must be 5, 19.99, 100.
        assert_eq!(
            run_money(
                r#"JSON.stringify(list([{t:$("100.00","USD")},{t:$("19.99","USD")},{t:$("5.00","USD")}]).sort_by("t").column("t").to_array().map(function(m){return m.to_string();}))"#
            ),
            r#"["5.00","19.99","100.00"]"#
        );
    }

    #[test]
    fn list_group_by_money_keeps_currency_distinct() {
        // USD 19.99 and EUR 19.99 must not collide into one group.
        assert_eq!(
            run_money(
                r#"String(list([{p:$("19.99","USD")},{p:$("19.99","EUR")}]).group_by("p").keys().len())"#
            ),
            "2"
        );
    }

    #[test]
    fn list_unique_dedupes_equal_money_by_amount_and_currency() {
        // Two equal USD values collapse; the EUR value differs by currency and survives.
        assert_eq!(
            run_money(r#"String(list([$("1","USD"),$("1","USD"),$("1","EUR")]).unique().len())"#),
            "2"
        );
    }

    #[test]
    fn list_oversize_input_is_refused() {
        // A factory input beyond the output cap throws before copying, rather than allocating.
        assert_eq!(
            run(
                r#"(function(){try{list(new Array(2000000));return "no-throw";}catch(e){return "threw";}})()"#
            ),
            "threw"
        );
    }

    // ---- dict -----------------------------------------------------------

    #[test]
    fn dict_unwrap_and_tojson() {
        assert_eq!(
            run(r"JSON.stringify(dict({a:1}).to_object())"),
            r#"{"a":1}"#
        );
        assert_eq!(run(r"JSON.stringify(dict({a:1}))"), r#"{"a":1}"#);
    }

    #[test]
    fn dict_transforms_do_not_mutate_receiver() {
        assert_eq!(
            run(
                r"(function(){var d=dict({a:1,b:2});var o=JSON.stringify(d.omit('b').to_object());return o+'|'+JSON.stringify(d.to_object());})()"
            ),
            r#"{"a":1}|{"a":1,"b":2}"#
        );
    }

    #[test]
    fn dict_get_dotted_path() {
        assert_eq!(run(r"String(dict({a:{b:{c:42}}}).get('a.b.c'))"), "42");
        assert_eq!(
            run(r#"String(dict({a:{}}).get("a.b.c","fallback"))"#),
            "fallback"
        );
        assert_eq!(run(r#"String(dict({}).get("x.y"))"#), "undefined");
    }

    #[test]
    fn dict_pick_omit_has_merge() {
        assert_eq!(
            run(r"JSON.stringify(dict({a:1,b:2,c:3}).pick('a','c').to_object())"),
            r#"{"a":1,"c":3}"#
        );
        assert_eq!(
            run(r"JSON.stringify(dict({a:1,b:2,c:3}).omit('b').to_object())"),
            r#"{"a":1,"c":3}"#
        );
        assert_eq!(run(r"String(dict({a:1}).has('a'))"), "true");
        assert_eq!(run(r"String(dict({a:1}).has('z'))"), "false");
        assert_eq!(
            run(r"JSON.stringify(dict({a:1,b:2}).merge({b:9,c:3}).to_object())"),
            r#"{"a":1,"b":9,"c":3}"#
        );
    }

    #[test]
    fn dict_keys_values_entries_bridge_to_list() {
        assert_eq!(
            run(r"JSON.stringify(dict({a:1,b:2}).keys().to_array())"),
            r#"["a","b"]"#
        );
        assert_eq!(
            run(r"JSON.stringify(dict({a:1,b:2}).values().to_array())"),
            "[1,2]"
        );
        assert_eq!(
            run(r"JSON.stringify(dict({a:1,b:2}).entries().to_array())"),
            r#"[["a",1],["b",2]]"#
        );
    }

    // ---- both profiles --------------------------------------------------

    #[test]
    fn present_and_identical_under_the_deterministic_sanitizer() {
        // Eval the sanitizer after injecting the collections, then confirm the whole surface still
        // works — they touch no ambient authority, so the deterministic profile removes nothing.
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        let out = ctx.with(|qctx| {
            inject(&qctx, false);
            qctx.eval::<(), _>(DETERMINISM).expect("eval determinism");
            qctx.eval::<String, _>(
                r#"list([{t:"0.1"},{t:"0.2"}]).sum("t").toString() + "|" + dict({a:{b:5}}).get("a.b") + "|" + String(typeof list) + String(typeof dict)"#,
            )
            .expect("eval")
        });
        assert_eq!(out, "0.3|5|functionfunction");
    }
}
