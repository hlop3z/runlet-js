//! Editor `.d.ts` type-surface assembly (D11).
//!
//! The editor-facing `container/types.d.ts` is machine-assembled from an always-on base fragment
//! plus each capability's own `.d.ts` fragment, so a third-party capability author composes against
//! it instead of forking a hand-maintained monolith. The registry is the single source of truth
//! for injection *and* `IntelliSense`; a golden test regenerates + diffs so the checked-in file can't
//! rot. Fragments share one TS namespace, so each prefixes its interface names by convention
//! (`Db`, `Mongo`, …) to keep autocomplete flat and collision-free.

use crate::capability::CapabilityDef;

/// The always-on base type surface: `json`/`Handler`/`Decimal`/`$`/`$sys`, the injectable
/// `hasura/client` module, and the `meta.io` response typing. First in the generated file.
pub const BASE_TYPES_DTS: &str = include_str!("js/base.d.ts");
/// The in-engine `http` capability's editor fragment (part of the enumerated mux-bypass surface).
pub const HTTP_TYPES_DTS: &str = include_str!("js/http.d.ts");
/// The in-engine `s3` capability's editor fragment.
pub const S3_TYPES_DTS: &str = include_str!("js/s3.d.ts");

/// Assembles a `types.d.ts` from the always-on base plus each provided capability fragment, the
/// base first and fragments joined by a blank line.
///
/// A composition passes the fragments of the in-engine capabilities it injects
/// ([`HTTP_TYPES_DTS`] / [`S3_TYPES_DTS`]) followed by each registered [`CapabilityDef`]'s
/// [`types`](CapabilityDef::types); the result is the file editors consume for autocomplete.
#[must_use]
pub fn generate_types_dts(fragments: &[&str]) -> String {
    let mut out = String::from(BASE_TYPES_DTS);
    for fragment in fragments {
        out.push('\n');
        out.push_str(fragment);
    }
    out
}

/// The `.d.ts` fragments for a set of registered defs, in registration order — a convenience for
/// callers assembling [`generate_types_dts`] input after the in-engine fragments.
#[must_use]
pub fn def_fragments(defs: &[CapabilityDef]) -> Vec<&str> {
    defs.iter().map(CapabilityDef::types).collect()
}

#[cfg(test)]
mod tests {
    //! D11 drift guard (relocated from `runlet-caps` on the byo-capabilities change): the checked-in
    //! `container/types.d.ts` equals what the stock **three-primitive** surface generates — the base
    //! fragment (which carries `json`/`$`/`Decimal`/`$sys`/`io`/`meta`) plus the in-engine `http` and
    //! `s3` fragments. No driver-capability defs ship, so there are no `Db`/`Mongo`/… interfaces.

    use super::{HTTP_TYPES_DTS, S3_TYPES_DTS, generate_types_dts};

    /// The checked-in `container/types.d.ts` is regenerable from base + `http` + `s3`. If this
    /// fails, regenerate the file — see `docs/design/composable-core.md`.
    #[test]
    fn types_dts_is_up_to_date() {
        let generated = generate_types_dts(&[HTTP_TYPES_DTS, S3_TYPES_DTS]);
        let checked_in = include_str!("../../../container/types.d.ts");
        assert_eq!(
            generated, checked_in,
            "container/types.d.ts is stale — regenerate from base + http + s3 fragments"
        );
    }
}
