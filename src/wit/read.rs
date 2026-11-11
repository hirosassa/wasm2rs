//! Reading a component's type section into [`WitFunc`]s: type, instance and alias
//! declarations with `wasmparser`, resolved against nested scopes.

use std::collections::HashMap;
use std::sync::Arc;

use wasmparser::{
    CanonicalFunction, ComponentAlias, ComponentDefinedType, ComponentExternalKind,
    ComponentFuncType, ComponentOuterAliasKind, ComponentType, ComponentTypeRef, ComponentValType,
    Encoding, InstanceTypeDeclaration, Parser, Payload, PrimitiveValType, TypeBounds, Validator,
    WasmFeatures,
};

use super::{WitEnum, WitFlags, WitFunc, WitRecord, WitResource, WitType, WitVariant};
use crate::TranspileError;

pub(super) type Sig = (Vec<(String, WitType)>, Option<WitType>);

/// A reading step that failed: the name of the unsupported (or ill-formed) type. It
/// is reported only when a function uses the type.
pub(super) type Soft<T> = Result<T, UnsupportedType>;

/// The name of a type a function cannot use (or why a type is ill-formed). Kept apart
/// from [`TranspileError`] because it only becomes one if a function uses the type.
#[derive(Debug, Clone)]
pub(super) struct UnsupportedType(String);

impl From<&str> for UnsupportedType {
    fn from(what: &str) -> Self {
        Self(what.to_owned())
    }
}

impl From<String> for UnsupportedType {
    fn from(what: String) -> Self {
        Self(what)
    }
}

impl std::fmt::Display for UnsupportedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<UnsupportedType> for TranspileError {
    fn from(what: UnsupportedType) -> Self {
        TranspileError::Unsupported(what.0)
    }
}

/// What a slot of a type index space holds.
#[derive(Clone)]
pub(super) enum Def {
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
pub(super) struct Instance {
    funcs: Vec<(String, Soft<Sig>)>,
    types: Vec<(String, Def)>,
    /// The resources this instance defines (`sub resource` exports), in order.
    resources: Vec<Arc<WitResource>>,
}

/// The type and instance index spaces of one component or instance type.
#[derive(Default)]
pub(super) struct Scope {
    types: Vec<Def>,
    instances: Vec<Option<Arc<Instance>>>,
    /// Resources declared so far; only the outermost scope's count is used.
    resources_declared: u32,
}

/// A new resource named `name`, with an id no other resource of the component has.
pub(super) fn new_resource(
    stack: &mut [Scope],
    name: &str,
) -> Result<Arc<WitResource>, TranspileError> {
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
        interface: None,
    }))
}

pub(super) fn innermost(stack: &[Scope]) -> Result<&Scope, TranspileError> {
    stack
        .last()
        .ok_or_else(|| unsupported("component has no type index space"))
}

pub(super) fn innermost_mut(stack: &mut [Scope]) -> Result<&mut Scope, TranspileError> {
    stack
        .last_mut()
        .ok_or_else(|| unsupported("component has no type index space"))
}

pub(super) fn unsupported(what: impl Into<String>) -> TranspileError {
    TranspileError::Unsupported(what.into())
}

