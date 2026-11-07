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
//! `error-context`, more than 32 flags, ...) is an [`Unsupported`](TranspileError::Unsupported) error naming
//! the type, never silently dropped. A type that no function uses is not checked.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use wasmparser::{
    ComponentAlias, ComponentDefinedType, ComponentExternalKind, ComponentFuncType,
    ComponentOuterAliasKind, ComponentType, ComponentTypeRef, ComponentValType, Encoding,
    InstanceTypeDeclaration, Parser, Payload, PrimitiveValType, TypeBounds, Validator,
    WasmFeatures,
};

use crate::TranspileError;
use crate::codegen::{assign_import_names, plain_ident};

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
}

/// A WIT variant: its name (as the interface exports it, kebab-case) and its cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitVariant {
    /// The variant's name, e.g. `stream-error`.
    pub name: String,
    /// The case names and their payload types (`None` for a case without payload), in order.
    pub cases: Vec<(String, Option<Arc<WitType>>)>,
}

/// A WIT enum: its name and its case names, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitEnum {
    /// The enum's name, e.g. `error-code`.
    pub name: String,
    /// The case names, in order.
    pub cases: Vec<String>,
}

/// A WIT flags type: its name and its flag names, in order (at most 32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitFlags {
    /// The flags type's name, e.g. `descriptor-flags`.
    pub name: String,
    /// The flag names, in order; flag `i` is bit `i`.
    pub flags: Vec<String>,
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

type Sig = (Vec<(String, WitType)>, Option<WitType>);

/// A reading step that failed: the name of the unsupported (or ill-formed) type. It
/// is reported only when a function uses the type.
type Soft<T> = Result<T, String>;

