//! Jinja2 string templating for the `QuickJS` sandbox (`$std.template`).
//!
//! Backed by `minijinja` — pure Rust, single required dependency (`serde`), no C toolchain and no
//! second crypto stack. Always injected (it is pure: rendering `(source, context)` is a function of
//! its inputs, so it is safe under both `Profile::Full` and `Profile::Deterministic`). The
//! `Environment` is built with only `minijinja`'s default, deterministic builtins — no clock,
//! randomness, or ambient reads are exposed to a template.
//!
//! JS API: `$std.template.html(source)` (HTML auto-escaping) / `$std.template.text(source)` (no
//! escaping) each return a compiled-template object with `.render(context)`, `.missing(placeholder)`,
//! and `.fields()`. Every op crosses the FFI boundary as strings:
//! `__template(op, source, arg2, arg3)` returns a `{"v":"<string>"}` scalar envelope, a
//! `{"list":[…]}` array envelope (`fields`), or `{"error":"…"}`.

use std::error::Error;

use minijinja::{
    AutoEscape, Environment, Error as MjError, ErrorKind, UndefinedBehavior, escape_formatter,
};
use rquickjs::{Ctx, Function, Value as JsValue};
use serde_json::Value as JsonValue;

use crate::sandbox;

/// JS wrapper — loaded from `src/js/template.js` at compile time. Built lazily on first
/// `$std.template` access by the engine's lazy-`$std` path.
pub(crate) const TEMPLATE_WRAPPER: &str = include_str!("js/template.js");

