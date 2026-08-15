use wasmparser::ValType;

use super::super::{
    ALLOW, FLATTEN_DEPTH_THRESHOLD, GenMeta, Node, SplitPlan, Val, can_flatten, default_value,
    estimate_body_len, flatten_body, index_u32, push_body_line, render_body_into, rust_type,
    rust_types,
};
use super::CatchPhase;
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    // ----- calls -----------------------------------------------------------

    /// Join call arguments into a comma-separated list, cloning any argument
    /// that lowers to the non-`Copy` `GcRef` (see [`Self::move_val`]).
    fn join_call_args(&self, args: Vec<Val>) -> Result<String, TranspileError> {
        let mut parts = Vec::with_capacity(args.len());
        for a in args {
            parts.push(self.move_val(&a)?);
        }
        Ok(parts.join(", "))
    }

    pub(super) fn call(&mut self, function_index: u32) -> Result<(), TranspileError> {
        self.emit_direct_call(function_index, false)
    }

    /// `return_call`: a tail call. The callee is invoked in tail position and
    /// its result(s) become this function's result, so — unlike a plain `call`
    /// — the operand stack below the arguments is discarded (control does not
    /// return here) and the call is emitted as a `return`. This lowers to an
    /// ordinary Rust call in tail position: exact in semantics, but without a
    /// constant-stack guarantee (Rust has no guaranteed TCO), matching wasm2c's
    /// default lowering.
    pub(super) fn return_call(&mut self, function_index: u32) -> Result<(), TranspileError> {
        self.emit_direct_call(function_index, true)?;
        self.emit_return()
    }

    /// Shared lowering for `call` and `return_call`. Consumes the arguments,
    /// binds the call result(s) to stable temporaries, and pushes them; the
    /// tail form leaves the caller to turn those temporaries into a `return`.
    fn emit_direct_call(&mut self, function_index: u32, tail: bool) -> Result<(), TranspileError> {
        let (params, results) = self
            .ctx
            .full_sig(function_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call to unknown function".into()))?;
        let param_count = params.len();
        let results = results.to_vec();

        // A call may read and write memory and globals. The arguments are
        // consumed here (evaluated in push order at the call site), so inline
        // them; freeze only the survivors below, pinning any earlier value that
        // must not observe the call's side effects (spill-before-mutation). A
        // tail call discards those survivors (it returns immediately), so they
        // need no freezing.
        if !tail {
            self.freeze_survivors(param_count)?;
        }

        let mut args = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = self.join_call_args(args)?;

        // A call is not re-evaluatable, so bind its result(s) to a temporary at
        // exactly this point (mirroring `memory_grow`) and push the stable
        // temporaries. A multi-value result is destructured from a tuple. For a
        // tail call, `emit_return` then turns those temporaries straight into
        // the function's `return`.
        let call_expr = self.ctx.call_expr(function_index as usize, &arg_list);
        let (prefix, temps) = self.result_binding(&results)?;
        self.line(format!("{prefix}{call_expr};"));
        self.push_temps(temps);
        Ok(())
    }

    pub(super) fn call_indirect(
        &mut self,
        type_index: u32,
        table_index: u32,
    ) -> Result<(), TranspileError> {
        self.emit_indirect_call(type_index, table_index, false)
    }

    /// `return_call_indirect`: the tail-call form of `call_indirect`. See
    /// [`Self::return_call`] for the tail-call semantics and their (absent)
    /// constant-stack guarantee.
    pub(super) fn return_call_indirect(
        &mut self,
        type_index: u32,
        table_index: u32,
    ) -> Result<(), TranspileError> {
        self.emit_indirect_call(type_index, table_index, true)
    }

    fn emit_indirect_call(
        &mut self,
        type_index: u32,
        table_index: u32,
        tail: bool,
    ) -> Result<(), TranspileError> {
        if !self.ctx.has_table {
            return Err(TranspileError::Unsupported(
                "call_indirect without a table".into(),
            ));
        }
        if table_index != 0 {
            return Err(TranspileError::Unsupported(
                "call_indirect on a non-zero table".into(),
            ));
        }
        let sig = self
            .ctx
            .types
            .get(type_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call_indirect: unknown type".into()))?;
        let results = sig.results.clone();
        // The functions any table entry could resolve to: exactly those whose
        // signature equals the declared type (no subtyping, so a structural
        // match is a type match). This spans the whole index space, so a table
        // entry may resolve to an imported function.
        let want = Some((sig.params.as_slice(), sig.results.as_slice()));
        let targets: Vec<usize> = (0..self.ctx.func_count())
            .filter(|&fidx| self.ctx.full_sig(fidx) == want)
            .collect();

        // The table index (top of stack) is consumed into the `entry` binding
        // below, which is emitted *before* the dispatch call — so only the index
        // may inline. The arguments must stay spilled to temporaries so they are
        // evaluated in wasm order (arguments before the index) rather than being
        // pulled after the `entry` computation. Spilling also pins any earlier
        // value against the callee's side effects (spill-before-mutation). A
        // tail call discards the survivors below the arguments, and its operands
        // are pure reads with nothing mutating between them (the only work in
        // between is the pure table read for `entry`), so nothing needs freezing.
        if !tail {
            self.freeze_survivors(1)?;
        }
        let index = self.pop()?;
        let mut args = Vec::with_capacity(sig.params.len());
        for _ in 0..sig.params.len() {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = self.join_call_args(args)?;

        // No local function has the requested type. Without a host, every entry
        // mismatches, so the call always traps and control cannot continue past
        // it. With a host, a table entry may still be an external handle the
        // host resolves, so fall through to the shared dispatch method (which
        // has only the null/external/catch-all arms).
        if targets.is_empty() && !self.ctx.has_imports {
            self.term("panic!(\"indirect call type mismatch\");".to_string());
            self.reachable = false;
            self.dead_nesting = 0;
            return Ok(());
        }

        // Read the entry into a local first: for an imported table `table()`
        // borrows the whole instance (the host), and passing the entry straight
        // into the dispatch call would keep that borrow live across the `&mut
        // self` method call. Copying out the `u32` releases it. An out-of-bounds
        // index panics on this slice access (a trap).
        let entry = self.fresh_temp();
        self.line(format!(
            "let {entry} = self.table()[({}) as u32 as usize];",
            index.code
        ));

        // Dispatch through the shared `call_ref_t{ti}` method (see
        // `dispatch_method_lines`): it holds the `match` on the funcref and is
        // also the public entry point the host uses to invoke a funcref the
        // module handed out. A null or wrong-type entry traps inside it.
        self.dispatch_sigs.insert(type_index);
        let (prefix, temps) = self.result_binding(&results)?;
        let sep = if arg_list.is_empty() { "" } else { ", " };
        self.line(format!(
            "{prefix}self.call_ref_t{type_index}({entry}{sep}{arg_list});"
        ));
        self.push_temps(temps);
        if tail {
            self.emit_return()?;
        }
        Ok(())
    }

    pub(super) fn call_ref(&mut self, type_index: u32) -> Result<(), TranspileError> {
        self.emit_call_ref(type_index, false)
    }

    /// `return_call_ref`: the tail-call form of `call_ref`. See
    /// [`Self::return_call`] for the tail-call semantics and their (absent)
    /// constant-stack guarantee.
    pub(super) fn return_call_ref(&mut self, type_index: u32) -> Result<(), TranspileError> {
        self.emit_call_ref(type_index, true)
    }

    /// Shared lowering for `call_ref` and `return_call_ref`. The funcref sits on
    /// top of the operand stack with the arguments below it (`[args.., funcref]`),
    /// so this mirrors [`Self::emit_indirect_call`] but takes the funcref straight
    /// off the stack instead of a table slot. A null or wrong-type funcref traps
    /// inside the generated `call_ref_t{ti}` dispatch method.
    fn emit_call_ref(&mut self, type_index: u32, tail: bool) -> Result<(), TranspileError> {
        let sig = self
            .ctx
            .types
            .get(type_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call_ref: unknown type".into()))?;
        let results = sig.results.clone();
        let param_count = sig.params.len();

        // The funcref (top of stack) is consumed into the `entry` binding below,
        // emitted *before* the dispatch call — so only it may inline. The
        // arguments must stay spilled to temporaries so they are evaluated in
        // wasm order (arguments before the funcref) and pinned against the
        // callee's side effects (spill-before-mutation). A tail call discards the
        // survivors below the arguments, and its operands are pure reads, so
        // nothing needs freezing.
        if !tail {
            self.freeze_survivors(1)?;
        }
        let funcref = self.pop()?;
        let mut args = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = self.join_call_args(args)?;

        // Bind the funcref to a local first for borrow-safety: passing the
        // expression straight into the `&mut self` dispatch call could keep an
        // earlier borrow live across it. Copying out the `u32` releases it.
        let entry = self.fresh_temp();
        self.line(format!("let {entry} = ({}) as u32;", funcref.code));

        // Dispatch through the shared `call_ref_t{ti}` method (see
        // `dispatch_method_lines`): it holds the `match` on the funcref.
        self.dispatch_sigs.insert(type_index);
        let (prefix, temps) = self.result_binding(&results)?;
        let sep = if arg_list.is_empty() { "" } else { ", " };
        self.line(format!(
            "{prefix}self.call_ref_t{type_index}({entry}{sep}{arg_list});"
        ));
        self.push_temps(temps);
        if tail {
            self.emit_return()?;
        }
        Ok(())
    }

    /// Build the `let …: … = ` binding prefix for a value producing `results`,
    /// allocating a fresh temporary per result. Returns the prefix (empty for
    /// zero results, so the value is emitted as a bare statement) and the
    /// temporaries to push once the value has been emitted. A multi-value
    /// result binds a tuple pattern.
    pub(super) fn result_binding(
        &mut self,
        results: &[ValType],
    ) -> Result<(String, Vec<(String, ValType)>), TranspileError> {
        match results {
            [] => Ok((String::new(), Vec::new())),
            [ty] => {
                let name = self.fresh_temp();
                let prefix = format!("let {name}: {} = ", rust_type(*ty, self.ctx.type_kinds)?);
                Ok((prefix, vec![(name, *ty)]))
            }
            many => {
                let tys = rust_types(many, self.ctx.type_kinds)?;
                let names: Vec<String> = many.iter().map(|_| self.fresh_temp()).collect();
                let prefix = format!("let ({}): ({}) = ", names.join(", "), tys.join(", "));
                let temps = names.into_iter().zip(many.iter().copied()).collect();
                Ok((prefix, temps))
            }
        }
    }

    /// Push materialised result temporaries onto the operand stack (stable).
    pub(super) fn push_temps(&mut self, temps: Vec<(String, ValType)>) {
        for (code, ty) in temps {
            self.push(Val {
                code,
                ty,
                stable: true,
            });
        }
    }

    /// Pop `n` result values (in source order) and format them as a single
    /// expression (`n == 1`) or a tuple (`n > 1`); `None` for `n == 0`.
    pub(super) fn pop_results(&mut self, n: usize) -> Result<Option<String>, TranspileError> {
        if n == 0 {
            return Ok(None);
        }
        let mut vals = Vec::with_capacity(n);
        for _ in 0..n {
            vals.push(self.pop()?);
        }
        vals.reverse();
        let mut parts = Vec::with_capacity(vals.len());
        for v in vals {
            parts.push(self.move_val(&v)?);
        }
        let joined = parts.join(", ");
        Ok(Some(if n == 1 {
            joined
        } else {
            format!("({joined})")
        }))
    }

    pub(super) fn emit_return(&mut self) -> Result<(), TranspileError> {
        // A `return` from inside a `try` body cannot leave its `catch_unwind`
        // closure directly; route it through the function-wide return signal that
        // each enclosing try's dispatch re-issues. A handler runs in the landing
        // pad, so a `return` there is a plain return unless it sits in an outer
        // try's body.
        if self
            .try_barriers
            .iter()
            .any(|b| b.phase == CatchPhase::Body)
        {
            return self.emit_return_escape();
        }
        match self.pop_results(self.results.len())? {
            Some(code) => self.term(format!("return {code};")),
            None => self.term("return;".to_string()),
        }
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    pub(super) fn end_function(&mut self) -> Result<(), TranspileError> {
        // If control falls off the end, the remaining stack values are the
        // results; otherwise a `return`/`br`/`unreachable` already produced them.
        if self.reachable {
            self.trailing = self.pop_results(self.results.len())?;
        }
        Ok(())
    }

    /// Append this function's Rust source to `out`, streaming the body straight
    /// in rather than materialising it as a second `String`, and return the
    /// helper dependencies it discovered.
    ///
    /// Each non-empty line is prefixed by `line_prefix` (empty for a free `pub
    /// fn`, four spaces when the function is a method inside a chunk's `impl`
    /// block); body statements additionally carry the usual four-space
    /// function-body indent. Because the whole body (`self.cur`, potentially
    /// hundreds of megabytes for a pathologically large function) is consumed
    /// line by line and dropped as it is copied, it is never held twice.
    pub(in crate::codegen) fn finish(
        mut self,
        index: usize,
        params: &[ValType],
        results: &[ValType],
        line_prefix: &str,
        out: &mut String,
    ) -> Result<GenMeta, TranspileError> {
        // A `return` escaping a try body stashes its results in these holders and
        // raises a shared flag; both are declared once at the function top so
        // every try's dispatch can re-issue the return.
        if self.uses_ret_escape {
            let mut decls = Vec::with_capacity(results.len() + 1);
            decls.push(Node::Line("let mut __returning: bool = false;".to_string()));
            for (i, ty) in results.iter().enumerate() {
                decls.push(Node::Line(format!(
                    "let mut __rv{i}: {} = {};",
                    rust_type(*ty, self.ctx.type_kinds)?,
                    default_value(*ty, self.ctx.type_kinds)
                )));
            }
            decls.append(&mut self.cur);
            self.cur = decls;
        }
        let mut params_src = String::new();
        // Stateful modules pass their memory/globals through `&mut self`.
        if self.ctx.is_method {
            params_src.push_str("&mut self");
        }
        for (i, ty) in params.iter().enumerate() {
            if self.ctx.is_method || i > 0 {
                params_src.push_str(", ");
            }
            let keyword = if self.mutable_locals.contains(&index_u32(i)?) {
                "mut "
            } else {
                ""
            };
            params_src.push_str(&format!(
                "{keyword}l{i}: {}",
                rust_type(*ty, self.ctx.type_kinds)?
            ));
        }

        let ret = match results {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty, self.ctx.type_kinds)?),
            many => format!(
                " -> ({})",
                rust_types(many, self.ctx.type_kinds)?.join(", ")
            ),
        };

        // A deeply nested function is flattened to a `loop { match pc { … } }`
        // dispatch so its rendered nesting cannot overflow rustc's parser; an
        // ordinary function keeps its readable nested form and its tail
        // expression. When flattening, the tail is returned from the dispatch's
        // exit state instead.
        let flatten = self.max_depth > FLATTEN_DEPTH_THRESHOLD && can_flatten(&self.cur);
        // A flattened dispatch past `split_dispatch` arms is emitted as sibling
        // part functions over a shared state struct (see [`SplitPlan`]). The part
        // functions sit beside the driver (in the `impl` for a method), while the
        // state struct must live at module scope: a free function's chunk already
        // is module scope, but a method's `impl` block is not — so a method's
        // struct is bubbled up through `GenMeta` and emitted at the crate root.
        let (body, trailing, split) = if flatten {
            let plan = SplitPlan {
                max_arms: self.ctx.split_dispatch,
                func_index: index,
                is_method: self.ctx.is_method,
                params: params
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| Ok((i, rust_type(*ty, self.ctx.type_kinds)?.to_string())))
                    .collect::<Result<Vec<_>, TranspileError>>()?,
                ret: match results {
                    [] => None,
                    [ty] => Some(rust_type(*ty, self.ctx.type_kinds)?.to_string()),
                    many => Some(format!(
                        "({})",
                        rust_types(many, self.ctx.type_kinds)?.join(", ")
                    )),
                },
            };
            let artifacts = flatten_body(self.cur, self.trailing, !results.is_empty(), Some(plan));
            (
                artifacts.body,
                None,
                Some((artifacts.siblings, artifacts.state_struct)),
            )
        } else {
            (self.cur, self.trailing, None)
        };

        // Reserve the function's whole size in one go so appending it never
        // triggers repeated doublings of a multi-megabyte buffer.
        let body_bytes = estimate_body_len(&body);
        out.reserve(body_bytes + params_src.len() + ret.len() + ALLOW.len() + 32);

        let (siblings, state_struct) = match split {
            Some((siblings, state_struct)) => (siblings, state_struct),
            None => (Vec::new(), String::new()),
        };
        // A free function's state struct precedes its driver at module scope; a
        // method's is bubbled to the crate root instead (an `impl` cannot hold a
        // struct definition).
        let mut state_structs = Vec::new();
        if !state_struct.is_empty() {
            if self.ctx.is_method {
                state_structs.push(state_struct);
            } else {
                out.push_str(&state_struct);
            }
        }

        // For a method the lint-suppression attribute is applied once on the
        // enclosing `impl`; free functions carry it individually.
        if !self.ctx.is_method {
            out.push_str(line_prefix);
            out.push_str(ALLOW);
            out.push('\n');
        }
        out.push_str(line_prefix);
        out.push_str(&format!("pub fn func{index}({params_src}){ret} {{\n"));

        // Render the deferred body (consuming it line by line so a huge function
        // is never held twice), then the tail expression at the body's level.
        render_body_into(body, line_prefix, out);
        if let Some(trailing) = trailing {
            push_body_line(out, &trailing, 0, line_prefix);
        }

        out.push_str(line_prefix);
        out.push_str("}\n");

        // The sibling part functions follow the driver, at the same scope.
        for line in siblings {
            if !line.is_empty() {
                out.push_str(line_prefix);
                out.push_str(&line);
            }
            out.push('\n');
        }
        Ok(GenMeta {
            helpers: self.used_helpers,
            rt: self.used_rt,
            simd: self.used_simd,
            dispatch_sigs: self.dispatch_sigs,
            uses_eh: self.uses_eh,
            state_structs,
        })
    }
}
