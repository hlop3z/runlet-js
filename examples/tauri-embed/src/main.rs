//! # Embed runlet-core in a Tauri desktop app — a worked example
//!
//! runlet's core is a **library**, not just the thing behind the HTTP binary. Its public entry —
//! [`LogicHost::run`] — knows nothing about HTTP, so you can drop it straight into a Tauri backend
//! and let your desktop app run **sandboxed, user-written JavaScript** (formulas, pricing rules,
//! automations) with exact-decimal money math and the `$std` value-utils — no server, no network.
//!
//! This example is the whole seam and nothing else. [`run_script`] below **is** the body you put
//! in a `#[tauri::command]`; `main()` just drives it so the example builds and runs on its own
//! (`cargo run -p tauri-embed`). The Tauri wiring — how to register the command, hold the host in
//! managed state, and call it from the webview — is shown in the `TAURI WIRING` block at the
//! bottom. Copy that into a real `tauri::Builder` app and you're done.
//!
//! Why this is a clean fit:
//! - **[`Profile::Deterministic`]** withholds every I/O capability and neutralizes nondeterminism
//!   (`Math.random`, `Date`) — the sandbox can only compute over the context you pass it. Exactly
//!   what you want for "let the user type a formula and run it" in a desktop app.
//! - **No network stack.** With `default-features = false` (see `Cargo.toml`) the crate links no
//!   `reqwest`/TLS — the value-utils are all pure Rust.
//! - **Build the host once.** [`LogicHost`] is `Send + Sync`; hold one in `tauri::State` and every
//!   command reuses its pre-warmed runtime pool. QuickJS is synchronous, so a command that runs a
//!   script should call [`run_script`] inside `tauri::async_runtime::spawn_blocking`.

use std::error::Error;
use std::sync::Arc;

use runlet_core::modules::ModuleRegistry;
use runlet_core::pool::JsPool;
use runlet_core::registry::ScriptRegistry;
use runlet_core::{EngineConfig, ExecOutcome, HostSettings, Invocation, LogicHost, Profile};

/// The sandboxed script the desktop user "wrote". It never touches the network — it just computes
/// an order total over the context, using the exact-decimal money global (`$`) so there are no
/// floating-point cents bugs. `handler(ctx)` returns the response via `json(data, error)`.
const USER_SCRIPT: &str = r#"
function handler(ctx) {
  // $(amount, currency) → exact decimal money. Method-based math (no operator overloading in JS).
  const subtotal = $(ctx.price, "USD").mul(ctx.qty);
  const total = subtotal.add_pct(ctx.tax_pct);   // add a percentage (e.g. sales tax)
  // For ctx { price:"100.00", qty:3, tax_pct:8.25 }: 100.00 × 3 = 300.00, +8.25% = 324.75.
  return json(
    {
      total: total.to_string(),   // "324.75"  (amount only)
      minor: total.to_minor(),    // 32475     (integer cents — safe to store/transmit)
      display: total.format()     // "$324.75" (currency-formatted for display)
    },
    null
  );
}
"#;

/// Build the host **once** and reuse it for every script run. In a Tauri app you'd do this in
/// `setup` and stash the returned host in `app.manage(host)` so commands pull it from
/// `tauri::State`. Nothing here is HTTP-specific.
fn build_host() -> Result<LogicHost, Box<dyn Error + Send + Sync>> {
    let config = EngineConfig::default();
    // The runtime pool (pre-warmed QuickJS runtimes, sized to CPU cores) and an empty script
    // registry — this example runs inline source, not registered keys.
    let pool = JsPool::new(config, Arc::new(ModuleRegistry::default()))?;
    let registry = Arc::new(ScriptRegistry::default());
    let settings = HostSettings {
        limits: config,
        // Deterministic scripts perform no egress, so this never comes into play; keep it strict.
        allow_private_targets: false,
    };

    // No `.capability(...)` calls: a deterministic desktop embed injects no I/O capabilities at all.
    LogicHost::builder(pool, registry, settings)
        .build()
        .map_err(|err| format!("failed to build host: {err}").into())
}

