use wasmparser::ValType;

use super::super::{ALLOW, GenMeta, Val, index_u32, rust_type, rust_types};
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    // ----- calls -----------------------------------------------------------
    pub(super) fn call(&mut self, function_index: u32) -> Result<(), TranspileError> {
        let (params, results) = self
            .ctx
            .full_sig(function_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call to unknown function".into()))?;
        let param_count = params.len();
        let results = results.to_vec();

        // A call may read and write memory and globals. Freezing every operand
        // first both materialises the arguments and pins any earlier value that
        // must not observe the call's side effects (spill-before-mutation).
        self.spill_nonstable()?;

        let mut args = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = args
            .into_iter()
            .map(|a| a.code)
            .collect::<Vec<_>>()
            .join(", ");

        // A call is not re-evaluatable, so bind its result(s) to a temporary at
        // exactly this point (mirroring `memory_grow`) and push the stable
        // temporaries. A multi-value result is destructured from a tuple.
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

        // Freeze operands (arguments and the table index) before the call, both
        // to share them across every match arm and to pin any earlier value
        // against the callee's side effects (spill-before-mutation).
        self.spill_nonstable()?;
        let index = self.pop()?;
        let mut args = Vec::with_capacity(sig.params.len());
        for _ in 0..sig.params.len() {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = args
            .into_iter()
            .map(|a| a.code)
            .collect::<Vec<_>>()
            .join(", ");

        // No local function has the requested type. Without a host, every entry
        // mismatches, so the call always traps and control cannot continue past
        // it. With a host, a table entry may still be an external handle the
        // host resolves, so fall through to the shared dispatch method (which
        // has only the null/external/catch-all arms).
        if targets.is_empty() && !self.ctx.has_imports {
            self.line("panic!(\"indirect call type mismatch\");".to_string());
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
                let prefix = format!("let {name}: {} = ", rust_type(*ty)?);
                Ok((prefix, vec![(name, *ty)]))
            }
            many => {
                let tys = rust_types(many)?;
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
        let joined = vals
            .into_iter()
            .map(|v| v.code)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Some(if n == 1 {
            joined
        } else {
            format!("({joined})")
        }))
    }

    pub(super) fn emit_return(&mut self) -> Result<(), TranspileError> {
        match self.pop_results(self.results.len())? {
            Some(code) => self.line(format!("return {code};")),
            None => self.line("return;".to_string()),
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
        self,
        index: usize,
        params: &[ValType],
        results: &[ValType],
        line_prefix: &str,
        out: &mut String,
    ) -> Result<GenMeta, TranspileError> {
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
            params_src.push_str(&format!("{keyword}l{i}: {}", rust_type(*ty)?));
        }

        let ret = match results {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty)?),
            many => format!(" -> ({})", rust_types(many)?.join(", ")),
        };

        // Reserve the function's whole size in one go so appending it never
        // triggers repeated doublings of a multi-megabyte buffer.
        let body_bytes: usize = self
            .cur
            .iter()
            .map(|l| line_prefix.len() + l.len() + 5)
            .sum();
        out.reserve(body_bytes + params_src.len() + ret.len() + ALLOW.len() + 32);

        // For a method the lint-suppression attribute is applied once on the
        // enclosing `impl`; free functions carry it individually.
        if !self.ctx.is_method {
            out.push_str(line_prefix);
            out.push_str(ALLOW);
            out.push('\n');
        }
        out.push_str(line_prefix);
        out.push_str(&format!("pub fn func{index}({params_src}){ret} {{\n"));

        // Emit one body line: a non-empty statement gets `line_prefix` plus the
        // four-space function-body indent; a blank line stays bare (matching the
        // single-file renderer's `indent`, which leaves empty lines empty).
        let push_body_line = |out: &mut String, line: &str| {
            if !line.is_empty() {
                out.push_str(line_prefix);
                out.push_str("    ");
                out.push_str(line);
            }
            out.push('\n');
        };
        for line in self.cur {
            push_body_line(out, &line);
        }
        if let Some(trailing) = self.trailing {
            push_body_line(out, &trailing);
        }

        out.push_str(line_prefix);
        out.push_str("}\n");
        Ok(GenMeta {
            helpers: self.used_helpers,
            rt: self.used_rt,
            dispatch_sigs: self.dispatch_sigs,
        })
    }
}