/// What a slot of a type index space holds.
#[derive(Clone)]
enum Def {
    Val(Soft<Arc<WitType>>),
    Func(Arc<Soft<Sig>>),
    Instance(Arc<Instance>),
    Resource(Arc<WitResource>),
    /// A type this reader does not model (a component type).
    Other(&'static str),
}

/// An instance type: its functions (an `Err` is an export that is not a plain function)
/// and its exported types, by name.
#[derive(Default)]
struct Instance {
    funcs: Vec<(String, Soft<Sig>)>,
    types: Vec<(String, Def)>,
    /// The resources this instance defines (`sub resource` exports), in order.
    resources: Vec<Arc<WitResource>>,
}

/// The type and instance index spaces of one component or instance type.
#[derive(Default)]
struct Scope {
    types: Vec<Def>,
    instances: Vec<Option<Arc<Instance>>>,
    /// Resources declared so far; only the outermost scope's count is used.
    resources_declared: u32,
}

/// A new resource named `name`, with an id no other resource of the component has.
fn new_resource(stack: &mut [Scope], name: &str) -> Result<Arc<WitResource>, TranspileError> {
    let outer = stack
        .first_mut()
        .ok_or_else(|| unsupported("component has no type index space"))?;
    let id = outer.resources_declared;
    outer.resources_declared = id
        .checked_add(1)
        .ok_or_else(|| unsupported("too many resources"))?;
    Ok(Arc::new(WitResource {
        name: name.to_string(),
        id,
    }))
}

fn innermost(stack: &[Scope]) -> Result<&Scope, TranspileError> {
    stack
        .last()
        .ok_or_else(|| unsupported("component has no type index space"))
}

fn innermost_mut(stack: &mut [Scope]) -> Result<&mut Scope, TranspileError> {
    stack
        .last_mut()
        .ok_or_else(|| unsupported("component has no type index space"))
}

fn unsupported(what: impl Into<String>) -> TranspileError {
    TranspileError::Unsupported(what.into())
}

fn primitive(p: PrimitiveValType) -> Soft<WitType> {
    Ok(match p {
        PrimitiveValType::Bool => WitType::Bool,
        PrimitiveValType::S8 => WitType::S8,
        PrimitiveValType::U8 => WitType::U8,
        PrimitiveValType::S16 => WitType::S16,
        PrimitiveValType::U16 => WitType::U16,
        PrimitiveValType::S32 => WitType::S32,
        PrimitiveValType::U32 => WitType::U32,
        PrimitiveValType::S64 => WitType::S64,
        PrimitiveValType::U64 => WitType::U64,
        PrimitiveValType::F32 => WitType::F32,
        PrimitiveValType::F64 => WitType::F64,
        PrimitiveValType::Char => WitType::Char,
        PrimitiveValType::String => WitType::String,
        PrimitiveValType::ErrorContext => return Err("error-context".into()),
    })
}

fn def_at(scope: &Scope, index: u32) -> Soft<&Def> {
    usize::try_from(index)
        .ok()
        .and_then(|i| scope.types.get(i))
        .ok_or_else(|| format!("type index {index} (out of range)"))
}

fn resource_at(scope: &Scope, index: u32) -> Soft<Arc<WitResource>> {
    match def_at(scope, index)? {
        Def::Resource(r) => Ok(r.clone()),
        _ => Err("own or borrow of a type that is not a resource".into()),
    }
}

fn val(scope: &Scope, v: ComponentValType) -> Soft<Arc<WitType>> {
    match v {
        ComponentValType::Primitive(p) => primitive(p).map(Arc::new),
        ComponentValType::Type(i) => match def_at(scope, i)? {
            Def::Val(v) => v.clone(),
            Def::Resource(_) => Err("resource (used as a value, not through own or borrow)".into()),
            Def::Other(what) => Err((*what).to_string()),
            Def::Func(_) | Def::Instance(_) => Err("a type that is not a value type".into()),
        },
    }
}

fn opt_val(scope: &Scope, v: Option<ComponentValType>) -> Soft<Option<Arc<WitType>>> {
    v.map(|v| val(scope, v)).transpose()
}

fn fields(scope: &Scope, fields: &[(&str, ComponentValType)]) -> Soft<Vec<(String, Arc<WitType>)>> {
    fields
        .iter()
        .map(|(name, ty)| Ok(((*name).to_string(), val(scope, *ty)?)))
        .collect()
}

fn defined(scope: &Scope, d: &ComponentDefinedType<'_>) -> Soft<WitType> {
    Ok(match d {
        ComponentDefinedType::Primitive(p) => primitive(*p)?,
        // Named when the interface exports the type (see `export_type`).
        ComponentDefinedType::Record(f) => WitType::Record(WitRecord {
            name: format!("record{}", scope.types.len()),
            fields: fields(scope, f)?,
        }),
        ComponentDefinedType::List(t) => WitType::List(val(scope, *t)?),
        ComponentDefinedType::Option(t) => WitType::Option(val(scope, *t)?),
        ComponentDefinedType::Tuple(ts) => WitType::Tuple(
            ts.iter()
                .map(|t| val(scope, *t))
                .collect::<Soft<Vec<_>>>()?,
        ),
        ComponentDefinedType::Result { ok, err } => WitType::Result {
            ok: opt_val(scope, *ok)?,
            err: opt_val(scope, *err)?,
        },
        ComponentDefinedType::Variant(cases) => WitType::Variant(WitVariant {
            name: format!("variant{}", scope.types.len()),
            cases: cases
                .iter()
                .map(|c| Ok((c.name.to_string(), opt_val(scope, c.ty)?)))
                .collect::<Soft<Vec<_>>>()?,
        }),
        ComponentDefinedType::Enum(cases) => WitType::Enum(WitEnum {
            name: format!("enum{}", scope.types.len()),
            cases: cases.iter().map(|c| (*c).to_string()).collect(),
        }),
        ComponentDefinedType::Flags(names) => {
            if names.len() > MAX_FLAGS {
                return Err(format!(
                    "flags with {} flags (at most {MAX_FLAGS} are supported)",
                    names.len()
                ));
            }
            WitType::Flags(WitFlags {
                name: format!("flags{}", scope.types.len()),
                flags: names.iter().map(|n| (*n).to_string()).collect(),
            })
        }
        ComponentDefinedType::Own(i) => WitType::Own(resource_at(scope, *i)?),
        ComponentDefinedType::Borrow(i) => WitType::Borrow(resource_at(scope, *i)?),
        ComponentDefinedType::Future(_) => return Err("future".into()),
        ComponentDefinedType::Stream(_) => return Err("stream".into()),
        ComponentDefinedType::Map(..) => return Err("map".into()),
        ComponentDefinedType::FixedLengthList(..) => return Err("fixed-length list".into()),
    })
}

/// The most flags one `flags` type may have: one `u32` of the canonical ABI.
const MAX_FLAGS: usize = 32;

fn func_sig(scope: &Scope, f: &ComponentFuncType<'_>) -> Soft<Sig> {
    if f.async_ {
        return Err("async function".into());
    }
    // One level of sharing is copied; the types below stay shared.
    let params = fields(scope, &f.params)?
        .into_iter()
        .map(|(name, ty)| (name, (*ty).clone()))
        .collect();
    let result = f.result.map(|r| val(scope, r)).transpose()?;
    Ok((params, result.map(|r| (*r).clone())))
}

/// The definition a type declaration makes, in the context of `stack` (innermost last).
fn component_type(stack: &mut Vec<Scope>, ty: &ComponentType<'_>) -> Result<Def, TranspileError> {
    Ok(match ty {
        ComponentType::Defined(d) => Def::Val(defined(innermost(stack)?, d).map(Arc::new)),
        ComponentType::Func(f) => Def::Func(Arc::new(func_sig(innermost(stack)?, f))),
        ComponentType::Resource { .. } => {
            let id = stack.first().map_or(0, |s| s.resources_declared);
            Def::Resource(new_resource(stack, &format!("resource{id}"))?)
        }
        ComponentType::Component(_) => Def::Other("component type"),
        ComponentType::Instance(decls) => Def::Instance(Arc::new(instance_type(stack, decls)?)),
    })
}

/// The definition an `eq`-bounded exported type gets: the same type, and a record,
/// variant, enum or flags type takes the exported name.
fn export_type(def: Def, name: &str) -> Def {
    if let Def::Val(Ok(ty)) = &def {
        let name = name.to_string();
        let renamed = match &**ty {
            WitType::Record(r) => WitType::Record(WitRecord {
                name,
                fields: r.fields.clone(),
            }),
            WitType::Variant(v) => WitType::Variant(WitVariant {
                name,
                cases: v.cases.clone(),
            }),
            WitType::Enum(e) => WitType::Enum(WitEnum {
                name,
                cases: e.cases.clone(),
            }),
            WitType::Flags(f) => WitType::Flags(WitFlags {
                name,
                flags: f.flags.clone(),
            }),
            _ => return def,
        };
        return Def::Val(Ok(Arc::new(renamed)));
    }
    def
}

/// What an alias adds to an index space.
enum AliasSlot {
    /// A type.
    Type(Def),
    /// An instance (its type is not tracked).
    Instance,
}

/// The type an alias of an exported type of `instance` (or an outer type) refers to.
fn alias_type(
    stack: &[Scope],
    from: &ComponentAlias<'_>,
) -> Result<Option<AliasSlot>, TranspileError> {
    let scope = innermost(stack)?;
    Ok(match from {
        ComponentAlias::InstanceExport {
            kind: ComponentExternalKind::Type,
            instance_index,
            name,
        } => {
            let inst = usize::try_from(*instance_index)
                .ok()
                .and_then(|i| scope.instances.get(i))
                .ok_or_else(|| unsupported(format!("instance index {instance_index}")))?;
            let def = inst
                .as_ref()
                .and_then(|i| i.types.iter().find(|(n, _)| n == name))
                .map_or(
                    Def::Other("an alias of a type of an unknown instance"),
                    |t| t.1.clone(),
                );
            Some(AliasSlot::Type(def))
        }
        ComponentAlias::InstanceExport {
            kind: ComponentExternalKind::Instance,
            ..
        } => Some(AliasSlot::Instance),
        ComponentAlias::Outer {
            kind: ComponentOuterAliasKind::Type,
            count,
            index,
        } => {
            let depth = usize::try_from(*count)
                .ok()
                .and_then(|c| stack.len().checked_sub(c.checked_add(1)?))
                .and_then(|d| stack.get(d))
                .ok_or_else(|| unsupported(format!("outer alias count {count}")))?;
            let def = def_at(depth, *index).map_err(unsupported)?.clone();
            Some(AliasSlot::Type(def))
        }
        _ => None,
    })
}

/// The types of one import of `declared`: every resource it defines is a new resource
/// (the declaration may be imported several times), and the types that mention it are
/// rewritten to match. Resources that came from outside keep their identity.
fn instantiate(
    stack: &mut [Scope],
    declared: &Arc<Instance>,
) -> Result<Arc<Instance>, TranspileError> {
    if declared.resources.is_empty() {
        return Ok(declared.clone());
    }
    let mut subst = Subst::default();
    let mut resources = Vec::new();
    for r in &declared.resources {
        let fresh = new_resource(stack, &r.name)?;
        subst.resources.insert(r.id, fresh.clone());
        resources.push(fresh);
    }
    let funcs = declared
        .funcs
        .iter()
        .map(|(n, sig)| (n.clone(), sig.clone().map(|(ps, r)| subst.sig(ps, r))))
        .collect();
    let types = declared
        .types
        .iter()
        .map(|(n, d)| (n.clone(), subst.def(d)))
        .collect();
    Ok(Arc::new(Instance {
        funcs,
        types,
        resources,
    }))
}

/// Replaces resources by id, keeping shared types shared (each `Arc` is rewritten once).
#[derive(Default)]
struct Subst {
    resources: HashMap<u32, Arc<WitResource>>,
    memo: HashMap<*const WitType, Arc<WitType>>,
}

impl Subst {
    fn resource(&self, r: &Arc<WitResource>) -> Arc<WitResource> {
        self.resources.get(&r.id).unwrap_or(r).clone()
    }

