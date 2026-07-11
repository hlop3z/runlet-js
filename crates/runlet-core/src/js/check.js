(function () {
  // The first-class `check` value-util at `$std.check` (beside `$std.text`/`$std.template`). A
  // checksum-VERIFICATION value: `$std.check(value)` wraps a value; each method answers one
  // question — "is this string's check digit internally consistent?" — and returns a boolean. It
  // covers only standards-anchored, decades-stable, registry-FREE algorithms: Luhn (ISO/IEC
  // 7812-1), the GS1 mod-10 GTIN check digit (ISO/IEC 15420), and the raw ISO/IEC 7064 check-
  // character standard (v1: the MOD 97-10 system). A method asserts a CONSISTENT check digit,
  // NEVER that the entity is real or registered — a checksum-valid card/IBAN can still map to no
  // account.
  //
  // Pure — no clock, no randomness, no ambient state, no `__sys` bridge, no Rust math (the whole
  // surface is small integer arithmetic over `Number`; MOD 97-10 uses a piecewise modulus so no
  // BigInt is needed) — so it is injected identically under every profile and the determinism
  // sanitizer removes nothing.
  //
  // Deliberate PERMANENT non-goals (do not add): branded registry/jurisdiction validators
  // (`iban`/`bic`/`vat`, national-ID tables) — they depend on living data that rots — and
  // publishing-only schemes (`isbn`/`issn`). An IBAN's checksum is reachable via the generic
  // `iso7064("mod_97_10")` primitive after the CALLER rearranges the IBAN; `iso7064` itself holds
  // no country-registry, length, or rearrangement logic.

  function Check(s) {
    this.value = s; // the wrapped plain string, verbatim
  }

  // The wrapped value as a string of ONLY decimal digits, or null if it holds any disallowed
  // character. `strip` names the characters tolerated as formatting (removed before the test);
  // pass "" to accept digits only.
  function digitsOnly(s, strip) {
    var out = "";
    for (var i = 0; i < s.length; i++) {
      var c = s.charAt(i);
      if (c >= "0" && c <= "9") {
        out += c;
      } else if (strip && strip.indexOf(c) !== -1) {
        continue; // tolerated formatting separator
      } else {
        return null; // any other character ⇒ not a valid digit string
      }
    }
    return out;
  }

  // ---- Luhn (ISO/IEC 7812-1 Annex B): credit/debit cards, IMEI ----------
  // Right-to-left: double every second digit, subtract 9 from any result > 9; a valid number's
  // total is a multiple of 10. Spaces and hyphens are tolerated as formatting.
  Check.prototype.luhn = function () {
    var d = digitsOnly(this.value, " -");
    if (d === null || d.length === 0) return false;
    var sum = 0;
    var doubleIt = false;
    for (var i = d.length - 1; i >= 0; i--) {
      var n = d.charCodeAt(i) - 48; // 48 === "0"
      if (doubleIt) {
        n *= 2;
        if (n > 9) n -= 9;
      }
      sum += n;
      doubleIt = !doubleIt;
    }
    return sum % 10 === 0;
  };

  // ---- GTIN (GS1 mod-10 / ISO/IEC 15420): UPC-A / EAN-13 / GTIN-8 / GTIN-14 ----
  // Strict digit string of length 8, 12, 13, or 14. The pre-check digits, weighted alternately by
  // 3 and 1 from the right (rightmost pre-check digit weight 3), sum with the check digit to a
  // multiple of 10.
  Check.prototype.gtin = function () {
    var d = digitsOnly(this.value, "");
    if (d === null) return false;
    var len = d.length;
    if (len !== 8 && len !== 12 && len !== 13 && len !== 14) return false;
    var sum = 0;
    for (var i = 0; i < len; i++) {
      var n = d.charCodeAt(i) - 48;
      var fromRight = len - 1 - i; // 0 === the check digit
      // Odd distance-from-check ⇒ weight 3; the check digit and even distances ⇒ weight 1.
      sum += fromRight % 2 === 1 ? n * 3 : n;
    }
    return sum % 10 === 0;
  };

  // Map one alphanumeric character to its ISO 7064 numeric value ("0".."9" → 0..9, "A".."Z"/
  // "a".."z" → 10..35), or -1 if it is outside the alphabet.
  function alnumValue(c) {
    if (c >= "0" && c <= "9") return c.charCodeAt(0) - 48;
    if (c >= "A" && c <= "Z") return c.charCodeAt(0) - 55; // 'A'(65) → 10
    if (c >= "a" && c <= "z") return c.charCodeAt(0) - 87; // 'a'(97) → 10
    return -1;
  }

  // MOD 97-10 (ISO/IEC 7064): value ≡ 1 (mod 97) over the alphanumeric-mapped decimal string.
  // Computed with a piecewise modulus so an arbitrarily long identifier never exceeds 2^53.
  function mod97_10(s) {
    if (s.length === 0) return false;
    var rem = 0;
    for (var i = 0; i < s.length; i++) {
      var v = alnumValue(s.charAt(i));
      if (v < 0) return false; // out-of-alphabet ⇒ not valid
      if (v < 10) {
        rem = (rem * 10 + v) % 97;
      } else {
        // A letter maps to two decimal digits (10..35); fold both.
        rem = (rem * 100 + v) % 97;
      }
    }
    return rem === 1;
  }

  // ---- ISO/IEC 7064 check-character systems -----------------------------
  // The standards-only primitive. Operates on the string AS GIVEN — for an IBAN the caller moves
  // the country + check characters to the end before calling; this method holds no IBAN/registry
  // logic. Unknown `system`, out-of-alphabet content, or empty input ⇒ false (never throws).
  Check.prototype.iso7064 = function (system) {
    if (system === "mod_97_10") return mod97_10(this.value);
    return false; // unknown system — the `system` argument is the extension point
  };

  // The factory: coerce to a string and wrap. Namespace-only — NOT added to `__stdExpose`, so
  // there is no bare `check` global (a script's own `check` local is unaffected).
  $std.check = function (input) {
    return new Check(String(input));
  };
})();
