//! The [`Egress`] I/O port, re-exported from [`runlet_wire`].
//!
//! The trait and its tagged-error result now live in the shared `runlet-wire` crate so the
//! driver host (`fabric-backends`, eventually `fabricd`) can implement them without linking the
//! sandbox. The engine seam (`engine::inject_egress`) and the public surface (`crate::Egress`)
//! continue to reach them through this path. See `docs/design/resource-egress.md`.

pub use runlet_wire::egress::{Egress, EgressError};
