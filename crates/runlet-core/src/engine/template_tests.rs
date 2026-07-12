use super::{ExecOutcome, ExecParams, Profile, run};
use rquickjs::Runtime;
use std::time::Duration;

/// Runs `script` under `profile` and returns the success `data`/`error` envelope JSON.
fn run_profiled(script: &str, profile: Profile) -> String {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let params = ExecParams {
        runtime: &runtime,
        bytecode_cache: None,
        surface: None,
        cache_namespace: None,
        script,
        context_json: "{}",
        timeout: Duration::from_secs(5),
        profile,
        sys_config: None,
        registry: None,
        enabled_io: &[],
        egress: None,
        default_currency: None,
        max_ops: 4096,
        max_emit_kind_len: 64,
        log_level: super::LogLevel::Info,
        max_log_entries: 256,
        max_log_entry_bytes: 256 * 1024,
        max_log_total_bytes: 1024 * 1024,
        max_output_size: 0,
        allow_private_targets: false,
    };
    let result = run(&params).unwrap_or_else(|_err| unreachable!());
    match result.outcome {
        ExecOutcome::Success(json) => json,
        ExecOutcome::Error(err) => format!("ERROR: {err:?}"),
    }
}

/// Convenience: run under the full profile.
fn run_script(script: &str) -> String {
    run_profiled(script, Profile::Full)
}

/// HTML mode escapes interpolated values; text mode emits them verbatim.
#[test]
fn escaping_modes() {
    let out = run_script(
        "function handler() { \
               return json({ \
                 html: $std.template.html('<p>{{ name }}</p>').render({ name: '<b>&x' }), \
                 text: $std.template.text('Hi {{ name }}').render({ name: '<b>&x' }) }); }",
    );
    assert!(
        out.contains("\"html\":\"<p>&lt;b&gt;&amp;x</p>\""),
        "html escapes: {out}"
    );
    assert!(
        out.contains("\"text\":\"Hi <b>&x\""),
        "text verbatim: {out}"
    );
}

/// Statements/expressions render, and nested context access resolves.
#[test]
fn statements_and_nested_access() {
    let out = run_script(
        "function handler() { \
               return json({ \
                 loop: $std.template.text('{% for i in items %}{{ i }},{% endfor %}').render({ items: [1, 2, 3] }), \
                 nested: $std.template.text('{{ user.name }}').render({ user: { name: 'Ada' } }) }); }",
    );
    assert!(out.contains("\"loop\":\"1,2,3,\""), "{out}");
    assert!(out.contains("\"nested\":\"Ada\""), "{out}");
}

/// Undefined merge tags render empty by default; `.missing()` substitutes a placeholder; the
/// receiver is immutable (the two renders share one compiled template).
#[test]
fn lenient_undefined_and_placeholder() {
    let out = run_script(
        "function handler() { \
               const tpl = $std.template.text('A{{ gap }}B'); \
               return json({ empty: tpl.render({}), dash: tpl.missing('-').render({}), again: tpl.render({}) }); }",
    );
    assert!(out.contains("\"empty\":\"AB\""), "{out}");
    assert!(out.contains("\"dash\":\"A-B\""), "{out}");
    assert!(
        out.contains("\"again\":\"AB\""),
        "missing() is immutable: {out}"
    );
}

/// `.fields()` reports the top-level merge tags (sorted, de-duplicated).
#[test]
fn fields_lists_merge_tags() {
    let out = run_script(
        "function handler() { \
               return json({ f: $std.template.text('{{ first }} {{ last }} {{ first }}').fields() }); }",
    );
    assert!(out.contains("\"f\":[\"first\",\"last\"]"), "{out}");
}

/// A malformed template throws a catchable Error at construction, never crashing the runtime.
#[test]
fn malformed_template_throws() {
    let out = run_script(
        "function handler() { \
               try { $std.template.text('{{ unclosed '); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
    );
    assert!(out.contains("\"caught\":true"), "{out}");
}

/// `$std.template` is pure, so it is present and identical under the deterministic profile.
#[test]
fn available_and_pure_under_deterministic() {
    let script = "function handler() { \
             return json({ \
               defined: typeof $std.template.html === 'function', \
               out: $std.template.text('Hi {{ n }}').render({ n: 'Ada' }) }); }";
    let full = run_profiled(script, Profile::Full);
    let deterministic = run_profiled(script, Profile::Deterministic);
    assert!(
        deterministic.contains("\"defined\":true"),
        "template available under deterministic: {deterministic}"
    );
    assert!(
        full.contains("\"out\":\"Hi Ada\"") && deterministic.contains("\"out\":\"Hi Ada\""),
        "render is identical across profiles: full={full} det={deterministic}"
    );
}
