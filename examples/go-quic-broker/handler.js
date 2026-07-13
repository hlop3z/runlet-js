// Identical script shape to the box-direct (FastAPI) example — the script cannot tell whether
// `cache` and `orders` resolve box-direct over loopback HTTP or across the network to this Go
// broker over QUIC. That indistinguishability is the whole design: box-direct and broker are
// the same `{action, payload}` wire.
function handler(ctx) {
  var name = ctx.name || "world";

  $std.io.call("cache", "set", { key: "greeting", value: "hello " + name });
  var cached = $std.io.call("cache", "get", { key: "greeting" });

  var order = $std.io.call("orders", "insert", { item: "widget", qty: 3 });

  return json({ cached: cached.value, order_item: order.item, order_by: order.by }, null);
}