pub(super) fn primitive(p: PrimitiveValType) -> Soft<WitType> {
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

pub(super) fn def_at(scope: &Scope, index: u32) -> Soft<&Def> {
    usize::try_from(index)
        .ok()
        .and_then(|i| scope.types.get(i))
        .ok_or_else(|| format!("type index {index} (out of range)").into())
}

pub(super) fn resource_at(scope: &Scope, index: u32) -> Soft<Arc<WitResource>> {
    match def_at(scope, index)? {
        Def::Resource(r) => Ok(r.clone()),
        _ => Err("own or borrow of a type that is not a resource".into()),
    }
}

pub(super) fn val(scope: &Scope, v: ComponentValType) -> Soft<Arc<WitType>> {
    match v {
        ComponentValType::Primitive(p) => primitive(p).map(Arc::new),
        ComponentValType::Type(i) => match def_at(scope, i)? {
            Def::Val(v) => v.clone(),
            Def::Resource(_) => Err("resource (used as a value, not through own or borrow)".into()),
            Def::Other(what) => Err((*what).into()),
            Def::Func(_) | Def::Instance(_) => Err("a type that is not a value type".into()),
        },
    }
}

pub(super) fn opt_val(scope: &Scope, v: Option<ComponentValType>) -> Soft<Option<Arc<WitType>>> {
    v.map(|v| val(scope, v)).transpose()
}

pub(super) fn fields(
    scope: &Scope,
    fields: &[(&str, ComponentValType)],
) -> Soft<Vec<(String, Arc<WitType>)>> {
    fields
        .iter()
        .map(|(name, ty)| Ok(((*name).to_string(), val(scope, *ty)?)))
        .collect()
}

pub(super) fn defined(scope: &Scope, d: &ComponentDefinedType<'_>) -> Soft<WitType> {
    Ok(match d {
        ComponentDefinedType::Primitive(p) => primitive(*p)?,
        // Named when the interface exports the type (see `export_type`).
        ComponentDefinedType::Record(f) => WitType::Record(WitRecord {
            name: format!("record{}", scope.types.len()),
            fields: fields(scope, f)?,
            interface: None,
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
            interface: None,
        }),
        ComponentDefinedType::Enum(cases) => WitType::Enum(WitEnum {
            name: format!("enum{}", scope.types.len()),
            cases: cases.iter().map(|c| (*c).to_string()).collect(),
            interface: None,
        }),
        ComponentDefinedType::Flags(names) => {
            if names.len() > MAX_FLAGS {
                return Err(format!(
                    "flags with {} flags (at most {MAX_FLAGS} are supported)",
                    names.len()
                )
                .into());
            }
            WitType::Flags(WitFlags {
                name: format!("flags{}", scope.types.len()),
                flags: names.iter().map(|n| (*n).to_string()).collect(),
                interface: None,
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
pub(super) const MAX_FLAGS: usize = 32;

pub(super) fn func_sig(scope: &Scope, f: &ComponentFuncType<'_>) -> Soft<Sig> {
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
pub(super) fn component_type(
    stack: &mut Vec<Scope>,
    ty: &ComponentType<'_>,
) -> Result<Def, TranspileError> {
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
pub(super) fn export_type(def: Def, name: &str) -> Def {
    if let Def::Val(Ok(ty)) = &def {
        let name = name.to_string();
        let renamed = match &**ty {
            WitType::Record(r) => WitType::Record(WitRecord {
                name,
                fields: r.fields.clone(),
                interface: r.interface.clone(),
            }),
            WitType::Variant(v) => WitType::Variant(WitVariant {
                name,
                cases: v.cases.clone(),
                interface: v.interface.clone(),
            }),
            WitType::Enum(e) => WitType::Enum(WitEnum {
                name,
                cases: e.cases.clone(),
                interface: e.interface.clone(),
            }),
            WitType::Flags(f) => WitType::Flags(WitFlags {
                name,
                flags: f.flags.clone(),
                interface: f.interface.clone(),
            }),
            _ => return def,
        };
        return Def::Val(Ok(Arc::new(renamed)));
    }
    def
}

/// What an alias adds to an index space.
pub(super) enum AliasSlot {
    /// A type.
    Type(Def),
    /// An instance (its type is not tracked).
    Instance,
}

/// The type an alias of an exported type of `instance` (or an outer type) refers to.
pub(super) fn alias_type(
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
            let def = def_at(depth, *index).map_err(TranspileError::from)?.clone();
            Some(AliasSlot::Type(def))
        }
        _ => None,
    })
}

/// The types of one import of `declared`: every resource it defines is a new resource
/// (the declaration may be imported several times), and the types that mention it are
/// rewritten to match. Resources that came from outside keep their identity. Every type the
/// declaration defines (one that no earlier import defined) is recorded as defined by
/// `interface`, the name of this import.
pub(super) fn instantiate(
    stack: &mut [Scope],
    declared: &Arc<Instance>,
    interface: &str,
) -> Result<Arc<Instance>, TranspileError> {
    let mut subst = Subst {
        interface: interface.to_string(),
        ..Subst::default()
    };
    let mut resources = Vec::new();
    for r in &declared.resources {
        let fresh = new_resource(stack, &r.name)?;
        let fresh = Arc::new(WitResource {
            interface: Some(interface.to_string()),
            ..(*fresh).clone()
        });
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
pub(super) struct Subst {
    /// The interface an unattributed type is defined by.
    interface: String,
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

    /// The defining interface of a type: the one it has, else this import's.
    fn definer(&self, interface: &Option<String>) -> Option<String> {
        interface.clone().or_else(|| Some(self.interface.clone()))
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
                interface: self.definer(&r.interface),
            }),
            WitType::Variant(v) => WitType::Variant(WitVariant {
                name: v.name.clone(),
                cases: v
                    .cases
                    .iter()
                    .map(|(n, t)| (n.clone(), self.opt(t)))
                    .collect(),
                interface: self.definer(&v.interface),
            }),
            WitType::Enum(e) => WitType::Enum(WitEnum {
                interface: self.definer(&e.interface),
                ..e.clone()
            }),
            WitType::Flags(f) => WitType::Flags(WitFlags {
                interface: self.definer(&f.interface),
                ..f.clone()
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

pub(super) fn instance_type(
    stack: &mut Vec<Scope>,
    decls: &[InstanceTypeDeclaration<'_>],
) -> Result<Instance, TranspileError> {
    stack.push(Scope::default());
    let result = instance_decls(stack, decls);
    stack.pop();
    result
}

pub(super) fn instance_decls(
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

pub(super) fn push_type(stack: &mut [Scope], def: Def) -> Result<(), TranspileError> {
    innermost_mut(stack)?.types.push(def);
    Ok(())
}

pub(super) fn push_instance(
    stack: &mut [Scope],
    inst: Option<Arc<Instance>>,
) -> Result<(), TranspileError> {
    innermost_mut(stack)?.instances.push(inst);
    Ok(())
}

pub(super) fn push_alias(stack: &mut [Scope], slot: AliasSlot) -> Result<(), TranspileError> {
    match slot {
        AliasSlot::Type(def) => push_type(stack, def),
        AliasSlot::Instance => push_instance(stack, None),
    }
}

/// What `read_component` found in the component body.
pub(super) struct Read {
    imports: Vec<(String, Arc<Instance>)>,
    /// The resources whose `canon resource.drop` the component body lowers.
    dropped: Vec<u32>,
    /// Whether the component has a core module at the top level.
    has_core: bool,
    /// Whether some drop could not be tied to a resource (or a nested component lowers some).
    unmapped_drop: bool,
}

/// Walks the payloads of `component`, building its type and instance index spaces and
/// collecting its imports. `exports`, if any, sees every payload first.
pub(super) fn read_component(
    component: &[u8],
    mut exports: Option<&mut super::exports::Reader>,
) -> Result<Read, TranspileError> {
    // A binary that does not validate has no meaningful index spaces to read.
    Validator::new_with_features(WasmFeatures::all()).validate_all(component)?;
    let mut stack = vec![Scope::default()];
    let mut imports: Vec<(String, Arc<Instance>)> = Vec::new();
    let mut first = true;
    let mut dropped: Vec<u32> = Vec::new();
    let mut has_core = false;
    let mut unmapped_drop = false;
    // The payloads of nested components and modules come inline; only the outermost
    // component's own imports are the host's.
    let mut depth = 0usize;
    for payload in Parser::new(0).parse_all(component) {
        let payload = payload?;
        if let Some(exports) = exports.as_deref_mut() {
            exports.feed(&payload, &stack)?;
        }
        if depth > 0 {
            match payload {
                // A nested component's own `canon resource.drop`s are not mapped to
                // resources (its index spaces are not read), so any drop may be lowered there.
                Payload::ComponentCanonicalSection(reader) => {
                    for func in reader {
                        if matches!(func?, CanonicalFunction::ResourceDrop { .. }) {
                            unmapped_drop = true;
                        }
                    }
                }
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
            Payload::ModuleSection { .. } => {
                has_core = true;
                depth = 1;
            }
            Payload::ComponentSection { .. } => depth = 1,
            Payload::ComponentCanonicalSection(reader) => {
                for func in reader {
                    if let CanonicalFunction::ResourceDrop { resource } = func? {
                        match def_at(innermost(&stack)?, resource) {
                            Ok(Def::Resource(r)) => dropped.push(r.id),
                            _ => unmapped_drop = true,
                        }
                    }
                }
            }
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
                            let inst = instantiate(&mut stack, &declared, &name)?;
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
    Ok(Read {
        imports,
        dropped,
        has_core,
        unmapped_drop,
    })
}

/// Every function of every imported interface instance of `component`, in import order.
///
/// An unsupported type used by a function (see the module docs) is an
/// [`Unsupported`](TranspileError::Unsupported) error naming the type and the function.
/// A core module, which has no interfaces, is an error too.
pub fn wit_functions(component: &[u8]) -> Result<Vec<WitFunc>, TranspileError> {
    let Read {
        imports,
        dropped,
        has_core,
        unmapped_drop,
    } = read_component(component, None)?;
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
        // the resource, but only if the component body lowers a `canon resource.drop` for it
        // (it is in the body, not in the type): a program that never drops e.g. `network`
        // has no such import. A component without a top-level core module (just its types),
        // with a nested component that lowers a drop, or with a drop that cannot be tied to a resource, keeps
        // every drop.
        for r in &inst.resources {
            if has_core && !unmapped_drop && !dropped.contains(&r.id) {
                continue;
            }
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
