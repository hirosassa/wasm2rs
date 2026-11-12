//! A typed host trait from the types of a component's imports.
//!
//! The component's type section, instance types, aliases and imports are read with
//! `wasmparser` (no code is generated from the core modules). Every function of every
//! imported interface instance becomes a [`WitFunc`] whose types are a [`WitType`], and
//! [`wit_host_trait`] renders them as a `pub trait Host` with one method per function,
//! named like the [`named_imports`](crate::TranspileOptions::named_imports) method of
//! the core import that lowers it (module = the interface, field = the function name).
//!
//! Supported types: integers, floats, `bool`, `char`, `string`, `list`, `option`,
//! `result`, `tuple`, `record`, `variant`, `enum`, `flags` (at most 32) and resource handles
//! (`own`/`borrow`). A function that uses anything else (`future`, `stream`,
//! `error-context`, more than 32 flags, ...) is an [`Unsupported`](crate::TranspileError::Unsupported) error naming
//! the type, never silently dropped. A type that no function uses is not checked.

use std::sync::Arc;

mod emit;
mod exports;
mod glue;
mod read;

pub(crate) use emit::NODE_BUDGET;
pub use emit::{
    WitNaming, WitOptions, wit_host_glue, wit_host_glue_with_options, wit_host_trait,
    wit_host_trait_with_options,
};
pub use exports::{WitExport, wit_exports};
pub use read::wit_functions;

pub(crate) use emit::{CoreImport, wit_host_glue_for_module};

/// A WIT value type that a host function can take or return.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs, reason = "the variants are the WIT type names")]
pub enum WitType {
    U8,
    U16,
    U32,
    U64,
    S8,
    S16,
    S32,
    S64,
    F32,
    F64,
    Bool,
    Char,
    String,
    List(Arc<WitType>),
    Option(Arc<WitType>),
    /// `result<ok, err>`; an absent side is `None`.
    Result {
        /// The success type.
        ok: Option<Arc<WitType>>,
        /// The error type.
        err: Option<Arc<WitType>>,
    },
    Tuple(Vec<Arc<WitType>>),
    Record(WitRecord),
    Variant(WitVariant),
    Enum(WitEnum),
    Flags(WitFlags),
    /// `own<R>`: a handle the callee takes over.
    Own(Arc<WitResource>),
    /// `borrow<R>`: a handle lent for the call.
    Borrow(Arc<WitResource>),
}

/// A WIT resource type. Two handles are of the same resource when their `id`s are equal,
/// however many interfaces re-export the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitResource {
    /// The resource's name as the interface that defines it exports it, e.g. `pollable`.
    pub name: String,
    /// Identifies the resource within one component.
    pub id: u32,
    /// The interface that defines the type, as imported (e.g. `wasi:io/error@0.2.0`), however
    /// many interfaces re-export it; `None` for a type no imported interface defines.
    pub interface: Option<String>,
}

/// A WIT record: its name (as the interface exports it, kebab-case) and its fields.
///
/// Types are shared (`Arc`) the way the component shares them, so a type that refers to
/// another one twice is not copied: decoding is linear in the size of the component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitRecord {
    /// The record's name, e.g. `file-info`.
    pub name: String,
    /// The field names and types, in order.
    pub fields: Vec<(String, Arc<WitType>)>,
    /// The interface that defines the type, as imported (e.g. `wasi:io/error@0.2.0`), however
    /// many interfaces re-export it; `None` for a type no imported interface defines.
    pub interface: Option<String>,
}

/// A WIT variant: its name (as the interface exports it, kebab-case) and its cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitVariant {
    /// The variant's name, e.g. `stream-error`.
    pub name: String,
    /// The case names and their payload types (`None` for a case without payload), in order.
    pub cases: Vec<(String, Option<Arc<WitType>>)>,
    /// The interface that defines the type, as imported (e.g. `wasi:io/error@0.2.0`), however
    /// many interfaces re-export it; `None` for a type no imported interface defines.
    pub interface: Option<String>,
}

/// A WIT enum: its name and its case names, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitEnum {
    /// The enum's name, e.g. `error-code`.
    pub name: String,
    /// The case names, in order.
    pub cases: Vec<String>,
    /// The interface that defines the type, as imported (e.g. `wasi:io/error@0.2.0`), however
    /// many interfaces re-export it; `None` for a type no imported interface defines.
    pub interface: Option<String>,
}

/// A WIT flags type: its name and its flag names, in order (at most 32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitFlags {
    /// The flags type's name, e.g. `descriptor-flags`.
    pub name: String,
    /// The flag names, in order; flag `i` is bit `i`.
    pub flags: Vec<String>,
    /// The interface that defines the type, as imported (e.g. `wasi:io/error@0.2.0`), however
    /// many interfaces re-export it; `None` for a type no imported interface defines.
    pub interface: Option<String>,
}

/// A function of an imported interface instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitFunc {
    /// The interface name as imported, e.g. `wasi:cli/environment@0.2.0`.
    pub interface: String,
    /// The function name, e.g. `get-arguments`.
    pub name: String,
    /// The parameter names and types, in order.
    pub params: Vec<(String, WitType)>,
    /// The result type, if the function returns a value.
    pub result: Option<WitType>,
}
