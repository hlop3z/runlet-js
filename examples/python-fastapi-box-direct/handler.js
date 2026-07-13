// A script that reaches the FastAPI backend purely through logical names.
// It never sees a host, a port, or a credential — only the nicknames `cache` and `orders`,
// which the operator bound to the Python service in config.json.
//
// The request must list these names in `config.io` (see the curl in README.md), else the
// allowlist gate rejects the call with RESOURCE_NOT_FOUND before any egress happens.
function handler(ctx) {
  var name = ctx.name || "world";

  // set -> {ok:true}, then get -> {value:...}. Same {action, payload} envelope the box
  // POSTs to /cache; whatever the service returns as 2xx JSON is handed back verbatim.
  $std.io.call("cache", "set", { key: "greeting", value: "hello " + name });
  var cached = $std.io.call("cache", "get", { key: "greeting" });

  var order = $std.io.call("orders", "insert", { item: "widget", qty: 3 });

  return json({ cached: cached.value, order_id: order.id }, null);
}