/// Registers the eager `__template` FFI bridge (the cheap native half); the `$std.template` wrapper
/// is built lazily by the engine on first access (D2).
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_native(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let template_fn = Function::new(
        qctx.clone(),
        |op: String, source: String, arg2: String, arg3: String| -> String {
            match dispatch(&op, &source, &arg2, &arg3) {
                Ok(json) => json,
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__template")?;

    qctx.globals().set("__template", template_fn)?;
    Ok(())
}

/// Injects `$std.template` eagerly (native bridge + wrapper). Retained for the module's own unit
/// tests; the engine registers the native eagerly and builds the wrapper lazily instead.
///
/// # Errors
///
/// Returns an error if registration or JS eval fails.
pub fn inject_template(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    register_native(qctx)?;
    let wrapper: JsValue<'_> = qctx.eval(TEMPLATE_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__template` call to the right operation. `arg2`/`arg3` carry per-op auxiliary
/// arguments: for `render`, `arg2` is the JSON context and `arg3` the `{"html","missing"}` options;
/// `check`/`fields` use only `source`.
fn dispatch(op: &str, source: &str, arg2: &str, arg3: &str) -> Result<String, String> {
    match op {
        "check" => check(source).map(|()| value_json("")),
        "render" => render(source, arg2, arg3).map(|out| value_json(&out)),
        "fields" => fields(source).map(|names| list_json(&names)),
        other => Err(format!("unknown template op: {other}")),
    }
}

// -- Options ----------------------------------------------------------------

/// The per-render options the JS wrapper encodes into `arg3`.
#[derive(serde::Deserialize)]
struct RenderOpts {
    /// Whether to HTML auto-escape interpolated values (`html` mode vs `text` mode).
    #[serde(default)]
    html: bool,
    /// Placeholder rendered in place of an undefined merge tag (empty ⇒ render undefined as empty).
    #[serde(default)]
    missing: String,
}

/// Parses the `{"html","missing"}` options envelope, treating an empty string as the defaults.
fn parse_opts(opts_json: &str) -> Result<RenderOpts, String> {
    if opts_json.trim().is_empty() {
        return Ok(RenderOpts {
            html: false,
            missing: String::new(),
        });
    }
    serde_json::from_str(opts_json).map_err(|err| format!("invalid template options: {err}"))
}

// -- Operations -------------------------------------------------------------

/// Compiles `source` to validate its syntax, discarding the result. Lets `$std.template.html`/`text`
/// surface a syntax error eagerly at construction, before any render.
fn check(source: &str) -> Result<(), String> {
    let env = Environment::new();
    env.template_from_str(source)
        .map(|_template| ())
        .map_err(|err| err.to_string())
}

/// Renders `source` against `context_json` in the mode encoded by `opts_json`.
///
/// Undefined variables are lenient (chainable ⇒ `a.b` on a missing `a` is undefined, not an error)
/// and render as the `missing` placeholder (empty by default). Auto-escaping follows the `html` flag.
fn render(source: &str, context_json: &str, opts_json: &str) -> Result<String, String> {
    let opts = parse_opts(opts_json)?;
    let context: JsonValue = serde_json::from_str(context_json)
        .map_err(|err| format!("invalid template context: {err}"))?;

    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    let escape = if opts.html {
        AutoEscape::Html
    } else {
        AutoEscape::None
    };
    env.set_auto_escape_callback(move |_name| escape);
    if !opts.missing.is_empty() {
        let placeholder = opts.missing;
        env.set_formatter(move |out, state, value| {
            if value.is_undefined() {
                out.write_str(&placeholder)
                    .map_err(|err| MjError::new(ErrorKind::WriteFailure, err.to_string()))
            } else {
                escape_formatter(out, state, value)
            }
        });
    }

    let template = env
        .template_from_str(source)
        .map_err(|err| err.to_string())?;
    template.render(&context).map_err(|err| err.to_string())
}

/// Returns the top-level merge tags `source` references, sorted for deterministic order.
fn fields(source: &str) -> Result<Vec<String>, String> {
    let env = Environment::new();
    let template = env
        .template_from_str(source)
        .map_err(|err| err.to_string())?;
    let mut names: Vec<String> = template.undeclared_variables(false).into_iter().collect();
    names.sort();
    Ok(names)
}

// -- Output -----------------------------------------------------------------

/// Builds the success envelope `{"v":"<value>"}`.
fn value_json(value: &str) -> String {
    let escaped = serde_json::to_string(value).unwrap_or_else(|_err| "\"\"".to_owned());
    format!("{{\"v\":{escaped}}}")
}

/// Builds the list envelope `{"list":["a","b",…]}` (the `fields` shape).
fn list_json(values: &[String]) -> String {
    let escaped = serde_json::to_string(values).unwrap_or_else(|_err| "[]".to_owned());
    format!("{{\"list\":{escaped}}}")
}

#[cfg(test)]
mod tests {
    use super::{check, fields, render};

    /// Convenience: render in `text` mode with no placeholder.
    fn text(source: &str, context_json: &str) -> Result<String, String> {
        render(source, context_json, "{\"html\":false}")
    }

    /// Convenience: render in `html` mode with no placeholder.
    fn html(source: &str, context_json: &str) -> Result<String, String> {
        render(source, context_json, "{\"html\":true}")
    }

    #[test]
    fn html_mode_escapes_interpolated_values() {
        assert_eq!(
            html("<p>{{ name }}</p>", "{\"name\":\"<b>&x\"}"),
            Ok("<p>&lt;b&gt;&amp;x</p>".to_owned())
        );
    }

    #[test]
    fn text_mode_emits_values_verbatim() {
        assert_eq!(
            text("Hi {{ name }}", "{\"name\":\"<b>&x\"}"),
            Ok("Hi <b>&x".to_owned())
        );
    }

    #[test]
    fn statements_and_expressions_render() {
        assert_eq!(
            text(
                "{% for i in items %}{{ i }},{% endfor %}",
                "{\"items\":[1,2,3]}"
            ),
            Ok("1,2,3,".to_owned())
        );
    }

    #[test]
    fn nested_context_access() {
        assert_eq!(
            text(
                "{{ user.name }} owes {{ amount }}",
                "{\"user\":{\"name\":\"Ada\"},\"amount\":\"10.00\"}"
            ),
            Ok("Ada owes 10.00".to_owned())
        );
    }

    #[test]
    fn missing_variable_renders_empty_by_default() {
        assert_eq!(text("A{{ gap }}B", "{}"), Ok("AB".to_owned()));
    }

    #[test]
    fn missing_nested_access_is_lenient() {
        // Chainable undefined: `user.name` on an absent `user` is undefined, not an error.
        assert_eq!(text("[{{ user.name }}]", "{}"), Ok("[]".to_owned()));
    }

    #[test]
    fn placeholder_substitutes_for_missing_variables() {
        assert_eq!(
            render("A{{ gap }}B", "{}", "{\"html\":false,\"missing\":\"—\"}"),
            Ok("A—B".to_owned())
        );
    }

    #[test]
    fn fields_lists_referenced_variables() {
        assert_eq!(
            fields("{{ first }} {{ last }} — {{ first }}"),
            Ok(vec!["first".to_owned(), "last".to_owned()])
        );
    }

    #[test]
    fn fields_is_empty_for_a_static_template() {
        assert_eq!(fields("no variables here"), Ok(Vec::<String>::new()));
    }

    #[test]
    fn malformed_template_is_an_error_not_a_panic() {
        assert!(check("{{ unclosed ").is_err());
        assert!(text("{{ unclosed ", "{}").is_err());
    }

    #[test]
    fn render_is_deterministic() {
        let ctx = "{\"n\":\"Ada\"}";
        assert_eq!(text("Hi {{ n }}", ctx), text("Hi {{ n }}", ctx));
    }
}