    fn arc(&mut self, t: &Arc<WitType>) -> Arc<WitType> {
        if let Some(done) = self.memo.get(&Arc::as_ptr(t)) {
            return done.clone();
        }
        let out = Arc::new(self.ty(t));
        self.memo.insert(Arc::as_ptr(t), out.clone());
        out
    }

    fn opt(&mut self, t: &Option<Arc<WitType>>) -> Option<Arc<WitType>> {
        t.as_ref().map(|t| self.arc(t))
    }

    fn ty(&mut self, t: &WitType) -> WitType {
        match t {
            WitType::List(t) => WitType::List(self.arc(t)),
            WitType::Option(t) => WitType::Option(self.arc(t)),
            WitType::Result { ok, err } => WitType::Result {
                ok: self.opt(ok),
                err: self.opt(err),
            },
            WitType::Tuple(ts) => WitType::Tuple(ts.iter().map(|t| self.arc(t)).collect()),
            WitType::Record(r) => WitType::Record(WitRecord {
                name: r.name.clone(),
                fields: r
                    .fields
                    .iter()
                    .map(|(n, t)| (n.clone(), self.arc(t)))
                    .collect(),
            }),
            WitType::Variant(v) => WitType::Variant(WitVariant {
                name: v.name.clone(),
                cases: v
                    .cases
                    .iter()
                    .map(|(n, t)| (n.clone(), self.opt(t)))
                    .collect(),
            }),
            WitType::Own(r) => WitType::Own(self.resource(r)),
            WitType::Borrow(r) => WitType::Borrow(self.resource(r)),
            other => other.clone(),
        }
    }