/// **This is your `#[tauri::command]`.** Given a host, the user's `script`, and a JSON `context`
/// string, run the script sandboxed and hand back the raw `{data, error, meta}` JSON envelope for
/// the webview to render. Errors are stringified so they cross the Tauri IPC boundary cleanly.
///
/// The `context` is opaque JSON passed straight to the sandbox as `ctx` — validate/serialize it on
/// the JS side of your app and forward the string here.
fn run_script(host: &LogicHost, script: &str, context: &str) -> Result<String, String> {
    let invocation = Invocation::inline(script, context)
        // Deterministic: no network, no clock, no randomness — pure compute over `ctx`.
        .profile(Profile::Deterministic);

    let outcome = host
        .run(invocation)
        .map_err(|err| format!("execution failed: {err:?}"))?;

    match outcome.result {
        // The success envelope is already JSON (`{"data":…,"error":null}`) — return it verbatim.
        ExecOutcome::Success(envelope) => Ok(envelope),
        ExecOutcome::Error(err) => Err(format!("handler error: {err:?}")),
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let host = build_host()?;

    // Stand in for the webview call: the user entered these numbers in the UI.
    let context = r#"{ "price": "100.00", "qty": 3, "tax_pct": 8.25 }"#;
    let envelope = run_script(&host, USER_SCRIPT, context)
        .map_err(|err| -> Box<dyn Error + Send + Sync> { err.into() })?;

    println!("script returned: {envelope}");

    // Prove the round-trip: 100.00 × 3 = 300.00, +8.25% = 324.75 → 32475 minor units.
    let parsed: serde_json::Value = serde_json::from_str(&envelope)?;
    let minor = parsed
        .get("data")
        .and_then(|data| data.get("minor"))
        .and_then(serde_json::Value::as_i64);
    if minor != Some(32475) {
        return Err(format!("expected minor == 32475, got {minor:?}").into());
    }
    println!("exact-decimal round-trip OK: total minor units == 32475");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
//  TAURI WIRING — copy this into a real `tauri` app (src-tauri/src/main.rs).
//  It's commented out so this example stays dependency-light and buildable without Tauri. The
//  only new dependency a real app adds is `tauri` itself; `runlet-core` is wired exactly as above.
// ─────────────────────────────────────────────────────────────────────────────────────────────
//
//  // Cargo.toml (src-tauri):
//  //   [dependencies]
//  //   tauri = { version = "2", features = [] }
//  //   runlet-core = { git = "https://github.com/hlop3z/runlet-js", default-features = false }
//
//  use runlet_core::LogicHost;
//  use tauri::{Manager, State};
//
//  /// The command the frontend calls. `State<LogicHost>` is the host built once in `setup`.
//  /// QuickJS is synchronous, so run it off the async runtime with `spawn_blocking`.
//  #[tauri::command]
//  async fn run_script_cmd(
//      host: State<'_, LogicHost>,
//      script: String,
//      context: String,
//  ) -> Result<String, String> {
//      let host = host.inner().clone(); // LogicHost is cheaply cloneable (Arc inside)
//      tauri::async_runtime::spawn_blocking(move || run_script(&host, &script, &context))
//          .await
//          .map_err(|join_err| format!("worker panicked: {join_err}"))?
//  }
//
//  fn main() {
//      tauri::Builder::default()
//          .setup(|app| {
//              app.manage(build_host()?); // build the host once, share it across commands
//              Ok(())
//          })
//          .invoke_handler(tauri::generate_handler![run_script_cmd])
//          .run(tauri::generate_context!())
//          .expect("error while running tauri application");
//  }
//
//  // Frontend (index.html / any framework):
//  //   import { invoke } from "@tauri-apps/api/core";
//  //   const script  = document.querySelector("#code").value;      // user-written JS
//  //   const context = JSON.stringify({ price: "100.00", qty: 3, tax_pct: 8.25 });
//  //   const envelope = await invoke("run_script_cmd", { script, context });
//  //   const { data, error } = JSON.parse(envelope);
//  //   render(error ?? data);
