(function () {
  // One native bridge for every $sys domain: __sys(domain, op, payloadJson).
  function call(domain, op, payload) {
    var raw = __sys(domain, op, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res.v;
  }

  // ---- $sys.secrets : opaque handles -------------------------------------
  // A secret's plaintext lives ONLY in Rust. JS holds a frozen handle carrying the
  // secret's name; every coercion yields "[secret:NAME]" (never the value), and the
  // handle is accepted solely as an HMAC key. SECRET is a closure-private symbol, so
  // a script can't read the name off the handle except via its own placeholder.
  var SECRET = Symbol("sys.secret");

  function makeSecret(name) {
    var label = "[secret:" + name + "]";
    var placeholder = function () { return label; };
    var h = {};
    Object.defineProperty(h, SECRET, { value: name });
    Object.defineProperty(h, "toString", { value: placeholder });
    Object.defineProperty(h, "toJSON", { value: placeholder });
    Object.defineProperty(h, "valueOf", { value: placeholder });
    Object.defineProperty(h, Symbol.toPrimitive, { value: placeholder });
    return Object.freeze(h);
  }

  // The secret name if `x` is a handle, else undefined.
  function secretName(x) {
    return x === null || typeof x !== "object" ? undefined : x[SECRET];
  }

  // Reject a secret handle anywhere it would be reversible (encode/hash/decode):
  // those echo their input, which would defeat use-not-extract. HMAC is the one
  // sink that may consume a secret, because its output is one-way.
  function denySecret(x, verb) {
    if (secretName(x) !== undefined) {
      throw new Error("a secret cannot be " + verb + "; use it only as an HMAC key");
    }
  }

  // Rust calls this with the configured secret names to populate $sys.secrets.
  Object.defineProperty(globalThis, "__sysMakeSecrets", {
    value: function (names) {
      var out = {};
      for (var i = 0; i < names.length; i++) out[names[i]] = makeSecret(names[i]);
      return Object.freeze(out);
    },
  });

  // ---- $sys.crypto -------------------------------------------------------
  function c(op, payload) { return call("crypto", op, payload); }

  function codec(encOp, decOp) {
    return {
      encode: function (s) { denySecret(s, "encoded"); return c(encOp, { data: String(s) }); },
      decode: function (s) { denySecret(s, "decoded"); return c(decOp, { data: String(s) }); },
    };
  }

  var crypto = {
    sha256: function (data) { denySecret(data, "hashed"); return c("sha256", { data: String(data) }); },
    sha512: function (data) { denySecret(data, "hashed"); return c("sha512", { data: String(data) }); },
    hmac: function (algo, key, msg, encoding) {
      denySecret(msg, "the HMAC message"); // a secret may be the key, never the message
      var name = secretName(key);
      var payload = { algo: String(algo), msg: String(msg), encoding: encoding || "hex" };
      // A handle sends its name (key_ref) so Rust resolves the plaintext; a plain
      // string key is sent as-is. Either way no plaintext crosses back out.
      if (name !== undefined) payload.key_ref = name;
      else payload.key = String(key);
      return c("hmac", payload);
    },
    uuid: function () { return c("uuid", {}); },
    base64: codec("base64_encode", "base64_decode"),
    base64url: codec("base64url_encode", "base64url_decode"),
    hex: codec("hex_encode", "hex_decode"),
    url: codec("url_encode", "url_decode"),
  };

  // NOTE: date/time is no longer under `$sys`. It is the first-class top-level `datetime`
  // value-util (see `js/datetime.js`), which rides the same `__sys("datetime", …)` bridge.

  globalThis.$sys = globalThis.$sys || {};
  $sys.crypto = crypto;
  // env/secrets default to empty; Rust overwrites them when config.sys is sent.
  // env holds plain returnable values; secrets become opaque handles (plaintext
  // stays in Rust) — returning one yields "[secret:NAME]", never the value.
  $sys.env = {};
  $sys.secrets = {};
})();