    fn sig(&mut self, params: Vec<(String, WitType)>, result: Option<WitType>) -> Sig {
        (
            params.into_iter().map(|(n, t)| (n, self.ty(&t))).collect(),
            result.map(|t| self.ty(&t)),
        )
    }

    fn def(&mut self, d: &Def) -> Def {
        match d {
            Def::Val(Ok(t)) => Def::Val(Ok(self.arc(t))),
            Def::Func(sig) => Def::Func(Arc::new(
                sig.as_ref().clone().map(|(ps, r)| self.sig(ps, r)),
            )),
            Def::Resource(r) => Def::Resource(self.resource(r)),
            other => other.clone(),
        }
    }
}

fn instance_type(
    stack: &mut Vec<Scope>,
    decls: &[InstanceTypeDeclaration<'_>],
) -> Result<Instance, TranspileError> {
    stack.push(Scope::default());
    let result = instance_decls(stack, decls);
    stack.pop();
    result
}

fn instance_decls(
    stack: &mut Vec<Scope>,
    decls: &[InstanceTypeDeclaration<'_>],
) -> Result<Instance, TranspileError> {
    let mut inst = Instance::default();
    for decl in decls {
        match decl {
            InstanceTypeDeclaration::CoreType(_) => {}
            InstanceTypeDeclaration::Type(ty) => {
                let def = component_type(stack, ty)?;
                push_type(stack, def)?;
            }
            InstanceTypeDeclaration::Alias(alias) => {
                if let Some(slot) = alias_type(stack, alias)? {
                    push_alias(stack, slot)?;
                }
            }
            InstanceTypeDeclaration::Export { name, ty } => {
                let name = name.full_name().into_owned();
                if let ComponentTypeRef::Type(TypeBounds::SubResource) = ty {
                    let r = new_resource(stack, &name)?;
                    inst.resources.push(r.clone());
                    let def = Def::Resource(r);
                    innermost_mut(stack)?.types.push(def.clone());
                    inst.types.push((name, def));
                    continue;
                }
                let scope = innermost_mut(stack)?;
                match ty {
                    ComponentTypeRef::Func(i) => {
                        let sig = match def_at(scope, *i) {
                            Ok(Def::Func(sig)) => (**sig).clone(),
                            Ok(_) => Err("a function export that is not a function type".into()),
                            Err(e) => Err(e),
                        };
                        inst.funcs.push((name, sig));
                    }
                    ComponentTypeRef::Type(bounds) => {
                        let def = match bounds {
                            TypeBounds::Eq(i) => match def_at(scope, *i) {
                                Ok(def) => export_type(def.clone(), &name),
                                Err(e) => Def::Val(Err(e)),
                            },
                            // Handled above.
                            TypeBounds::SubResource => Def::Other("a resource"),
                        };
                        scope.types.push(def.clone());
                        inst.types.push((name, def));
                    }
                    ComponentTypeRef::Instance(i) => {
                        let nested = match def_at(scope, *i) {
                            Ok(Def::Instance(n)) => Some(n.clone()),
                            _ => None,
                        };
                        scope.instances.push(nested);
                        inst.funcs.push((name, Err("a nested instance".into())));
                    }
                    ComponentTypeRef::Value(_) => inst.funcs.push((name, Err("a value".into()))),
                    ComponentTypeRef::Module(_) => {
                        inst.funcs.push((name, Err("a core module".into())));
                    }
                    ComponentTypeRef::Component(_) => {
                        inst.funcs.push((name, Err("a component".into())));
                    }
                }
            }
        }
    }
    Ok(inst)
}

fn push_type(stack: &mut [Scope], def: Def) -> Result<(), TranspileError> {
    innermost_mut(stack)?.types.push(def);
    Ok(())
}

fn push_instance(stack: &mut [Scope], inst: Option<Arc<Instance>>) -> Result<(), TranspileError> {
    innermost_mut(stack)?.instances.push(inst);
    Ok(())
}

fn push_alias(stack: &mut [Scope], slot: AliasSlot) -> Result<(), TranspileError> {
    match slot {
        AliasSlot::Type(def) => push_type(stack, def),
        AliasSlot::Instance => push_instance(stack, None),
    }
}

/// Every function of every imported interface instance of `component`, in import order.
///
/// An unsupported type used by a function (see the [module docs](self)) is an
/// [`Unsupported`](TranspileError::Unsupported) error naming the type and the function.
/// A core module, which has no interfaces, is an error too.
pub fn wit_functions(component: &[u8]) -> Result<Vec<WitFunc>, TranspileError> {
    // A binary that does not validate has no meaningful index spaces to read.
    Validator::new_with_features(WasmFeatures::all()).validate_all(component)?;
    let mut stack = vec![Scope::default()];
    let mut imports: Vec<(String, Arc<Instance>)> = Vec::new();
    let mut first = true;
    // The payloads of nested components and modules come inline; only the outermost
    // component's own imports are the host's.
    let mut depth = 0usize;
    for payload in Parser::new(0).parse_all(component) {
        let payload = payload?;
        if depth > 0 {
            match payload {
                Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => {
                    depth = depth.saturating_add(1);
                }
                Payload::End(_) => depth = depth.saturating_sub(1),
                _ => {}
            }
            continue;
        }
        match payload {
            Payload::Version { encoding, .. } if first => {
                first = false;
                if encoding == Encoding::Module {
                    return Err(unsupported(
                        "a core module has no component imports (the input is not a component)",
                    ));
                }
            }
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth = 1,
            Payload::ComponentTypeSection(reader) => {
                for ty in reader {
                    let def = component_type(&mut stack, &ty?)?;
                    push_type(&mut stack, def)?;
                }
            }
            Payload::ComponentAliasSection(reader) => {
                for alias in reader {
                    if let Some(slot) = alias_type(&stack, &alias?)? {
                        push_alias(&mut stack, slot)?;
                    }
                }
            }
            Payload::ComponentInstanceSection(reader) => {
                for instance in reader {
                    instance?;
                    push_instance(&mut stack, None)?;
                }
            }
            Payload::ComponentExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    let scope = innermost_mut(&mut stack)?;
                    let index = usize::try_from(export.index).ok();
                    match export.kind {
                        ComponentExternalKind::Type => {
                            let def = index
                                .and_then(|i| scope.types.get(i))
                                .cloned()
                                .unwrap_or(Def::Other("an export of an unknown type"));
                            scope.types.push(def);
                        }
                        ComponentExternalKind::Instance => {
                            let inst = index.and_then(|i| scope.instances.get(i)).cloned();
                            scope.instances.push(inst.flatten());
                        }
                        _ => {}
                    }
                }
            }
            Payload::ComponentImportSection(reader) => {
                for import in reader {
                    let import = import?;
                    let name = import.name.full_name().into_owned();
                    match import.ty {
                        ComponentTypeRef::Instance(i) => {
                            let Ok(Def::Instance(declared)) =
                                def_at(innermost(&stack)?, i).cloned()
                            else {
                                return Err(unsupported(format!(
                                    "import \"{name}\" has no instance type"
                                )));
                            };
                            // Each import makes its own resource types.
                            let inst = instantiate(&mut stack, &declared)?;
                            innermost_mut(&mut stack)?
                                .instances
                                .push(Some(inst.clone()));
                            imports.push((name, inst));
                        }
                        ComponentTypeRef::Type(TypeBounds::SubResource) => {
                            let r = new_resource(&mut stack, &name)?;
                            innermost_mut(&mut stack)?.types.push(Def::Resource(r));
                        }
                        ComponentTypeRef::Type(TypeBounds::Eq(i)) => {
                            let scope = innermost_mut(&mut stack)?;
                            let def = def_at(scope, i)
                                .cloned()
                                .unwrap_or_else(|e| Def::Val(Err(e)));
                            scope.types.push(def);
                        }
                        ComponentTypeRef::Func(_) => {
                            return Err(unsupported(format!(
                                "import \"{name}\" is a function, not an interface instance"
                            )));
                        }
                        ComponentTypeRef::Value(_) => {
                            return Err(unsupported(format!(
                                "import \"{name}\" is a value, not an interface instance"
                            )));
                        }
                        ComponentTypeRef::Module(_) | ComponentTypeRef::Component(_) => {
                            return Err(unsupported(format!(
                                "import \"{name}\" is a module or component, not an interface instance"
                            )));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if first {
        return Err(unsupported("the input is not a component"));
    }

    let mut funcs = Vec::new();
    for (interface, inst) in &imports {
        for (name, sig) in &inst.funcs {
            let (params, result) = sig.clone().map_err(|what| {
                unsupported(format!("{what} in function `{name}` of `{interface}`"))
            })?;
            funcs.push(WitFunc {
                interface: interface.clone(),
                name: name.clone(),
                params,
                result,
            });
        }
        // The core module imports `[resource-drop]<name>` from the interface that defines
        // the resource (`canon resource.drop` is in the component body, not its type).
        for r in &inst.resources {
            funcs.push(WitFunc {
                interface: interface.clone(),
                name: format!("[resource-drop]{}", r.name),
                params: vec![("handle".to_string(), WitType::Own(r.clone()))],
                result: None,
            });
        }
    }
    Ok(funcs)
}

/// How many type nodes rendering one trait may visit. A component can share a type
/// between many others, so a small component can expand to Rust text of exponential size;
/// past this the trait is refused instead.
const NODE_BUDGET: usize = 200_000;

/// A named compound type the trait declares once.
enum Decl {
    Record(WitRecord),
    Variant(WitVariant),
    Enum(WitEnum),
    Flags(WitFlags),
}

impl Decl {
    /// The WIT name of the declared type.
    fn name(&self) -> &str {
        match self {
            Decl::Record(r) => &r.name,
            Decl::Variant(v) => &v.name,
            Decl::Enum(e) => &e.name,
            Decl::Flags(f) => &f.name,
        }
    }
}

/// The records, variants, enums, flags and resources the trait's functions use, with the
/// Rust names they were given (one namespace).
#[derive(Default)]
struct Records {
    /// Resource ids and their Rust names.
    resource_names: HashMap<u32, String>,
    /// The resources in the order they must be emitted, with their Rust names.
    resources: Vec<String>,
    /// Each distinct declared type (by kind, name and rendered members) and its Rust name.
    names: HashMap<String, String>,
    /// The declared types in the order they must be emitted, with their Rust names.
    list: Vec<(Decl, String)>,
    /// Type nodes visited so far, against [`NODE_BUDGET`].
    visited: Cell<usize>,
}

/// `kebab-case` as a `CamelCase` Rust type name that is not one of the std names the
/// generated trait uses.
fn type_name(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        out.push_str("Record");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'T');
    }
    // The std names the generated code uses, and the prelude items a `struct Some(..)` or
    // `trait Default` would shadow in a module that also holds the user's `impl Host`.
    const RESERVED: [&str; 18] = [
        "Host",
        "String",
        "Vec",
        "Option",
        "Result",
        "Self",
        "Box",
        "Some",
        "None",
        "Ok",
        "Err",
        "Default",
        "Clone",
        "Copy",
        "Eq",
        "PartialEq",
        "Drop",
        "Send",
    ];
    if RESERVED.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

impl Records {
    fn visit(&self) -> Result<(), TranspileError> {
        let n = self.visited.get().saturating_add(1);
        self.visited.set(n);
        if n > NODE_BUDGET {
            return Err(unsupported(
                "type too large (it expands to more than 200000 type nodes)",
            ));
        }
        Ok(())
    }

    /// What identifies a declared type: its kind, name and members as rendered.
    fn key(&self, decl: &Decl) -> Result<String, TranspileError> {
        let mut key = String::new();
        match decl {
            Decl::Record(r) => {
                let _ = write!(key, "record|{}", r.name);
                for (name, ty) in &r.fields {
                    let _ = write!(key, "|{name}:{}", self.rust(ty)?);
                }
            }
            Decl::Variant(v) => {
                let _ = write!(key, "variant|{}", v.name);
                for (name, ty) in &v.cases {
                    let payload = match ty {
                        Some(t) => self.rust(t)?,
                        None => String::new(),
                    };
                    let _ = write!(key, "|{name}:{payload}");
                }
            }
            Decl::Enum(e) => {
                let _ = write!(key, "enum|{}", e.name);
                for name in &e.cases {
                    let _ = write!(key, "|{name}");
                }
            }
            Decl::Flags(f) => {
                let _ = write!(key, "flags|{}", f.name);
                for name in &f.flags {
                    let _ = write!(key, "|{name}");
                }
            }
        }
        Ok(key)
    }

    /// Registers `decl` (after the types its members contain), giving it a Rust name
    /// that no other, different declared type has.
    fn add(&mut self, decl: Decl) -> Result<(), TranspileError> {
        match &decl {
            Decl::Record(r) => r.fields.iter().try_for_each(|(_, ty)| self.add_in(ty))?,
            Decl::Variant(v) => v
                .cases
                .iter()
                .filter_map(|(_, t)| t.as_ref())
                .try_for_each(|ty| self.add_in(ty))?,
            Decl::Enum(_) | Decl::Flags(_) => {}
        }
        let key = self.key(&decl)?;
        if self.names.contains_key(&key) {
            return Ok(());
        }
        let rust_name = self.fresh_name(decl.name());
        self.names.insert(key, rust_name.clone());
        self.list.push((decl, rust_name));
        Ok(())
    }

    /// A Rust type name for `wit_name` that no declared type or resource has yet.
    fn fresh_name(&self, wit_name: &str) -> String {
        let base = type_name(wit_name);
        let mut name = base.clone();
        let mut n = 1usize;
        while self.list.iter().any(|(_, taken)| *taken == name) || self.resources.contains(&name) {
            n = n.saturating_add(1);
            name = format!("{base}{n}");
        }
        name
    }

    fn add_resource(&mut self, r: &Arc<WitResource>) {
        if !self.resource_names.contains_key(&r.id) {
            let name = self.fresh_name(&r.name);
            self.resource_names.insert(r.id, name.clone());
            self.resources.push(name);
        }
    }

    fn add_in(&mut self, ty: &WitType) -> Result<(), TranspileError> {
        self.visit()?;
        match ty {
            WitType::List(t) | WitType::Option(t) => self.add_in(t),
            WitType::Result { ok, err } => {
                for t in [ok, err].into_iter().flatten() {
                    self.add_in(t)?;
                }
                Ok(())
            }
            WitType::Tuple(ts) => ts.iter().try_for_each(|t| self.add_in(t)),
            WitType::Record(r) => self.add(Decl::Record(r.clone())),
            WitType::Variant(v) => self.add(Decl::Variant(v.clone())),
            WitType::Enum(e) => self.add(Decl::Enum(e.clone())),
            WitType::Flags(f) => self.add(Decl::Flags(f.clone())),
            WitType::Own(r) | WitType::Borrow(r) => {
                self.add_resource(r);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The Rust name of an already registered declared type.
    fn declared(&self, decl: Decl) -> Result<String, TranspileError> {
        Ok(match self.names.get(&self.key(&decl)?) {
            Some(name) => name.clone(),
            None => type_name(decl.name()),
        })
    }

    fn rust(&self, ty: &WitType) -> Result<String, TranspileError> {
        self.visit()?;
        let inner = |t: &Option<Arc<WitType>>| match t {
            Some(t) => self.rust(t),
            None => Ok("()".to_string()),
        };
        Ok(match ty {
            WitType::U8 => "u8".into(),
            WitType::U16 => "u16".into(),
            WitType::U32 => "u32".into(),
            WitType::U64 => "u64".into(),
            WitType::S8 => "i8".into(),
            WitType::S16 => "i16".into(),
            WitType::S32 => "i32".into(),
            WitType::S64 => "i64".into(),
            WitType::F32 => "f32".into(),
            WitType::F64 => "f64".into(),
            WitType::Bool => "bool".into(),
            WitType::Char => "char".into(),
            WitType::String => "String".into(),
            WitType::List(t) => format!("Vec<{}>", self.rust(t)?),
            WitType::Option(t) => format!("Option<{}>", self.rust(t)?),
            WitType::Result { ok, err } => format!("Result<{}, {}>", inner(ok)?, inner(err)?),
            WitType::Tuple(ts) => {
                let parts = ts
                    .iter()
                    .map(|t| self.rust(t))
                    .collect::<Result<Vec<_>, _>>()?;
                match parts.as_slice() {
                    [one] => format!("({one},)"),
                    many => format!("({})", many.join(", ")),
                }
            }
            WitType::Record(r) => self.declared(Decl::Record(r.clone()))?,
            WitType::Variant(v) => self.declared(Decl::Variant(v.clone()))?,
            WitType::Enum(e) => self.declared(Decl::Enum(e.clone()))?,
            WitType::Flags(f) => self.declared(Decl::Flags(f.clone()))?,
            WitType::Own(r) | WitType::Borrow(r) => match self.resource_names.get(&r.id) {
                Some(name) => name.clone(),
                None => type_name(&r.name),
            },
        })
    }
}

/// One identifier per name from `ident`, made unique by appending `_{n}` to a repeat.
fn dedup_idents<'a>(
    names: impl Iterator<Item = &'a str>,
    ident: impl Fn(&str) -> String,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, name) in names.enumerate() {
        let mut id = ident(name);
        while out.contains(&id) {
            id = format!("{id}_{i}");
        }
        out.push(id);
    }
    out
}

/// Field and parameter identifiers.
fn unique_idents<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    dedup_idents(names, plain_ident)
}

/// `kebab-case` as `CamelCase` enum case names that are valid, distinct and not `Self`.
fn case_idents<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    dedup_idents(names, |name| {
        let ident = type_name(name);
        if ident == "Record" && !name.eq_ignore_ascii_case("record") {
            "Case".to_string()
        } else {
            ident
        }
    })
}

/// `kebab-case` as `UPPER_SNAKE_CASE` associated constant names, distinct from each other.
fn flag_idents<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    dedup_idents(names, |name| {
        let words: Vec<String> = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_ascii_uppercase)
            .collect();
        let mut ident = if words.is_empty() {
            "FLAG".to_string()
        } else {
            words.join("_")
        };
        if ident.starts_with(|c: char| c.is_ascii_digit()) {
            ident.insert(0, 'F');
        }
        ident
    })
}

/// A flags type as `pub struct Name(pub u8|u16|u32)` (the canonical ABI's size: flag `i`
/// is bit `i`) with one associated constant per flag, `contains` and `union`.
fn write_flags(out: &mut String, name: &str, flags: &WitFlags) {
    let int = match flags.flags.len() {
        0..=8 => "u8",
        9..=16 => "u16",
        _ => "u32",
    };
    let _ = writeln!(
        out,
        "\n#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]\npub struct {name}(pub {int});\n\nimpl {name} {{"
    );
    for (i, ident) in flag_idents(flags.flags.iter().map(String::as_str))
        .iter()
        .enumerate()
    {
        let _ = writeln!(out, "    pub const {ident}: Self = Self(1 << {i});");
    }
    out.push_str(
        "    pub fn contains(self, other: Self) -> bool {\n        self.0 & other.0 == other.0\n    }\n\n    pub fn union(self, other: Self) -> Self {\n        Self(self.0 | other.0)\n    }\n}\n",
    );
}

/// Rust source for a `pub trait Host` with one method per function of every interface
/// the component imports, plus the types of the records, variants, enums and flags they use (`std` only).
///
/// A method `fn <name>(&mut self, <params>) -> <result>;` is named as the
/// [`named_imports`](crate::TranspileOptions::named_imports) method of the core import
/// with the interface as module and the function name as field, so a glue can map
/// one to the other. Types map as `string` to `String`, `list<T>` to `Vec<T>`,
/// `option<T>` to `Option<T>`, `result<T, E>` to `Result<T, E>` (an absent side is
/// `()`), `tuple` to a Rust tuple, `record` to a struct (named after the record, its
/// fields snake_case, emitted once however many functions use it) and the integers,
/// floats, `bool` and `char` to their Rust types.
///
/// A `variant` becomes a Rust `enum` (`Case(T)` for a case with a payload, `Case` without;
/// `#[derive(Debug, Clone, PartialEq)]`), an `enum` a fieldless Rust `enum` (also `Copy`, `Eq`),
/// and `flags` a `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub struct F(pub u8)`
/// (`u16` for 9 to 16 flags, `u32` for 17 to 32; more than 32 is an error) whose flag `i` is bit
/// `i`, with an associated constant per flag (`F::READ`) and `contains`/`union`. Case names are
/// `CamelCase`d; flag constants are `UPPER_SNAKE_CASE`d. Like records, each is emitted once.
///
/// A resource `r` becomes one `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct R(pub u32);`
/// handle (however many interfaces use it); `own<r>` and `borrow<r>` are both `R`, since the
/// host owns the handles. Its `[method]r.f`, `[static]r.f` and `[constructor]r` functions are
/// methods named like the core imports with those field names (a method's `self` parameter
/// is `self_`). The interface that defines the resource also gets
/// `fn <module>_resource_drop_r(&mut self, handle: R);`, named like the core import
/// `[resource-drop]r` that the lowered `resource.drop` becomes.
///
/// # Errors
///
/// A parse error, a core module or other non-component input, an import that is not an
/// interface instance, or a function using an unsupported type
/// ([`TranspileError::Unsupported`], naming the type, the function and the interface).
pub fn wit_host_trait(component: &[u8]) -> Result<String, TranspileError> {
    let funcs = wit_functions(component)?;
    let mut records = Records::default();
    for f in &funcs {
        for (_, ty) in &f.params {
            records.add_in(ty)?;
        }
        if let Some(ty) = &f.result {
            records.add_in(ty)?;
        }
    }
    let mut indexed = Vec::new();
    for (i, f) in funcs.iter().enumerate() {
        let index = u32::try_from(i).map_err(|_| unsupported("too many imported functions"))?;
        indexed.push((f.interface.as_str(), f.name.as_str(), index));
    }
    let names = assign_import_names(indexed);

    let mut out = String::from("// Generated by wasm2rs --wit-host.\n");
    for name in &records.resources {
        let _ = writeln!(
            out,
            "\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct {name}(pub u32);"
        );
    }
    for (decl, name) in &records.list {
        match decl {
            Decl::Record(record) => {
                let idents = unique_idents(record.fields.iter().map(|(n, _)| n.as_str()));
                let _ = writeln!(
                    out,
                    "\n#[derive(Debug, Clone, PartialEq)]\npub struct {name} {{"
                );
                for (ident, (_, ty)) in idents.iter().zip(&record.fields) {
                    let _ = writeln!(out, "    pub {ident}: {},", records.rust(ty)?);
                }
                out.push_str("}\n");
            }
            Decl::Variant(variant) => {
                let idents = case_idents(variant.cases.iter().map(|(n, _)| n.as_str()));
                let _ = writeln!(
                    out,
                    "\n#[derive(Debug, Clone, PartialEq)]\npub enum {name} {{"
                );
                for (ident, (_, ty)) in idents.iter().zip(&variant.cases) {
                    match ty {
                        Some(t) => {
                            let _ = writeln!(out, "    {ident}({}),", records.rust(t)?);
                        }
                        None => {
                            let _ = writeln!(out, "    {ident},");
                        }
                    }
                }
                out.push_str("}\n");
            }
            Decl::Enum(e) => {
                let idents = case_idents(e.cases.iter().map(String::as_str));
                let _ = writeln!(
                    out,
                    "\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum {name} {{"
                );
                for ident in &idents {
                    let _ = writeln!(out, "    {ident},");
                }
                out.push_str("}\n");
            }
            Decl::Flags(f) => write_flags(&mut out, name, f),
        }
    }
    out.push_str("\npub trait Host {\n");
    for (f, name) in funcs.iter().zip(&names) {
        // Debug-escaped: an interface or function name may hold any characters.
        let _ = writeln!(out, "    /// {:?} {:?}", f.interface, f.name);
        let idents = unique_idents(f.params.iter().map(|(n, _)| n.as_str()));
        let mut sig = format!("    fn {name}(&mut self");
        for (ident, (_, ty)) in idents.iter().zip(&f.params) {
            let _ = write!(sig, ", {ident}: {}", records.rust(ty)?);
        }
        sig.push(')');
        if let Some(ty) = &f.result {
            let _ = write!(sig, " -> {}", records.rust(ty)?);
        }
        let _ = writeln!(out, "{sig};");
    }
    out.push_str("}\n");
    Ok(out)
}
