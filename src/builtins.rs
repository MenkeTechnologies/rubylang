//! Registered fusevm builtins + the Ruby method/kernel dispatch.
//!
//! Each `Op::CallBuiltin(id, argc)` the compiler emits lands in one of the `b_*`
//! functions here: they marshal values off the VM stack, call into the
//! thread-local `RubyHost`, and push the result. Method dispatch (`dispatch`)
//! and the Kernel functions (`kernel`) do fine-grained `with_host` borrows so
//! that iterator methods can run block bodies as nested VMs without holding the
//! host borrow across the re-entry.

use crate::host::ops;
use crate::host::{
    call_instance_method, call_method, call_proc, current_block, has_pending_signal,
    raise_signal_break, raise_signal_next, raise_signal_retry, raise_signal_return,
    raise_signal_throw, take_break, take_throw, with_host, RKey, RubyHost,
};
use fusevm::{Value, VM};
use indexmap::IndexMap;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Register every rubylang builtin on `vm`.
pub fn install(vm: &mut VM) {
    vm.register_builtin(ops::GETLOCAL, b_getlocal);
    vm.register_builtin(ops::SETLOCAL, b_setlocal);
    vm.register_builtin(ops::GETIVAR, b_getivar);
    vm.register_builtin(ops::SETIVAR, b_setivar);
    vm.register_builtin(ops::GETCVAR, b_getcvar);
    vm.register_builtin(ops::SETCVAR, b_setcvar);
    vm.register_builtin(ops::GETGVAR, b_getgvar);
    vm.register_builtin(ops::SETGVAR, b_setgvar);
    vm.register_builtin(ops::GETCONST, b_getconst);
    vm.register_builtin(ops::SETCONST, b_setconst);
    vm.register_builtin(ops::CALL, b_call);
    vm.register_builtin(ops::CALL_BLK, b_call_blk);
    vm.register_builtin(ops::CALL_METHOD, b_call_method);
    vm.register_builtin(ops::CALL_METHOD_BLK, b_call_method_blk);
    vm.register_builtin(ops::MKSTR, b_mkstr);
    vm.register_builtin(ops::MKSTRF, b_mkstrf);
    vm.register_builtin(ops::MKSYM, b_mksym);
    vm.register_builtin(ops::MKARRAY, b_mkarray);
    vm.register_builtin(ops::MKHASH, b_mkhash);
    vm.register_builtin(ops::MKRANGE, b_mkrange);
    vm.register_builtin(ops::MKPROC, b_mkproc);
    vm.register_builtin(ops::MKLAMBDA, b_mklambda);
    vm.register_builtin(ops::MKREGEX, b_mkregex);
    vm.register_builtin(ops::YIELD, b_yield);
    vm.register_builtin(ops::TRUTHY, b_truthy);
    vm.register_builtin(ops::INDEX_GET, b_index_get);
    vm.register_builtin(ops::INDEX_SET, b_index_set);
    vm.register_builtin(ops::TOSTR, b_tostr);
    vm.register_builtin(ops::DEFINED, b_defined);
    vm.register_builtin(ops::SIG_BREAK, b_sig_break);
    vm.register_builtin(ops::SIG_NEXT, b_sig_next);
    vm.register_builtin(ops::SIG_RETURN, b_sig_return);
    vm.register_builtin(ops::SIG_RETRY, b_sig_retry);
    vm.register_builtin(ops::NO_MATCH, b_no_match);
    vm.register_builtin(ops::GETSELF, b_getself);
    vm.register_builtin(ops::BEGIN, b_begin);
    vm.register_builtin(ops::SUPER, b_super);
    vm.register_builtin(ops::SUPER_FWD, b_super_fwd);
    vm.register_builtin(ops::SUPER_BLK, b_super_blk);
    vm.register_builtin(ops::SUPER_FWD_BLK, b_super_fwd_blk);
    vm.register_builtin(ops::MKARGS, b_mkargs);
    vm.register_builtin(ops::MKHASH_MERGE, b_mkhash_merge);
    vm.register_builtin(ops::CALL_ARR, b_call_arr);
    vm.register_builtin(ops::CALL_METHOD_ARR, b_call_method_arr);
    vm.register_builtin(ops::CALL_ARR_BLK, b_call_arr_blk);
    vm.register_builtin(ops::CALL_METHOD_ARR_BLK, b_call_method_arr_blk);
    vm.register_builtin(ops::DEFINED_DESC, b_defined_desc);
    vm.register_builtin(ops::DEFINE_SINGLETON, b_define_singleton);
    vm.register_builtin(ops::DEFINE_METHOD_DYN, b_define_method_dyn);
    vm.register_builtin(ops::FIRE_HOOK, b_fire_hook);
}

/// Whether `name` is a built-in Kernel function (so `defined?(puts)` and the
/// like report `"method"`). These are dispatched by `kernel()`, not registered
/// as ordinary methods, so `responds_to` doesn't see them.
fn is_kernel_function(name: &str) -> bool {
    matches!(
        name,
        "puts"
            | "print"
            | "p"
            | "pp"
            | "gets"
            | "open"
            | "require"
            | "require_relative"
            | "load"
            | "intercept"
            | "raise"
            | "fail"
            | "abort"
            | "warn"
            | "exit"
            | "exit!"
            | "loop"
            | "lambda"
            | "proc"
            | "format"
            | "sprintf"
            | "printf"
            | "sleep"
            | "rand"
            | "srand"
            | "catch"
            | "throw"
            | "eval"
            | "block_given?"
            | "Integer"
            | "Float"
            | "String"
            | "Array"
    )
}

/// `defined?(operand)` runtime check for a named operand: returns the Ruby
/// description string (`"constant"`, `"local-variable"`, …) or nil when the
/// thing is not defined. The compiler passes a `kind` tag and the `name`.
fn b_defined_desc(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    let kind = name_of(&vm.pop());
    let set = |v: Value| !matches!(v, Value::Undef);
    let desc: Option<&str> = match kind.as_str() {
        "const" => {
            // A *qualified* name (`A::B`) is defined only if it is actually
            // registered — the builtin-exception name heuristic (`*Error`) must not
            // fire on it, or `defined?(Mustermann::Error)` falsely reports a
            // constant before the module defines it (skipping the `unless defined?`
            // guard that gems use to define it once).
            let defined = with_host(|h| {
                set(h.get_const(&name)) || h.class_exists(&name) || h.is_builtin_class(&name)
            }) || (!name.contains("::") && is_builtin_exception(&name));
            defined.then_some("constant")
        }
        "ivar" => with_host(|h| set(h.get_ivar(&name))).then_some("instance-variable"),
        "gvar" => with_host(|h| set(h.get_global(&name))).then_some("global-variable"),
        "cvar" => with_host(|h| {
            let this = h.current_self();
            h.cvar_owner(&this)
                .map(|cls| set(h.get_cvar(&cls, &name)))
                .unwrap_or(false)
        })
        .then_some("class variable"),
        "local" => {
            if with_host(|h| h.local_defined(&name)) {
                Some("local-variable")
            } else if with_host(|h| h.responds_to(&name)) || is_kernel_function(&name) {
                Some("method")
            } else {
                None
            }
        }
        "yield" => current_block().is_some().then_some("yield"),
        _ => None,
    };
    match desc {
        Some(s) => new_str(s.to_string()),
        None => Value::Undef,
    }
}

/// Concatenate `argc` arrays into one (splat argument/element building).
/// Merge `argc` hashes (left-to-right, later keys win) into one. Used to build a
/// hash literal too large for a single `MKHASH` (its argc is a `u8`), by chunking
/// it into sub-hashes.
fn b_mkhash_merge(vm: &mut VM, argc: u8) -> Value {
    let pieces = pop_n(vm, argc as usize);
    let mut map: IndexMap<RKey, Value> = IndexMap::new();
    for p in &pieces {
        if let Some(m) = with_host(|h| h.as_hash(p)) {
            for (k, v) in m {
                map.insert(k, v);
            }
        }
    }
    with_host(|h| h.new_hash(map))
}

fn b_mkargs(vm: &mut VM, argc: u8) -> Value {
    let pieces = pop_n(vm, argc as usize);
    let mut out = Vec::new();
    for p in &pieces {
        match with_host(|h| h.as_array(p)) {
            Some(xs) => out.extend(xs),
            // A splat of a Range/Set/Enumerator (`*1..5`, `*set`) expands to its
            // elements via `to_a`; a plain scalar (`*5`) is a one-element run.
            None if with_host(|h| {
                h.is_a(p, "Range")
                    || h.is_a(p, "Set")
                    || h.is_a(p, "Enumerator")
                    || h.is_a(p, "MatchData")
            }) =>
            {
                match dispatch(p, "to_a", &[], None)
                    .ok()
                    .and_then(|a| with_host(|h| h.as_array(&a)))
                {
                    Some(xs) => out.extend(xs),
                    None => out.push(p.clone()),
                }
            }
            None => out.push(p.clone()),
        }
    }
    with_host(|h| h.new_array(out))
}

/// Self call with a spread argument array: stack `[name, args_array]`.
fn b_call_arr(vm: &mut VM, _: u8) -> Value {
    let arr = vm.pop();
    let name = name_of(&vm.pop());
    let args = with_host(|h| h.as_array(&arr).unwrap_or_default());
    match dispatch_call(&name, &args, None) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

/// Self call with a spread argument array and a block: `[name, args, proc]`.
/// The block operand of a `*_BLK` call op: a nil value (`&nil`, or a forwarded
/// block that was never given) means "no block", not a block that happens to be
/// nil — so `block_given?` stays false and `&blk` capture yields nil. A real
/// block from `MKPROC` is always a Proc, never `Undef`, so this only ever
/// normalizes the block-pass-by-value path (`&expr` / `...` forwarding).
fn block_operand(v: Value) -> Option<Value> {
    match v {
        Value::Undef => None,
        other => Some(other),
    }
}

fn b_call_arr_blk(vm: &mut VM, _: u8) -> Value {
    let block = block_operand(vm.pop());
    let arr = vm.pop();
    let name = name_of(&vm.pop());
    let args = with_host(|h| h.as_array(&arr).unwrap_or_default());
    match dispatch_call(&name, &args, block) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

/// Method call with a spread argument array and a block:
/// `[recv, name, args, proc]`.
fn b_call_method_arr_blk(vm: &mut VM, _: u8) -> Value {
    let block = block_operand(vm.pop());
    let arr = vm.pop();
    let name = name_of(&vm.pop());
    let recv = vm.pop();
    let args = with_host(|h| h.as_array(&arr).unwrap_or_default());
    match dispatch(&recv, &name, &args, block) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

/// Method call with a spread argument array: stack `[recv, name, args_array]`.
fn b_call_method_arr(vm: &mut VM, _: u8) -> Value {
    let arr = vm.pop();
    let name = name_of(&vm.pop());
    let recv = vm.pop();
    let args = with_host(|h| h.as_array(&arr).unwrap_or_default());
    match dispatch(&recv, &name, &args, None) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

fn b_getself(_vm: &mut VM, _: u8) -> Value {
    with_host(|h| h.current_self())
}

fn b_super(vm: &mut VM, argc: u8) -> Value {
    let args = pop_n(vm, argc as usize);
    match crate::host::call_super(Some(args)) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

fn b_super_fwd(vm: &mut VM, _: u8) -> Value {
    match crate::host::call_super(None) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

/// `super(args) { blk }` — the proc rides on top of the args; argc = n_args + 1.
fn b_super_blk(vm: &mut VM, argc: u8) -> Value {
    let block = block_operand(vm.pop());
    let args = pop_n(vm, argc.saturating_sub(1) as usize);
    match crate::host::call_super_blk(Some(args), block) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

/// `super { blk }` — forward args, pass a new block (the proc on top of stack).
fn b_super_fwd_blk(vm: &mut VM, _: u8) -> Value {
    let block = block_operand(vm.pop());
    match crate::host::call_super_blk(None, block) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

fn b_begin(vm: &mut VM, _: u8) -> Value {
    let id = match vm.pop() {
        Value::Int(n) => n as usize,
        _ => return abort(vm, "bad begin id".into()),
    };
    match crate::host::run_begin(id) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

/// Pop `n` values, returning them in push order (first pushed at index 0).
fn pop_n(vm: &mut VM, n: usize) -> Vec<Value> {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(vm.pop());
    }
    v.reverse();
    v
}

/// `case/in` fell through with no matching clause and no `else`.
fn b_no_match(vm: &mut VM, _: u8) -> Value {
    let subj = vm.pop();
    let msg = with_host(|h| h.inspect(&subj));
    let e = raise_exc("NoMatchingPatternError", &msg);
    abort(vm, e)
}

/// Abort the running chunk with an error, halting the VM cleanly.
fn abort(vm: &mut VM, e: String) -> Value {
    // Record this frame on the in-flight exception (if any) for the MRI-format
    // uncaught printer: the source line of the raising op plus the file. Frames
    // arrive innermost-first as the error unwinds through each chunk's `abort`.
    // The VM pre-increments `ip` before dispatching an op, so the raising op is
    // at `ip - 1`.
    let line = vm
        .chunk
        .lines
        .get(vm.ip.wrapping_sub(1))
        .copied()
        .unwrap_or(0);
    let src = crate::host::current_file_path().unwrap_or_else(|| "-e".into());
    with_host(|h| h.record_backtrace_frame(&src, line));
    with_host(|h| h.error = Some(e));
    vm.ip = vm.chunk.ops.len();
    Value::Undef
}

fn name_of(v: &Value) -> String {
    with_host(|h| {
        h.as_str(v)
            .or_else(|| h.as_symbol(v))
            .or_else(|| h.classref_name(v))
            .unwrap_or_default()
    })
}

// ---- variable builtins ----------------------------------------------------

fn b_getlocal(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    let (defined, v) = with_host(|h| (h.local_defined(&name), h.get_local(&name)));
    if defined {
        return v;
    }
    // Bare zero-arg Kernel queries that read like variables.
    match name.as_str() {
        "block_given?" => return Value::Bool(current_block().is_some()),
        "__method__" => {
            return with_host(|h| {
                let (_, m, _, _) = h.super_context();
                match m {
                    Some(name) => h.new_symbol(&name),
                    None => Value::Undef,
                }
            })
        }
        // The directory of the file currently running (the top of the require
        // file-dir stack), as a String. For a `-e` one-liner or piped stdin the
        // stack is seeded with the current directory, so `__dir__` returns that.
        "__dir__" => {
            return match crate::host::current_file_dir() {
                Some(d) => new_str(d.to_string_lossy().to_string()),
                None => Value::Undef,
            }
        }
        // The path of the file currently running (top of the file-path stack) —
        // the script path as given for the top-level script, the required file's
        // own path inside a required file, and "-e" for a one-liner.
        "__FILE__" => {
            return match crate::host::current_file_path() {
                Some(p) => new_str(p),
                None => Value::Undef,
            }
        }
        _ => {}
    }
    // A bare name that is not a local is a zero-arg call: a method on `self` /
    // top-level method (errors propagate), else a Kernel function (`puts`, …).
    // An unknown bare name reads as nil, matching rubylang's lenient behaviour.
    //
    // When `self` is a builtin-typed value (a String, Array, Integer, …, not a
    // user object or class), dispatch on that value FIRST — its native/reopened
    // methods (`empty?`, `upcase`, `length`) must win over the `responds_to`
    // path, which can spuriously report a *global* method of the same name after
    // a gem defines one (`empty?` after activesupport loads). `responds_to`
    // cannot confirm native methods, so a genuine miss falls through below.
    let this = with_host(|h| h.current_self());
    let builtin_self =
        with_host(|h| h.object_class(&this).is_none() && h.classref_name(&this).is_none());
    if builtin_self {
        match dispatch(&this, &name, &[], None) {
            Ok(v) => return propagate(vm, v),
            Err(e) if e.starts_with("undefined method") => {}
            Err(e) => return abort(vm, e),
        }
    }
    if with_host(|h| h.responds_to(&name)) {
        return match dispatch_call(&name, &[], None) {
            Ok(v) => propagate(vm, v),
            Err(e) => abort(vm, e),
        };
    }
    match kernel(&name, &[], None) {
        Ok(v) => propagate(vm, v),
        Err(e) if e.starts_with("undefined method") => Value::Undef,
        Err(e) => abort(vm, e),
    }
}

/// If a non-local control signal (a `return` from inside a block) is pending
/// after a call returns, halt this chunk too so it unwinds to the method
/// boundary that will consume the signal.
fn propagate(vm: &mut VM, v: Value) -> Value {
    if has_pending_signal() {
        vm.ip = vm.chunk.ops.len();
    }
    v
}
fn b_setlocal(vm: &mut VM, _: u8) -> Value {
    let val = vm.pop();
    let name = name_of(&vm.pop());
    with_host(|h| h.set_local(&name, val.clone()));
    val
}
fn b_getivar(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    with_host(|h| h.get_ivar(&name))
}
fn b_setivar(vm: &mut VM, _: u8) -> Value {
    let val = vm.pop();
    let name = name_of(&vm.pop());
    with_host(|h| h.set_ivar(&name, val.clone()));
    val
}
fn b_getcvar(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    with_host(|h| {
        let this = h.current_self();
        match h.cvar_owner(&this) {
            Some(cls) => h.get_cvar(&cls, &name),
            None => Value::Undef,
        }
    })
}
fn b_setcvar(vm: &mut VM, _: u8) -> Value {
    let val = vm.pop();
    let name = name_of(&vm.pop());
    with_host(|h| {
        let this = h.current_self();
        if let Some(cls) = h.cvar_owner(&this) {
            h.set_cvar(&cls, &name, val.clone());
        }
    });
    val
}
fn b_getgvar(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    with_host(|h| h.get_global(&name))
}
fn b_setgvar(vm: &mut VM, _: u8) -> Value {
    let val = vm.pop();
    let name = name_of(&vm.pop());
    with_host(|h| h.set_global(&name, val.clone()));
    val
}
fn b_getconst(vm: &mut VM, _: u8) -> Value {
    let encoded = name_of(&vm.pop());
    // A bare constant read inside a namespace is compiled to a `\x1f`-separated
    // candidate chain (innermost-qualified first, then the top-level name) — the
    // lexical constant search. Try each in order; the last (or a name with no
    // separator, at the top level) is the plain name.
    for name in encoded.split('\u{1f}') {
        let v = with_host(|h| h.get_const(name));
        if !matches!(v, Value::Undef) {
            return v;
        }
        // An unassigned constant that names a class (user-defined, or a builtin
        // exception like `RuntimeError`) resolves to a class reference. The
        // builtin-exception fallback is a name heuristic (`*Error`), so it must
        // only fire for an unqualified candidate — otherwise a namespaced miss
        // like `Foo::NegativeError` would spuriously resolve and shadow the real
        // top-level `NegativeError` later in the candidate chain.
        if with_host(|h| h.class_exists(name) || h.is_builtin_class(name))
            || (!name.contains("::") && is_builtin_exception(name))
        {
            return with_host(|h| h.class_ref(name));
        }
        // A pending `autoload name, "path"`: require the file (once), then retry
        // this candidate — the required file defines the constant/class.
        if let Some(path) = with_host(|h| h.take_autoload(name)) {
            let pv = with_host(|h| h.new_string(path));
            if let Err(e) = do_require(&[pv], ReqMode::Require) {
                return abort(vm, e);
            }
            let v = with_host(|h| h.get_const(name));
            if !matches!(v, Value::Undef) {
                return v;
            }
            if with_host(|h| h.class_exists(name) || h.is_builtin_class(name)) {
                return with_host(|h| h.class_ref(name));
            }
        }
    }
    Value::Undef
}

/// Builtin exception class names that resolve to a class reference even without
/// a user `class` definition.
fn is_builtin_exception(name: &str) -> bool {
    name.ends_with("Error") || name == "Exception" || name == "StopIteration"
}
/// Whether `cls` is an exception class — a builtin one, or a user class whose
/// superclass chain reaches a builtin exception (e.g. `class MyErr < StandardError`).
fn is_exception_class(cls: &str) -> bool {
    if is_builtin_exception(cls) {
        return true;
    }
    with_host(|h| {
        let mut cur = h.superclass_of(cls);
        while let Some(name) = cur {
            if is_builtin_exception(&name) {
                return true;
            }
            cur = h.superclass_of(&name);
        }
        false
    })
}
fn b_setconst(vm: &mut VM, _: u8) -> Value {
    let val = vm.pop();
    let name = name_of(&vm.pop());
    // `Point = Struct.new(...)` names the anonymous struct after the constant.
    let val = with_host(|h| match h.classref_name(&val) {
        Some(cref) if cref.starts_with("Struct:") && h.struct_def(&cref).is_some() => {
            h.rename_struct(&cref, &name);
            // Also move any methods defined by a `Struct.new(...) do ... end` body.
            h.rename_class(&cref, &name);
            h.class_ref(&name)
        }
        // `Foo = Class.new` / `Foo = Module.new` names the anonymous class/module
        // after the constant (MRI behavior), so `include Foo` resolves by name.
        Some(cref) if h.is_anon_class(&cref) => {
            h.rename_class(&cref, &name);
            h.class_ref(&name)
        }
        _ => val.clone(),
    });
    with_host(|h| h.set_const(&name, val.clone()));
    val
}
fn b_defined(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    Value::Bool(with_host(|h| h.local_defined(&name)))
}

/// `def obj.m` / `def Klass.m` / `class << obj; def m`: stack is
/// `[recv, name, synth]`. Register the stashed body (under `synth`) as a
/// singleton method on `recv` (a per-object singleton, or a class method when
/// `recv` is a class). Evaluates to `:name`.
fn b_define_singleton(vm: &mut VM, _: u8) -> Value {
    let synth = name_of(&vm.pop());
    let name = name_of(&vm.pop());
    let recv = vm.pop();
    let Some(def) = with_host(|h| h.method_def(&synth)) else {
        return abort(vm, format!("internal: missing method body '{synth}'"));
    };
    with_host(|h| {
        if let Some(cls) = h.classref_name(&recv) {
            h.add_class_method(&cls, &name, def);
        } else if let Value::Obj(id) = recv {
            h.add_singleton_method(id, &name, def);
        }
    });
    with_host(|h| h.new_symbol(&name))
}

/// A plain `def`: stack is `[name, synth]`. When a `class_eval`/`instance_eval`
/// target is active the body registers onto it; otherwise a no-op (the method is
/// already hoisted). Evaluates to `:name`.
fn b_define_method_dyn(vm: &mut VM, _: u8) -> Value {
    let synth = name_of(&vm.pop());
    let name = name_of(&vm.pop());
    crate::host::apply_def_target(&name, &synth);
    with_host(|h| h.new_symbol(&name))
}

/// `inherited`/`included`/`extended`/`prepended` hook: stack is
/// `[module, hook, target]`. If `module` defines the class method `hook`, call
/// it with a reference to `target` (the subclass or including class).
fn b_fire_hook(vm: &mut VM, _: u8) -> Value {
    let target = name_of(&vm.pop());
    let hook = name_of(&vm.pop());
    let module = name_of(&vm.pop());
    // `module` may be the unqualified name of a module nested in `target`'s
    // namespace: `extend LazyLoadHooks` inside `module ActiveSupport` compiles
    // the short `LazyLoadHooks` (the nested module isn't registered yet at
    // compile time, so `resolve_class_name` can't qualify it), but its class
    // methods register under `ActiveSupport::LazyLoadHooks`. Canonicalize
    // against the target's namespace before the hook lookup — mirroring
    // `find_class_method`'s extends resolution — else `self.extended` /
    // `self.included` silently never fires.
    let module = with_host(|h| {
        if h.class_exists(&module) {
            return module.clone();
        }
        let nested = format!("{target}::{module}");
        if h.class_exists(&nested) {
            return nested;
        }
        // A superclass named unqualified because it lives in a *required* file
        // (so it wasn't registered when this `class Kid < Parent` was compiled and
        // couldn't be qualified): resolve it against the target's namespace,
        // walking outward. `target = A::B::Kid`, `module = Parent` → try
        // `A::B::Parent`, then `A::Parent`. Without this, an `inherited` hook on a
        // required superclass never fires.
        let mut prefix = target.as_str();
        while let Some(idx) = prefix.rfind("::") {
            prefix = &prefix[..idx];
            let cand = format!("{prefix}::{module}");
            if h.class_exists(&cand) {
                return cand;
            }
        }
        module.clone()
    });
    if let Some(def) = with_host(|h| h.find_class_method(&module, &hook)) {
        let recv = with_host(|h| h.class_ref(&module));
        let arg = with_host(|h| h.class_ref(&target));
        if let Err(e) = crate::host::call_class_method(recv, &def, &hook, &module, &[arg], None) {
            return abort(vm, e);
        }
    }
    Value::Undef
}

// ---- calls ----------------------------------------------------------------

fn b_call(vm: &mut VM, argc: u8) -> Value {
    let mut vals = pop_n(vm, argc as usize);
    let name = name_of(&vals.remove(0));
    match dispatch_call(&name, &vals, None) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}
fn b_call_blk(vm: &mut VM, argc: u8) -> Value {
    let mut vals = pop_n(vm, argc as usize);
    let block = block_operand(vals.pop().unwrap_or(Value::Undef));
    let name = name_of(&vals.remove(0));
    match dispatch_call(&name, &vals, block) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}
fn b_call_method(vm: &mut VM, argc: u8) -> Value {
    let mut vals = pop_n(vm, argc as usize);
    let recv = vals.remove(0);
    let name = name_of(&vals.remove(0));
    match dispatch(&recv, &name, &vals, None) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}
fn b_call_method_blk(vm: &mut VM, argc: u8) -> Value {
    let mut vals = pop_n(vm, argc as usize);
    let block = block_operand(vals.pop().unwrap_or(Value::Undef));
    let recv = vals.remove(0);
    let name = name_of(&vals.remove(0));
    match dispatch(&recv, &name, &vals, block) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}

// ---- constructors ---------------------------------------------------------

fn b_mkstr(vm: &mut VM, argc: u8) -> Value {
    let parts = pop_n(vm, argc as usize);
    let mut s = String::new();
    for p in &parts {
        s.push_str(&display(p));
    }
    with_host(|h| h.new_string(s))
}

/// `# frozen_string_literal: true`: build a string literal and freeze it, so
/// `"x".frozen?` is true and mutating it raises FrozenError. Emitted (in place of
/// `MKSTR`) only for non-interpolated literals in a file carrying the comment.
fn b_mkstrf(vm: &mut VM, argc: u8) -> Value {
    let s = b_mkstr(vm, argc);
    with_host(|h| h.freeze_value(&s));
    s
}
fn b_mksym(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    with_host(|h| h.new_symbol(&name))
}
fn b_mkarray(vm: &mut VM, argc: u8) -> Value {
    let items = pop_n(vm, argc as usize);
    with_host(|h| h.new_array(items))
}
fn b_mkhash(vm: &mut VM, argc: u8) -> Value {
    let vals = pop_n(vm, argc as usize);
    with_host(|h| {
        let mut m = IndexMap::new();
        for pair in vals.chunks_exact(2) {
            let key = h.value_to_key(&pair[0]);
            m.insert(key, pair[1].clone());
        }
        h.new_hash(m)
    })
}
fn b_mkrange(vm: &mut VM, _: u8) -> Value {
    let excl = vm.pop();
    let hi = vm.pop();
    let lo = vm.pop();
    let excl = matches!(excl, Value::Bool(true));
    // `1..Float::INFINITY` is an endless integer range; `-Float::INFINITY..n`
    // is beginless. Map either infinite float bound to the range sentinel.
    let bound = |v: &Value, endless_sentinel: i64| -> Option<i64> {
        match v {
            Value::Int(n) => Some(*n),
            Value::Float(f) if f.is_infinite() => Some(endless_sentinel),
            _ => None,
        }
    };
    if let (Some(a), Some(b)) = (
        bound(&lo, crate::host::RANGE_BEGINLESS),
        bound(&hi, crate::host::RANGE_ENDLESS),
    ) {
        return with_host(|h| h.new_range(a, b, excl));
    }
    match (&lo, &hi) {
        (Value::Int(a), Value::Int(b)) => with_host(|h| h.new_range(*a, *b, excl)),
        // A Float endpoint (on either side) makes a Float range; a mixed
        // Int/Float range coerces the Integer bound to Float, matching Ruby.
        (Value::Float(_) | Value::Int(_), Value::Float(_) | Value::Int(_))
            if matches!(lo, Value::Float(_)) || matches!(hi, Value::Float(_)) =>
        {
            with_host(|h| h.new_float_range(as_f(&lo), as_f(&hi), excl))
        }
        _ => {
            // String endpoints (`'a'..'e'`) produce a String range.
            let ls = with_host(|h| h.as_str(&lo));
            let hs = with_host(|h| h.as_str(&hi));
            match (ls, hs) {
                (Some(a), Some(b)) => with_host(|h| h.new_str_range(a, b, excl)),
                _ => abort(vm, "bad value for range".into()),
            }
        }
    }
}
fn b_mkproc(vm: &mut VM, _: u8) -> Value {
    let id = match vm.pop() {
        Value::Int(n) => n as usize,
        _ => return abort(vm, "bad proc id".into()),
    };
    with_host(|h| h.new_proc(id))
}
fn b_mklambda(vm: &mut VM, _: u8) -> Value {
    let id = match vm.pop() {
        Value::Int(n) => n as usize,
        _ => return abort(vm, "bad proc id".into()),
    };
    with_host(|h| h.new_lambda(id))
}
fn b_mkregex(vm: &mut VM, _: u8) -> Value {
    let flags = name_of(&vm.pop());
    let source = name_of(&vm.pop());
    match with_host(|h| h.new_regex(&source, &flags)) {
        Ok(v) => v,
        Err(e) => abort(vm, e),
    }
}
fn b_yield(vm: &mut VM, argc: u8) -> Value {
    let args = pop_n(vm, argc as usize);
    let Some(block) = current_block() else {
        return abort(vm, "no block given (yield)".into());
    };
    match call_proc(&block, &args) {
        Ok(v) => propagate(vm, v),
        Err(e) => abort(vm, e),
    }
}
fn b_truthy(vm: &mut VM, _: u8) -> Value {
    let v = vm.pop();
    Value::Bool(with_host(|h| h.truthy(&v)))
}
fn b_tostr(vm: &mut VM, _: u8) -> Value {
    let v = vm.pop();
    with_host(|h| {
        let s = h.to_s(&v);
        h.new_string(s)
    })
}

// ---- indexing -------------------------------------------------------------

fn b_index_get(vm: &mut VM, argc: u8) -> Value {
    let mut vals = pop_n(vm, argc as usize);
    let recv = vals.remove(0);
    match dispatch(&recv, "[]", &vals, None) {
        Ok(v) => v,
        Err(e) => abort(vm, e),
    }
}
fn b_index_set(vm: &mut VM, argc: u8) -> Value {
    let mut vals = pop_n(vm, argc as usize);
    let recv = vals.remove(0);
    match dispatch(&recv, "[]=", &vals, None) {
        Ok(v) => v,
        Err(e) => abort(vm, e),
    }
}

// ---- control-flow signals -------------------------------------------------

fn b_sig_break(vm: &mut VM, _: u8) -> Value {
    let v = vm.pop();
    raise_signal_break(v);
    vm.ip = vm.chunk.ops.len();
    Value::Undef
}
fn b_sig_next(vm: &mut VM, _: u8) -> Value {
    let v = vm.pop();
    raise_signal_next(v);
    vm.ip = vm.chunk.ops.len();
    Value::Undef
}
fn b_sig_return(vm: &mut VM, _: u8) -> Value {
    let v = vm.pop();
    raise_signal_return(v);
    vm.ip = vm.chunk.ops.len();
    Value::Undef
}
fn b_sig_retry(vm: &mut VM, _: u8) -> Value {
    vm.pop(); // `retry` carries no value
    raise_signal_retry();
    vm.ip = vm.chunk.ops.len();
    Value::Undef
}

// ===========================================================================
// Dispatch
// ===========================================================================

/// A self/top-level call: a user method if defined, else a Kernel function.
fn dispatch_call(name: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    // Inline Rust FFI: the `rust { ... }` desugar emits `__rust_compile(b64,
    // line)`; compile + register the block's exported functions. The base64 body
    // is a Ruby String object, so read it through the host, not `Value::to_str`.
    if name == "__rust_compile" {
        let b64 = args
            .first()
            .and_then(|v| with_host(|h| h.as_str(v)))
            .unwrap_or_default();
        return fusevm::ffi::compile_and_register(&b64).map(|_| Value::Undef);
    }
    // Inside a class method `self` is a class ref — `new` and class methods
    // dispatch on the class; anything else (raise, puts, …) is a Kernel call.
    let this = with_host(|h| h.current_self());
    // `autoload :Const, "path"` — register a lazy require, namespaced to the
    // current `self` when it is a module (`I18n::Backend`), else top-level.
    // `b_getconst` fires the require on first reference of the constant.
    if name == "autoload" && args.len() >= 2 {
        let const_name = name_of(&args[0]);
        let path = with_host(|h| h.as_str(&args[1])).unwrap_or_default();
        let full = match with_host(|h| h.classref_name(&this)) {
            Some(cls) => format!("{cls}::{const_name}"),
            None => const_name,
        };
        with_host(|h| h.set_autoload(&full, &path));
        return Ok(Value::Undef);
    }
    if name == "autoload?" && !args.is_empty() {
        let const_name = name_of(&args[0]);
        let full = match with_host(|h| h.classref_name(&this)) {
            Some(cls) => format!("{cls}::{const_name}"),
            None => const_name,
        };
        return Ok(with_host(|h| match h.autoload_path(&full) {
            Some(p) => h.new_string(p),
            None => Value::Undef,
        }));
    }
    if let Some(cls) = with_host(|h| h.classref_name(&this)) {
        // `define_method(:name) { ... }` in a class body registers an instance
        // method whose body is the block.
        if name == "define_method" {
            let mname = name_of(&args[0]);
            let proc = block.or_else(|| args.get(1).cloned()).ok_or_else(|| {
                raise_exc("ArgumentError", "tried to create method without a block")
            })?;
            with_host(|h| h.add_define_method(&cls, &mname, proc));
            return Ok(with_host(|h| h.new_symbol(&mname)));
        }
        // `alias_method(:new, :old)` — register an alias on the class.
        if name == "alias_method" && args.len() >= 2 {
            let (new_name, old_name) = (name_of(&args[0]), name_of(&args[1]));
            with_host(|h| h.add_alias(&cls, &new_name, &old_name));
            return Ok(with_host(|h| h.new_symbol(&new_name)));
        }
        // A bare `include`/`prepend`/`extend M` in a class body (e.g. a
        // conditional `prepend M if cond`, which the compile-time class-body
        // handler doesn't extract) — dispatch on the class itself.
        if matches!(name, "include" | "prepend" | "extend") && !args.is_empty() {
            return dispatch(&this, name, args, block);
        }
        // A bare `class_eval`/`module_eval` (string or block) in a class/module
        // body — `self` is the class, so dispatch on it to reach the receiver
        // handler. i18n defines its delegators this way (`module_eval <<-STR`).
        if matches!(name, "class_eval" | "module_eval") {
            return dispatch(&this, name, args, block);
        }
        // Bare Module reflection/definition calls in a class/module body (`self`
        // is the class): route to the receiver handler. Gems call these
        // unqualified inside `class X … end` (activesupport: `method_defined?`).
        if matches!(
            name,
            "method_defined?"
                | "public_method_defined?"
                | "private_method_defined?"
                | "protected_method_defined?"
                | "instance_methods"
                | "public_instance_methods"
                | "private_instance_methods"
                | "instance_method"
                | "const_defined?"
                | "const_get"
                | "const_set"
                | "remove_const"
                | "class_variable_get"
                | "class_variable_set"
                | "class_variable_defined?"
                | "class_variables"
        ) && !args.is_empty()
        {
            return dispatch(&this, name, args, block);
        }
        // Bare universal object method in a class body (`self` is the class, an
        // object like any other): `instance_variable_defined?`, `respond_to?`,
        // `send`, `instance_variable_get/set`, … — dispatch on the class object.
        if is_universal_object_method(name) {
            return dispatch(&this, name, args, block);
        }
        // Bare `undef :m` (keyword), `undef_method :m`, `remove_method :m` in a
        // class body — drop the named instance method(s). `undef :sym` parses as
        // a plain call, so it arrives here (activesupport, HashWithIndifferentAccess).
        if matches!(name, "undef" | "undef_method" | "remove_method") && !args.is_empty() {
            for a in args {
                let m = name_of(a);
                with_host(|h| h.remove_instance_method(&cls, &m));
            }
            return Ok(Value::Undef);
        }
        // Runtime `attr`/`attr_accessor`/`attr_reader`/`attr_writer` (e.g. in a
        // `class_eval` / conditional class body) — register native accessors.
        // `attr :x` is a reader (the deprecated `attr :x, true` accessor form is
        // not modeled).
        if matches!(
            name,
            "attr" | "attr_accessor" | "attr_reader" | "attr_writer"
        ) {
            let reader = name != "attr_writer";
            let writer = matches!(name, "attr_accessor" | "attr_writer");
            for a in args {
                if matches!(a, Value::Bool(_)) {
                    continue; // the old `attr :x, true` boolean flag
                }
                let field = name_of(a);
                with_host(|h| h.add_attr(&cls, &field, reader, writer));
            }
            return Ok(Value::Undef);
        }
        // Visibility directives in a class/module body — rubylang does not enforce
        // visibility, so accept them as no-ops (gems use them constantly:
        // `private_constant :X`, `private :m`, `module_function :m`). Ruby returns
        // the single name argument, or nil.
        if matches!(
            name,
            "private"
                | "public"
                | "protected"
                | "module_function"
                | "private_constant"
                | "public_constant"
                | "private_class_method"
                | "public_class_method"
                | "deprecate_constant"
                | "ruby2_keywords"
        ) {
            return Ok(match args {
                [one] if name != "module_function" => one.clone(),
                _ => Value::Undef,
            });
        }
        // Route to class-receiver dispatch for the class's own class methods and
        // for instance methods it inherits as a `Class < Module` object (Rails
        // core-ext macros like `delegate`, called bare in a class body). Names
        // that are neither (`puts`, `raise`, …) fall through to Kernel.
        if name == "new"
            // Common Module/Class builtin reflection called bare in a class body
            // or method (`self` is the class): `name`/`to_s`/`inspect`. These
            // arrive as calls only when no local shadows them, so routing to the
            // class is safe. activesupport's autoload path derivation uses `name`.
            || matches!(
                name,
                "name"
                    | "to_s"
                    | "inspect"
                    | "singleton_class"
                    | "instance_methods"
                    | "public_instance_methods"
                    | "private_instance_methods"
                    | "class_variables"
                    | "constants"
                    | "ancestors"
                    | "superclass"
            )
            || with_host(|h| {
                h.find_class_method(&cls, name).is_some()
                    || h.find_class_define_method(&cls, name).is_some()
                    || h.find_method("Class", name).is_some()
                    || h.find_method("Module", name).is_some()
            })
        {
            return dispatch_classref(&cls, name, args, block);
        }
        // A top-level `def` is a private method of Object, so it is callable by
        // bareword from anywhere — including a class/module body, where `self` is
        // a class object (itself an Object). `DelegateClass(...)` used as a nested
        // class's superclass expression is exactly this case.
        if with_host(|h| h.has_method(name)) {
            return call_method(name, args, block);
        }
        return kernel(name, args, block);
    }
    // A per-object singleton method (`def obj.m`, `class << obj`) or a
    // `define_singleton_method` block on `self` — a bare self-call must reach it
    // through object dispatch (singletons are keyed by object id, not class).
    // This matters when `self` is rebound (`instance_eval`/`instance_exec`) to a
    // receiver carrying singleton methods, and for ordinary singleton-to-singleton
    // self-calls.
    if with_host(|h| {
        h.find_singleton_method(&this, name).is_some()
            || h.find_singleton_define_method(&this, name).is_some()
    }) {
        return dispatch(&this, name, args, block);
    }
    // A bare call inside an instance method is `self.name(...)`. Route to object
    // dispatch when `self` handles it via a `define_method` or a universal object
    // method (instance_variable_get/set, send, respond_to?, …); a plain user
    // method goes through `call_method`, and anything else is a Kernel call.
    if let Some(cls) = with_host(|h| h.object_class(&this)) {
        if with_host(|h| h.find_define_method(&cls, name)).is_some()
            || is_universal_object_method(name)
        {
            return dispatch(&this, name, args, block);
        }
        // A bare call to a Struct member accessor (`x` / `x=`) inside a struct
        // method must route through object dispatch to reach `struct_method`.
        if let Some((members, _)) = with_host(|h| h.struct_def(&cls)) {
            let member = name.strip_suffix('=').unwrap_or(name);
            if members.iter().any(|m| m == member) {
                return dispatch(&this, name, args, block);
            }
        }
    }
    // A bare call inside a method whose `self` is a builtin-typed value (String,
    // Array, Hash, Integer, …): dispatch its native/reopened methods FIRST, so
    // `transform_keys { … }` / `map(&:sym)` inside a reopened builtin method reach
    // the native method rather than a same-named global (mirrors the argless
    // path in `b_getlocal`). A genuine miss falls through to the global path.
    if with_host(|h| h.object_class(&this).is_none() && h.classref_name(&this).is_none()) {
        match dispatch(&this, name, args, block.clone()) {
            Ok(v) => return Ok(v),
            Err(e) if e.starts_with("undefined method") => {}
            Err(e) => return Err(e),
        }
    }
    if with_host(|h| h.responds_to(name)) {
        return call_method(name, args, block);
    }
    // A `rust { ... }` block's exported functions are callable by bareword.
    // User-defined Ruby methods still win (resolved above); the registry is only
    // consulted once nothing else claimed the name, and the membership check
    // keeps it off the hot path. Ruby String args are host `Value::Obj` handles,
    // so marshal them into native `fusevm::Value::Str` — fusevm's FFI reads
    // string args via `Value::to_str`, which on a bare heap handle would yield
    // `(obj:N)`; ints/floats are already native and pass through unchanged.
    if fusevm::ffi::is_registered(name) {
        let marshalled: Vec<Value> = args
            .iter()
            .map(|v| match with_host(|h| h.as_str(v)) {
                Some(s) => Value::str(s),
                None => v.clone(),
            })
            .collect();
        if let Some(r) = fusevm::ffi::try_call(name, &marshalled) {
            return r;
        }
    }
    kernel(name, args, block)
}

/// Object methods available on every value (so a bare self-call inside a method
/// resolves them before falling through to Kernel).
fn is_universal_object_method(name: &str) -> bool {
    matches!(
        name,
        "instance_variable_get"
            | "instance_variable_set"
            | "instance_variables"
            | "instance_variable_defined?"
            | "send"
            | "__send__"
            | "public_send"
            | "respond_to?"
            | "method"
            | "is_a?"
            | "kind_of?"
            | "instance_of?"
            | "tap"
            | "then"
            | "yield_self"
            | "itself"
            | "freeze"
            | "frozen?"
            | "dup"
            | "clone"
            | "methods"
            | "instance_eval"
            | "instance_exec"
            | "define_singleton_method"
    )
}

/// The `def` target for an `instance_eval`/`class_eval` on `recv`. For
/// `instance_eval` (`instance == true`) a class receiver takes class methods and
/// an object takes singletons; for `class_eval` a class receiver takes instance
/// methods.
fn eval_target(recv: &Value, instance: bool) -> crate::host::DefTarget {
    use crate::host::DefTarget;
    if let Some(cls) = with_host(|h| h.classref_name(recv)) {
        if instance {
            DefTarget::ClassMethod(cls)
        } else {
            DefTarget::Instance(cls)
        }
    } else if let Value::Obj(id) = recv {
        DefTarget::Singleton(*id)
    } else {
        DefTarget::None
    }
}

/// A receiver method call. Universal methods first, then per-class.
pub(crate) fn dispatch(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    // A per-object singleton method (`def obj.m`, `class << obj`) has the highest
    // priority — ahead of the class's own instance methods and the universal
    // fallbacks.
    if let Some(def) = with_host(|h| h.find_singleton_method(recv, name)) {
        return crate::host::call_singleton(recv.clone(), &def, name, args, block);
    }
    // A `define_singleton_method` block runs with `self` = the receiver.
    if let Some(proc) = with_host(|h| h.find_singleton_define_method(recv, name)) {
        return crate::host::call_proc_self(&proc, args, Some(recv));
    }
    // A user-defined method wins over the universal fallbacks (so a class can
    // override `to_s`, `==`, `inspect`, etc.). This also covers methods added to
    // a *builtin* class by reopening it (`class String; def pluralize; …`) —
    // Rails/activesupport core-ext defines hundreds this way — so use the value's
    // full class (`class_of`: "String"/"Array"/…) when it is not a user object.
    {
        let cls = with_host(|h| h.object_class(recv).unwrap_or_else(|| h.class_of(recv)));
        if with_host(|h| h.find_method_owner(&cls, name)).is_some() {
            return call_instance_method(recv.clone(), &cls, name, args, block);
        }
    }
    // Universal methods available on every object.
    match name {
        // Argless `to_s` is the universal default; `to_s(base)` etc. fall through
        // to the per-class dispatch (e.g. `Integer#to_s(2)`).
        "to_s" if args.is_empty() => {
            return Ok(with_host(|h| {
                let s = h.to_s(recv);
                h.new_string(s)
            }))
        }
        "inspect" => {
            return Ok(with_host(|h| {
                let s = h.inspect(recv);
                h.new_string(s)
            }))
        }
        "class" => {
            // `Object#class` returns the Class object (a class reference), not
            // its name — so `5.class == Integer`, `obj.class.name`, and
            // `p e.class` (prints the bare name) all behave like Ruby.
            return Ok(with_host(|h| {
                let name = h.class_of(recv).to_string();
                h.class_ref(&name)
            }));
        }
        "nil?" => return Ok(Value::Bool(matches!(recv, Value::Undef))),
        // `to_json` on any value (a user class that defines its own wins via the
        // find_method_owner check above). Ignores the optional generator-state arg.
        "to_json" => {
            return match with_host(|h| json_encode(h, recv)) {
                Ok(s) => Ok(new_str(s)),
                Err(e) => Err(raise_exc("JSON::GeneratorError", &e)),
            };
        }
        "==" => return Ok(Value::Bool(with_host(|h| h.eq_values(recv, &args[0])))),
        "!=" => return Ok(Value::Bool(!with_host(|h| h.eq_values(recv, &args[0])))),
        "===" => {
            // Case-equality: a Class matches instances, a Regexp matches a
            // string, a Range covers, else `==`.
            if let Some(cls) = with_host(|h| h.classref_name(recv)) {
                return Ok(Value::Bool(with_host(|h| h.is_a(&args[0], &cls))));
            }
            if let Some((re, _)) = with_host(|h| h.as_regex(recv)) {
                return Ok(Value::Bool(
                    re.is_match(&arg_str(&args[0])).unwrap_or(false),
                ));
            }
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(recv)) {
                let n = as_i(&args[0]);
                let end = if excl { hi } else { hi + 1 };
                return Ok(Value::Bool(n >= lo && n < end));
            }
            if let Some((lo, hi, excl)) = with_host(|h| h.as_float_range(recv)) {
                let x = as_f(&args[0]);
                return Ok(Value::Bool(if excl {
                    x >= lo && x < hi
                } else {
                    x >= lo && x <= hi
                }));
            }
            return Ok(Value::Bool(with_host(|h| h.eq_values(recv, &args[0]))));
        }
        "is_a?" | "kind_of?" => {
            let cls = name_of(&args[0]);
            return Ok(Value::Bool(with_host(|h| h.is_a(recv, &cls))));
        }
        "instance_of?" => {
            let cls = name_of(&args[0]);
            return Ok(Value::Bool(with_host(|h| h.class_of(recv)) == cls));
        }
        "itself" => return Ok(recv.clone()),
        // `freeze` records the receiver as frozen and returns it (Ruby freezes in
        // place; immutability is not enforced here but `frozen?` reports it).
        "freeze" => {
            with_host(|h| h.freeze_value(recv));
            return Ok(recv.clone());
        }
        // `equal?` is object identity: true only for the very same object (same
        // heap handle) or the same immediate value.
        "equal?" => {
            let same = match (recv, &args[0]) {
                (Value::Obj(i), Value::Obj(j)) => i == j,
                (Value::Int(i), Value::Int(j)) => i == j,
                (Value::Float(i), Value::Float(j)) => i == j,
                (Value::Bool(i), Value::Bool(j)) => i == j,
                (Value::Undef, Value::Undef) => true,
                _ => false,
            };
            return Ok(Value::Bool(same));
        }
        // `object_id` / `__id__` — a stable per-object identity integer. Ruby
        // guarantees `Integer#object_id == 2n+1`; nil/false/true use their fixed
        // immediate ids; every heap object gets a distinct even id from its
        // handle (never colliding with the odd integer ids or the immediates).
        "object_id" | "__id__" => {
            let id = match recv {
                Value::Int(n) => n.wrapping_mul(2).wrapping_add(1),
                Value::Undef => 4,
                Value::Bool(false) => 0,
                Value::Bool(true) => 20,
                Value::Obj(h) => (*h as i64 + 1).wrapping_mul(8),
                Value::Float(f) => (f.to_bits() as i64) | 1,
                _ => 8,
            };
            return Ok(Value::Int(id));
        }
        // `dup` makes a fresh shallow copy so mutating the copy does not leak back
        // to the original; the copy is never frozen. `clone` also shallow-copies
        // but preserves the frozen state of the original.
        "dup" => return Ok(with_host(|h| h.dup_value(recv))),
        "clone" => {
            return Ok(with_host(|h| {
                let copy = h.dup_value(recv);
                if h.is_frozen(recv) {
                    h.freeze_value(&copy);
                }
                copy
            }));
        }
        // Immediates (Integer/Float/true/false/nil) and Symbols are always frozen
        // in Ruby; a mutable reference type reports frozen only after `freeze`.
        // Method/constant visibility directives. rubylang does not enforce
        // visibility, so these are accepted as no-ops (used pervasively by gems:
        // `private_constant :X`, `private :m`, `module_function`). Ruby returns
        // the single name argument (or nil), which callers occasionally chain.
        "private"
        | "public"
        | "protected"
        | "module_function"
        | "private_constant"
        | "public_constant"
        | "private_class_method"
        | "public_class_method"
        | "private_constant?" => {
            return Ok(match args {
                [one] if name != "module_function" => one.clone(),
                _ => Value::Undef,
            });
        }
        "frozen?" => return Ok(Value::Bool(with_host(|h| h.is_frozen(recv)))),
        "instance_variable_get" => {
            let raw = name_of(&args[0]);
            let key = raw.strip_prefix('@').unwrap_or(&raw);
            return Ok(with_host(|h| h.ivar_of(recv, key)));
        }
        "instance_variable_set" => {
            // A frozen object rejects ivar mutation (MRI raises FrozenError).
            if with_host(|h| h.is_frozen(recv)) {
                let cls = with_host(|h| h.class_of(recv));
                return Err(raise_exc(
                    "FrozenError",
                    &format!("can't modify frozen {cls}"),
                ));
            }
            let raw = name_of(&args[0]);
            let key = raw.strip_prefix('@').unwrap_or(&raw).to_string();
            let val = args[1].clone();
            with_host(|h| h.set_ivar_of(recv, &key, val.clone()));
            return Ok(val);
        }
        "instance_variables" => {
            let names = with_host(|h| h.ivar_names(recv));
            return Ok(with_host(|h| {
                let syms: Vec<Value> = names.iter().map(|n| h.new_symbol(n)).collect();
                h.new_array(syms)
            }));
        }
        "instance_variable_defined?" => {
            // True when the named ivar has been assigned on the receiver.
            // `ivar_names` reports them with the leading `@`, so normalize the
            // query (accepting `:@x` and `"@x"`) to the same form.
            let raw = name_of(&args[0]);
            let key = format!("@{}", raw.strip_prefix('@').unwrap_or(&raw));
            let has = with_host(|h| h.ivar_names(recv).contains(&key));
            return Ok(Value::Bool(has));
        }
        "tap" => {
            if let Some(b) = &block {
                call_proc(b, std::slice::from_ref(recv))?;
            }
            return Ok(recv.clone());
        }
        "then" | "yield_self" => {
            if let Some(b) = &block {
                return call_proc(b, std::slice::from_ref(recv));
            }
            return Ok(recv.clone());
        }
        // `.lazy` wraps an enumerable in a lazy pipeline. A range (possibly
        // endless) stays a range source; anything else materializes to an array.
        "lazy" if with_host(|h| h.lazy_parts(recv)).is_none() => {
            let source = if with_host(|h| {
                h.as_range(recv).is_some()
                    || h.as_array(recv).is_some()
                    || h.generator_block(recv).is_some()
            }) {
                recv.clone()
            } else {
                new_arr(with_host(|h| h.as_array(recv)).unwrap_or_default())
            };
            return Ok(with_host(|h| h.new_lazy(source, vec![])));
        }
        // `Object#methods` — the receiver's instance methods (own + user-defined
        // ancestors) as symbols. Bounded to user-defined method surface; builtin
        // Kernel methods are not enumerated.
        "methods" if args.is_empty() => {
            if let Some(cls) = with_host(|h| h.object_class(recv)) {
                return Ok(with_host(|h| {
                    let names = h.instance_method_names(&cls, true);
                    let syms: Vec<Value> = names.iter().map(|n| h.new_symbol(n)).collect();
                    h.new_array(syms)
                }));
            }
            return Ok(new_arr(vec![]));
        }
        "send" | "__send__" | "public_send" => {
            let m = name_of(&args[0]);
            return dispatch(recv, &m, &args[1..], block);
        }
        // `obj.method(:name)` — capture a bound Method object.
        "method" => {
            let m = name_of(&args[0]);
            return Ok(with_host(|h| h.new_method(recv.clone(), &m)));
        }
        "respond_to?" => {
            // For a user object, a method responds if the class (or an included/
            // prepended module or superclass) defines it as a normal method, a
            // `define_method` block, or an alias; a per-object singleton method
            // also responds; or its `respond_to_missing?` returns true. Built-in
            // receivers stay permissive (their surface is not enumerable here).
            let m = name_of(&args[0]);
            if with_host(|h| h.find_singleton_method(recv, &m)).is_some()
                || with_host(|h| h.find_singleton_define_method(recv, &m)).is_some()
            {
                return Ok(Value::Bool(true));
            }
            if let Some(cls) = with_host(|h| h.object_class(recv)) {
                if with_host(|h| h.find_method_owner(&cls, &m)).is_some()
                    || with_host(|h| h.find_define_method(&cls, &m)).is_some()
                    || with_host(|h| h.find_alias(&cls, &m)).is_some()
                {
                    return Ok(Value::Bool(true));
                }
                if with_host(|h| h.find_method_owner(&cls, "respond_to_missing?")).is_some() {
                    let include_private = args.get(1).cloned().unwrap_or(Value::Bool(false));
                    let sym = with_host(|h| h.new_symbol(&m));
                    return call_instance_method(
                        recv.clone(),
                        &cls,
                        "respond_to_missing?",
                        &[sym, include_private],
                        None,
                    );
                }
                // Struct instances respond to the deconstruction protocol even
                // though those methods are Struct-provided, not user-defined; the
                // pattern-match gate depends on this being true.
                if with_host(|h| h.struct_def(&cls)).is_some()
                    && matches!(m.as_str(), "deconstruct" | "deconstruct_keys")
                {
                    return Ok(Value::Bool(true));
                }
                // OpenStruct responds to a current attribute's reader and writer,
                // plus the container methods. A writer for a not-yet-set field is
                // not reported (MRI only adds it once assigned).
                if cls == "OpenStruct" {
                    let field = m.trim_end_matches('=');
                    let has = with_host(|h| {
                        h.ivar_names(recv)
                            .iter()
                            .any(|n| n.trim_start_matches('@') == field)
                    });
                    let always = matches!(
                        m.as_str(),
                        "to_h" | "each_pair" | "each" | "[]" | "[]=" | "dig" | "members"
                    );
                    return Ok(Value::Bool(has || always));
                }
                return Ok(Value::Bool(false));
            }
            // A class/module receiver reports accurately: sinatra's `set` DSL
            // branches on `respond_to?("opt=")` and must see a setter as absent
            // until it is defined. Class methods, a `define_singleton_method`, a
            // singleton-class instance method, and the reflection surface count;
            // `respond_to_missing?` on the singleton class is the final say.
            if let Some(cname) = with_host(|h| h.classref_name(recv)) {
                if with_host(|h| h.class_responds_to(&cname, &m)) {
                    return Ok(Value::Bool(true));
                }
                let sclass = format!("#<Class:{cname}>");
                if with_host(|h| h.find_method_owner(&sclass, "respond_to_missing?")).is_some() {
                    let include_private = args.get(1).cloned().unwrap_or(Value::Bool(false));
                    let sym = with_host(|h| h.new_symbol(&m));
                    return call_instance_method(
                        recv.clone(),
                        &sclass,
                        "respond_to_missing?",
                        &[sym, include_private],
                        None,
                    );
                }
                // A builtin class (Dir, File, …) has native class methods that are
                // not enumerable here, so stay permissive; a *user* class has all
                // its class methods registered, so report an undefined one as absent
                // (sinatra's `set` needs `respond_to?("opt=")` false until defined).
                return Ok(Value::Bool(with_host(|h| h.is_builtin_class(&cname))));
            }
            // Built-in receivers are otherwise permissive, but the pattern-match
            // deconstruction protocol must be accurate: only Arrays respond to
            // `deconstruct`, only Hashes to `deconstruct_keys`. Reporting `true`
            // for e.g. an Integer would make `case 5; in [a, b]` call a missing
            // method instead of falling through to the next clause.
            match name_of(&args[0]).as_str() {
                "deconstruct" => return Ok(Value::Bool(with_host(|h| h.is_a(recv, "Array")))),
                "deconstruct_keys" => return Ok(Value::Bool(with_host(|h| h.is_a(recv, "Hash")))),
                _ => {}
            }
            return Ok(Value::Bool(true));
        }
        // `obj.define_singleton_method(:name) { body }` — a per-object method from
        // the block (or a Method/Proc arg), keyed by the receiver's heap id.
        "define_singleton_method" if matches!(recv, Value::Obj(_)) => {
            let mname = name_of(&args[0]);
            let proc = block
                .clone()
                .or_else(|| args.get(1).cloned())
                .ok_or_else(|| {
                    raise_exc("ArgumentError", "tried to create Proc without a block")
                })?;
            // On a class/module object a singleton method is a *class method*,
            // inherited by subclasses — register it under the class name rather
            // than the classref value's heap id (which is recreated per reference,
            // so an id-keyed singleton would be lost on the next `Klass.m` call).
            if let Some(cls) = with_host(|h| h.classref_name(recv)) {
                with_host(|h| h.add_class_define_method(&cls, &mname, proc));
                return Ok(with_host(|h| h.new_symbol(&mname)));
            }
            if let Value::Obj(id) = recv {
                with_host(|h| h.add_singleton_define_method(*id, &mname, proc));
            }
            return Ok(with_host(|h| h.new_symbol(&mname)));
        }
        // `obj.extend(M, …)` — mix each module's instance methods into the
        // receiver's singleton table (MRI returns the receiver).
        "extend" if matches!(recv, Value::Obj(_)) => {
            if let Value::Obj(id) = recv {
                for a in args {
                    if let Some(mname) = with_host(|h| h.classref_name(a)) {
                        with_host(|h| h.extend_object(*id, &mname));
                    }
                }
            }
            return Ok(recv.clone());
        }
        // `instance_eval`/`instance_exec` run with `self` = receiver: a bare `def`
        // defines a singleton on the receiver (or a class method when the receiver
        // is a class), and `@ivar` accesses the receiver's instance variables.
        "instance_eval" | "instance_exec" => {
            let target = eval_target(recv, true);
            if let Some(b) = block {
                // instance_eval yields the receiver; instance_exec passes `args`.
                let block_args: Vec<Value> = if name == "instance_exec" {
                    args.to_vec()
                } else {
                    vec![recv.clone()]
                };
                return crate::host::eval_block_scoped(&b, recv, target, &block_args);
            }
            // String form: `instance_eval("code")`.
            let src = arg_str(&args[0]);
            return crate::host::eval_string_scoped(&src, recv, target);
        }
        // `class_eval`/`module_eval` run with `self` = the class: a bare `def`
        // defines an instance method on it.
        "class_eval" | "module_eval" if with_host(|h| h.classref_name(recv)).is_some() => {
            let target = eval_target(recv, false);
            if let Some(b) = block {
                return crate::host::eval_block_scoped(
                    &b,
                    recv,
                    target,
                    std::slice::from_ref(recv),
                );
            }
            let src = arg_str(&args[0]);
            return crate::host::eval_string_scoped(&src, recv, target);
        }
        _ => {}
    }

    // A class reference (`Foo.new`, `Foo.name`) or a user object.
    if let Some(cls) = with_host(|h| h.classref_name(recv)) {
        return dispatch_classref(&cls, name, args, block);
    }
    if let Some(cls) = with_host(|h| h.object_class(recv)) {
        return dispatch_object(recv, &cls, name, args, block);
    }

    let class = with_host(|h| h.class_of(recv));
    let fallback_block = block.clone();
    let result = match class.as_str() {
        "Integer" | "Float" => dispatch_number(recv, name, args, block),
        "String" => dispatch_string(recv, name, args, block),
        "Array" => dispatch_array(recv, name, args, block),
        "Hash" => dispatch_hash(recv, name, args, block),
        "Range" => dispatch_range(recv, name, args, block),
        "Symbol" => dispatch_symbol(recv, name, args),
        "Proc" => dispatch_proc(recv, name, args),
        "Method" => dispatch_method(recv, name, args, block),
        "Regexp" => dispatch_regexp(recv, name, args),
        "MatchData" => dispatch_matchdata(recv, name, args),
        "Set" => dispatch_set(recv, name, args, block),
        "Time" => dispatch_time(recv, name, args),
        "Date" => dispatch_date(recv, name, args),
        "DateTime" => dispatch_datetime(recv, name, args),
        "Enumerator" => dispatch_enumerator(recv, name, args, block),
        "Fiber" => dispatch_fiber(recv, name, args),
        "Thread" => dispatch_thread(recv, name, args),
        "IO" | "File" => dispatch_io(recv, name, args, block),
        "TCPServer" => dispatch_tcp_server(recv, name, args, block),
        "TCPSocket" => dispatch_tcp_socket(recv, name, args, block),
        "SQLite3::Database" => dispatch_sqlite_db(recv, name, args, block),
        "Fiddle::Handle" => dispatch_fiddle_handle(recv, name, args),
        "Fiddle::Function" => dispatch_fiddle_function(recv, name, args),
        "Fiddle::Pointer" => dispatch_fiddle_pointer(recv, name, args),
        "Enumerator::Lazy" => dispatch_lazy(recv, name, args, block),
        "Enumerator::Yielder" => dispatch_yielder(recv, name, args),
        "Rational" => dispatch_rational(recv, name, args),
        "Complex" => dispatch_complex(recv, name, args),
        "TrueClass" | "FalseClass" | "NilClass" => dispatch_bool(recv, name, args),
        _ => Err(no_method_error(recv, name)),
    };
    // Builtin values inherit from `Object`. When native dispatch above does not
    // handle the method, fall back to a *user-defined* method on Object (or a
    // module included into Object). activesupport core-ext defines Object#deep_dup,
    // #try, #presence, #in?, #then, etc. this way — they must be callable on
    // Integers/Strings/Arrays/Hashes too. This runs *after* native dispatch so a
    // native method (e.g. Array#length) always wins over a same-named Object method.
    if let Err(ref e) = result {
        if e.starts_with("undefined method")
            && class != "Object"
            && with_host(|h| h.find_method_owner("Object", name)).is_some()
        {
            return call_instance_method(recv.clone(), "Object", name, args, fallback_block);
        }
    }
    result
}

/// `true`/`false`/`nil` boolean logic operators (`&` `|` `^` `!`). Ruby treats
/// the argument by truthiness (only `nil`/`false` are falsy).
fn dispatch_bool(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let this = with_host(|h| h.truthy(recv));
    let arg = || with_host(|h| h.truthy(&args[0]));
    match name {
        "&" => Ok(Value::Bool(this && arg())),
        "|" => Ok(Value::Bool(this || arg())),
        "^" => Ok(Value::Bool(this != arg())),
        "!" => Ok(Value::Bool(!this)),
        "to_s" | "inspect" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        // `nil.to_a` is `[]`; `nil.to_h` is `{}` — the empty-conversion methods.
        "to_a" if matches!(recv, Value::Undef) => Ok(new_arr(vec![])),
        "to_h" if matches!(recv, Value::Undef) => Ok(with_host(|h| h.new_hash(IndexMap::new()))),
        _ => Err(no_method_error(recv, name)),
    }
}

/// Methods on a class reference: `new` (allocate + `initialize`), `name`.
fn dispatch_classref(
    cls: &str,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    // A user-defined class method takes precedence over the native builtin
    // handlers below: a reopened builtin's `def self.m`, and the synthetic
    // `__class_body__` that runs a reopened module/class body. Without this,
    // reopening a builtin module (`module SecureRandom; <non-def statement>; end`)
    // fails because its `__class_body__` call hits the builtin dispatch.
    if let Some((def, owner)) = with_host(|h| h.find_class_method_owner(cls, name)) {
        let recv = with_host(|h| h.class_ref(cls));
        return crate::host::call_class_method(recv, &def, name, &owner, args, block);
    }
    // A `Klass.define_singleton_method(:m) { … }` class method (proc-backed,
    // inherited by subclasses). Runs with `self` = the class ref.
    if let Some(proc) = with_host(|h| h.find_class_define_method(cls, name)) {
        let recv = with_host(|h| h.class_ref(cls));
        return crate::host::call_proc_self(&proc, args, Some(&recv));
    }
    // A method defined on the class's *singleton class* is a class method:
    // `Klass.singleton_class.class_eval { define_method(:m) { … } }` /
    // `{ def m; … end }` (sinatra's `set` DSL defines its accessors this way).
    // The singleton class is registered under the synthetic name `#<Class:Klass>`;
    // its instance methods run with `self` = the class ref.
    {
        let sclass = format!("#<Class:{cls}>");
        if let Some(proc) = with_host(|h| h.find_define_method(&sclass, name)) {
            let recv = with_host(|h| h.class_ref(cls));
            return crate::host::call_proc_self(&proc, args, Some(&recv));
        }
        if let Some((_, owner)) = with_host(|h| h.find_method_owner(&sclass, name)) {
            let recv = with_host(|h| h.class_ref(cls));
            return call_instance_method(recv, &owner, name, args, block);
        }
    }
    // `Mod.autoload :Const, "path"` — a module registering a lazy require on a
    // *receiver* (`Tilt.autoload :Foo, "tilt/foo"`), namespaced under the module.
    // The self-call and `super`-override forms are handled elsewhere; this is the
    // explicit-receiver form.
    if name == "autoload" && args.len() >= 2 {
        let const_name = name_of(&args[0]);
        let path = with_host(|h| h.as_str(&args[1])).unwrap_or_default();
        let full = format!("{cls}::{const_name}");
        with_host(|h| h.set_autoload(&full, &path));
        return Ok(Value::Undef);
    }
    if name == "autoload?" && !args.is_empty() {
        let const_name = name_of(&args[0]);
        let full = format!("{cls}::{const_name}");
        return Ok(with_host(|h| match h.autoload_path(&full) {
            Some(p) => h.new_string(p),
            None => Value::Undef,
        }));
    }
    // `ENV` — the process environment as a hash-like object (backed by std::env).
    if cls == "ENV" {
        return dispatch_env(name, args, block);
    }
    // `GC` — rubylang has no user-visible GC to drive, so the control surface is
    // accepted as no-ops returning MRI-shaped values.
    if cls == "GC" {
        return match name {
            "start"
            | "garbage_collect"
            | "compact"
            | "verify_compaction_references"
            | "verify_internal_consistency" => Ok(Value::Undef),
            "enable" | "disable" | "stress" => Ok(Value::Bool(false)),
            "count" | "total_time" => Ok(Value::Int(0)),
            "stat" | "latest_gc_info" => Ok(with_host(|h| h.new_hash(IndexMap::new()))),
            _ => Err(raise_exc(
                "NoMethodError",
                &format!("undefined method '{name}' for GC:Module"),
            )),
        };
    }
    // `ObjectSpace` — the heap is not enumerable by class here, so the
    // enumeration surface returns 0/empty rather than pretending to walk objects.
    if cls == "ObjectSpace" {
        match name {
            "garbage_collect" => return Ok(Value::Undef),
            "count_objects" => return Ok(with_host(|h| h.new_hash(IndexMap::new()))),
            "each_object" => return Ok(Value::Int(0)),
            // A capitalized name (`ObjectSpace::WeakMap`) is a nested constant /
            // class — fall through to the constant/nested-class resolution below.
            _ if name.chars().next().is_some_and(|c| c.is_uppercase()) => {}
            _ => {
                return Err(raise_exc(
                    "NoMethodError",
                    &format!("undefined method '{name}' for ObjectSpace:Module"),
                ))
            }
        }
    }
    // `Mutex.new` / `Thread::Mutex.new` / `Monitor.new` — a lock object. Under the
    // GVL a critical section with no blocking call is already exclusive, so the
    // lock is a `locked` flag on the object (see `mutex_method`).
    if (cls == "Mutex" || cls == "Thread::Mutex" || cls == "Monitor") && name == "new" {
        return Ok(with_host(|h| {
            let o = h.new_object(cls);
            h.set_ivar_of(&o, "__locked", Value::Bool(false));
            o
        }));
    }
    // `Queue.new` / `SizedQueue.new(cap)` — a thread-safe FIFO. The blocking core
    // lives in a side table keyed by the object's `__qid` ivar (see `queue_method`).
    if matches!(
        cls,
        "Queue" | "Thread::Queue" | "SizedQueue" | "Thread::SizedQueue"
    ) && name == "new"
    {
        let cap = if cls.ends_with("SizedQueue") {
            Some(args.first().map(as_i).unwrap_or(0).max(0) as usize)
        } else {
            None
        };
        let qid = crate::host::new_queue(cap);
        return Ok(with_host(|h| {
            let o = h.new_object(if cls.ends_with("SizedQueue") {
                "SizedQueue"
            } else {
                "Queue"
            });
            h.set_ivar_of(&o, "__qid", Value::Int(qid as i64));
            o
        }));
    }
    // `ConditionVariable.new`.
    if matches!(cls, "ConditionVariable" | "Thread::ConditionVariable") && name == "new" {
        let cvid = crate::host::new_condvar();
        return Ok(with_host(|h| {
            let o = h.new_object("ConditionVariable");
            h.set_ivar_of(&o, "__cvid", Value::Int(cvid as i64));
            o
        }));
    }
    // `Thread` class methods. `Thread.new { }` spawns a real OS thread that runs
    // under the GVL (only one Ruby thread executes at a time, like MRI).
    if cls == "Thread" {
        return match name {
            "new" | "start" | "fork" => {
                let b =
                    block.ok_or_else(|| raise_exc("ThreadError", "must be called with a block"))?;
                Ok(crate::host::spawn_thread(b))
            }
            "current" | "main" => Ok(crate::host::current_thread()),
            // Cooperative-scheduler hints; the GVL already serializes execution.
            "pass" => Ok(Value::Undef),
            "list" => Ok(new_arr(vec![crate::host::current_thread()])),
            _ => Err(raise_exc(
                "NoMethodError",
                &format!("undefined method '{name}' for Thread"),
            )),
        };
    }
    // `Random` class methods; `Random.new(seed)` builds an object with its own
    // reproducible PRNG stream (see `random_advance`).
    if cls == "Random" {
        return match name {
            "new" => Ok(with_host(|h| {
                let seed = args.first().and_then(int_arg).unwrap_or(0x9E3779B9);
                let o = h.new_object("Random");
                h.set_ivar_of(&o, "state", Value::Int(seed));
                h.set_ivar_of(&o, "seed", Value::Int(seed));
                o
            })),
            "rand" => Ok(kernel_rand(args)),
            "srand" => {
                let seed = args.first().and_then(int_arg).unwrap_or(0);
                Ok(Value::Int(rng_srand(seed)))
            }
            "new_seed" => Ok(Value::Int((rng_next() >> 1) as i64)),
            _ => Err(raise_exc(
                "NoMethodError",
                &format!("undefined method '{name}' for Random"),
            )),
        };
    }
    // `StringIO.new(initial="")` — a String-backed IO. The buffer and read cursor
    // live in the `buf`/`pos` ivars (see `stringio_method`).
    if cls == "StringIO" && name == "new" {
        let init = args.first().map(arg_str).unwrap_or_default();
        return Ok(with_host(|h| {
            let obj = h.new_object("StringIO");
            let sv = h.new_string(init);
            h.set_ivar_of(&obj, "buf", sv);
            h.set_ivar_of(&obj, "pos", Value::Int(0));
            obj
        }));
    }
    // `Struct.new(:a, :b [, keyword_init: true])` defines a new struct class and
    // returns a reference to it (usually assigned to a constant).
    if cls == "Struct" && name == "new" {
        let mut members = Vec::new();
        let mut keyword_init = false;
        for a in args {
            if let Some(sym) = with_host(|h| h.as_symbol(a)) {
                members.push(sym);
            } else if let Some(kw) = with_host(|h| h.as_hash(a)) {
                keyword_init = kw
                    .get(&RKey::Sym("keyword_init".into()))
                    .map(|v| with_host(|h| h.truthy(v)))
                    .unwrap_or(false);
            }
        }
        let struct_name = with_host(|h| h.define_struct(members, keyword_init));
        let cref = with_host(|h| h.class_ref(&struct_name));
        // `Struct.new(:a) do ... end` — the block body defines instance methods on
        // the new struct class (run as a `class_eval`).
        if let Some(bl) = block {
            crate::host::eval_block_scoped(
                &bl,
                &cref,
                crate::host::DefTarget::Instance(struct_name),
                std::slice::from_ref(&cref),
            )?;
        }
        return Ok(cref);
    }
    // `Data.define(:x, :y)` — an immutable value class. Reuses struct storage.
    if cls == "Data" && name == "define" {
        let mut members = Vec::new();
        for a in args {
            if let Some(sym) = with_host(|h| h.as_symbol(a)) {
                members.push(sym);
            }
        }
        let data_name = with_host(|h| h.define_data(members));
        let cref = with_host(|h| h.class_ref(&data_name));
        // `Data.define(:x) do ... end` — the block defines instance methods.
        if let Some(bl) = block {
            crate::host::eval_block_scoped(
                &bl,
                &cref,
                crate::host::DefTarget::Instance(data_name),
                std::slice::from_ref(&cref),
            )?;
        }
        return Ok(cref);
    }
    // Instantiating a struct class: bind positional args (or keyword args) to the
    // member instance variables.
    if let Some((members, keyword_init)) = with_host(|h| h.struct_def(cls)) {
        // A `Data.define`d class accepts positional *or* keyword args and yields a
        // frozen instance; a keyword-only hash is detected as the sole argument.
        if with_host(|h| h.is_data_class(cls)) && (name == "new" || name == "[]") {
            let obj = with_host(|h| h.new_object(cls));
            let kw = match args.first() {
                Some(a) if args.len() == 1 => with_host(|h| h.as_hash(a)),
                _ => None,
            };
            match kw {
                // Keyword form: `D.new(x: 1, y: 2)`. Missing members read as nil.
                Some(k) => {
                    for m in &members {
                        let v = k
                            .get(&RKey::Sym(m.clone()))
                            .cloned()
                            .unwrap_or(Value::Undef);
                        with_host(|h| h.set_ivar_of(&obj, m, v));
                    }
                }
                // Positional form: `D.new(1, 2)`.
                None => {
                    for (i, m) in members.iter().enumerate() {
                        let v = args.get(i).cloned().unwrap_or(Value::Undef);
                        with_host(|h| h.set_ivar_of(&obj, m, v));
                    }
                }
            }
            with_host(|h| h.freeze_value(&obj));
            return Ok(obj);
        }
        if name == "new" || name == "[]" {
            let obj = with_host(|h| h.new_object(cls));
            if keyword_init {
                let kw = args.first().and_then(|a| with_host(|h| h.as_hash(a)));
                for m in &members {
                    let v = kw
                        .as_ref()
                        .and_then(|k| k.get(&RKey::Sym(m.clone())).cloned())
                        .unwrap_or(Value::Undef);
                    with_host(|h| h.set_ivar_of(&obj, m, v));
                }
            } else {
                for (i, m) in members.iter().enumerate() {
                    let v = args.get(i).cloned().unwrap_or(Value::Undef);
                    with_host(|h| h.set_ivar_of(&obj, m, v));
                }
            }
            return Ok(obj);
        }
        if name == "members" {
            let syms: Vec<Value> = members
                .iter()
                .map(|m| with_host(|h| h.new_symbol(m)))
                .collect();
            return Ok(new_arr(syms));
        }
    }
    // `Enumerator.new { |y| ... }` — a block-based generator. The block drives
    // the enumerator by sending `<<`/`yield` to the yielder it receives.
    if cls == "Enumerator" && name == "new" {
        let b = block.ok_or_else(|| {
            raise_exc(
                "ArgumentError",
                "tried to create Enumerator without a block",
            )
        })?;
        return Ok(with_host(|h| h.new_generator(b)));
    }
    // `Fiber.new { |first| ... }` — a stackful coroutine. `Fiber.yield(v)`
    // suspends the currently running fiber.
    if cls == "Fiber" {
        match name {
            "new" => {
                let b = block.ok_or_else(|| {
                    raise_exc("ArgumentError", "tried to create a Fiber without a block")
                })?;
                return Ok(crate::host::new_fiber(b));
            }
            "yield" => {
                let v = match args {
                    [] => Value::Undef,
                    [one] => one.clone(),
                    many => new_arr(many.to_vec()),
                };
                return crate::host::fiber_yield(v);
            }
            // `Fiber.current` — the running fiber (a stable object; i18n stores
            // it as a config owner and compares identity).
            "current" => return Ok(crate::host::current_fiber()),
            // `Fiber[:key]` / `Fiber[:key] = value` — fiber-scoped storage
            // (Ruby 3.2+). Backed by one process-global hash: correct for the
            // common single-fiber use (e.g. i18n config); per-fiber isolation is
            // not modeled.
            "[]" => {
                let store = with_host(fiber_store);
                return dispatch(&store, "[]", args, None);
            }
            "[]=" => {
                let store = with_host(fiber_store);
                return dispatch(&store, "[]=", args, None);
            }
            _ => {}
        }
    }
    // `Timeout.timeout(sec, klass = nil, msg = nil) { … }` — runs the block and
    // returns its value. Wall-clock enforcement (interrupting a running block via
    // a watcher thread) is not modeled; the common defensive use — where the
    // block completes well within the limit — works. `sec == nil`/`0` disables it.
    if cls == "Timeout" && name == "timeout" {
        return match block {
            Some(b) => {
                let arg = args.first().cloned().unwrap_or(Value::Undef);
                call_proc(&b, std::slice::from_ref(&arg))
            }
            None => Ok(Value::Undef),
        };
    }
    // `CGI` module methods used by activesupport's `to_query`/`to_param` and by
    // gems (faraday): URL and HTML escaping.
    if cls == "CGI" {
        match name {
            "escape" => return Ok(new_str(cgi_escape(&arg_str(&args[0])))),
            "unescape" => return Ok(new_str(cgi_unescape(&arg_str(&args[0])))),
            "escapeHTML" | "escape_html" => {
                return Ok(new_str(cgi_escape_html(&arg_str(&args[0]))))
            }
            "unescapeHTML" | "unescape_html" => {
                return Ok(new_str(cgi_unescape_html(&arg_str(&args[0]))))
            }
            "escapeURIComponent" | "escapeURI" => {
                return Ok(new_str(cgi_escape(&arg_str(&args[0])).replace('+', "%20")))
            }
            _ => {}
        }
    }
    // `Etc` (from `require "etc"`, a C-ext stdlib) — only the CPU-count surface
    // gems actually use for pool sizing.
    // `Encoding::UTF_8` / `::ASCII_8BIT` / … — the encoding constants. We carry
    // only UTF-8 semantically, but name each object as requested so identity/name
    // checks pass.
    if cls == "Encoding" {
        let enc_name = match name {
            "UTF_8" => Some("UTF-8"),
            "ASCII_8BIT" | "BINARY" => Some("ASCII-8BIT"),
            "US_ASCII" | "ASCII" => Some("US-ASCII"),
            "UTF_16" => Some("UTF-16"),
            "UTF_16LE" => Some("UTF-16LE"),
            "UTF_16BE" => Some("UTF-16BE"),
            "GB18030" => Some("GB18030"),
            "GBK" => Some("GBK"),
            "SHIFT_JIS" | "SJIS" => Some("Shift_JIS"),
            "EUC_JP" => Some("EUC-JP"),
            "ISO_2022_JP" => Some("ISO-2022-JP"),
            "ISO_8859_1" | "ISO8859_1" => Some("ISO-8859-1"),
            "Windows_1252" | "WINDOWS_1252" => Some("Windows-1252"),
            _ => None,
        };
        if let Some(en) = enc_name {
            return Ok(encoding_object(en));
        }
        if name == "default_external" || name == "default_internal" {
            return Ok(encoding_object("UTF-8"));
        }
    }
    if cls == "Etc" {
        match name {
            "nprocessors" => {
                let n = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1);
                return Ok(Value::Int(n as i64));
            }
            "sysconfdir" => return Ok(new_str("/etc".to_string())),
            "systmpdir" => return Ok(new_str("/tmp".to_string())),
            _ => {}
        }
    }
    // `Ractor` — rubylang has no real Ractors, but gems (concurrent-ruby) probe
    // `Ractor.shareable?`/`make_shareable` unconditionally on Ruby 3+. Model just
    // the shareability surface: immutable/frozen values are shareable.
    if cls == "Ractor" {
        match name {
            "shareable?" => {
                let v = &args[0];
                let sharable = matches!(
                    v,
                    Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Undef
                ) || with_host(|h| h.is_frozen(v));
                return Ok(Value::Bool(sharable));
            }
            "make_shareable" => {
                with_host(|h| h.freeze_value(&args[0]));
                return Ok(args[0].clone());
            }
            "current" | "main" => return Ok(Value::Undef),
            _ => {}
        }
    }
    // `Regexp` class methods: build/escape/union, and the last-match accessor.
    if cls == "Regexp" {
        match name {
            "new" | "compile" => {
                let src = match with_host(|h| h.as_regex(&args[0])) {
                    Some((_, s)) => s,
                    None => arg_str(&args[0]),
                };
                // A truthy second arg means case-insensitive (Regexp::IGNORECASE
                // is 1); the full option bitset is not modeled.
                let flags = match args.get(1) {
                    Some(Value::Bool(true)) | Some(Value::Int(_)) => "i",
                    _ => "",
                };
                return with_host(|h| h.new_regex(&src, flags));
            }
            "escape" | "quote" => return Ok(new_str(regex_escape(&arg_str(&args[0])))),
            "union" => {
                // Flatten a single array arg; each element contributes its source
                // (a String is escaped, a Regexp uses its pattern), joined by `|`.
                let items: Vec<Value> = match args {
                    [one] => with_host(|h| h.as_array(one)).unwrap_or_else(|| vec![one.clone()]),
                    many => many.to_vec(),
                };
                let parts: Vec<String> = items
                    .iter()
                    .map(|v| match with_host(|h| h.as_regex(v)) {
                        Some((_, s)) => format!("(?-mix:{s})"),
                        None => regex_escape(&arg_str(v)),
                    })
                    .collect();
                return with_host(|h| h.new_regex(&parts.join("|"), ""));
            }
            "last_match" => {
                let m = with_host(|h| h.get_global("~"));
                return match args.first() {
                    Some(idx) => dispatch(&m, "[]", std::slice::from_ref(idx), None),
                    None => Ok(m),
                };
            }
            _ => {}
        }
    }
    // `Set.new(enum)` / `Set[a, b, c]` — a deduplicated collection.
    if cls == "Set" {
        match name {
            "new" => {
                let items = match args.first() {
                    Some(v) => with_host(|h| h.as_array(v))
                        .or_else(|| with_host(|h| h.as_set(v)))
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                return Ok(with_host(|h| h.new_set(items)));
            }
            "[]" => return Ok(with_host(|h| h.new_set(args.to_vec()))),
            _ => {}
        }
    }
    // `OpenStruct.new(field: val, ...)` — a struct whose attributes are set from
    // the (optional) initial hash and grow dynamically. State lives in ivars.
    if cls == "OpenStruct" && name == "new" {
        let obj = with_host(|h| h.new_object("OpenStruct"));
        if let Some(map) = args.first().and_then(|a| with_host(|h| h.as_hash(a))) {
            for (k, v) in map {
                let kv = with_host(|h| h.key_value(&k));
                let field = name_of(&kv);
                with_host(|h| h.set_ivar_of(&obj, &field, v.clone()));
            }
        }
        return Ok(obj);
    }
    // `Time` constructors. All produce a UTC time (the local-timezone offset is
    // not modeled). `Time.at(n)` wraps epoch seconds; `Time.utc`/`Time.gm`
    // build from broken-down UTC fields; `Time.now` reads the system clock.
    if cls == "Time" {
        match name {
            "at" => {
                let secs = args.first().map(as_f).unwrap_or(0.0);
                return Ok(with_host(|h| h.new_time(secs)));
            }
            "utc" | "gm" | "new" | "local" | "mktime" => {
                // Time.utc(year, month=1, day=1, hour=0, min=0, sec=0).
                let g = |i: usize, dflt: i64| args.get(i).map(as_i).unwrap_or(dflt);
                let (y, mo, d) = (g(0, 1970), g(1, 1), g(2, 1));
                let (hh, mi) = (g(3, 0), g(4, 0));
                let ss = args.get(5).map(as_f).unwrap_or(0.0);
                // `Time.new` with no args is `Time.now`; with args it builds a date.
                if name == "new" && args.is_empty() {
                    return Ok(with_host(|h| h.new_time(now_epoch_secs())));
                }
                let days = crate::host::days_from_civil(y, mo, d);
                let secs = days as f64 * 86_400.0 + (hh * 3600 + mi * 60) as f64 + ss;
                return Ok(with_host(|h| h.new_time(secs)));
            }
            "now" => return Ok(with_host(|h| h.new_time(now_epoch_secs()))),
            _ => {}
        }
    }
    // `Date` constructors (proleptic Gregorian). `Date.new(y, m, d)`,
    // `Date.today` (from the system clock), `Date.parse(str)` for ISO dates,
    // and `Date.jd(n)` from a Julian Day Number.
    if cls == "Date" {
        match name {
            "new" | "civil" => {
                let g = |i: usize, dflt: i64| args.get(i).map(as_i).unwrap_or(dflt);
                let (y, mo, d) = (g(0, -4712), g(1, 1), g(2, 1));
                return Ok(with_host(|h| {
                    let days = crate::host::days_from_civil(y, mo, d);
                    h.new_date(days)
                }));
            }
            "today" => {
                let days = (now_epoch_secs() / 86_400.0).floor() as i64;
                return Ok(with_host(|h| h.new_date(days)));
            }
            "jd" => {
                let jd = args
                    .first()
                    .map(as_i)
                    .unwrap_or(crate::host::UNIX_EPOCH_JDN);
                return Ok(with_host(|h| h.new_date(jd - crate::host::UNIX_EPOCH_JDN)));
            }
            "parse" => {
                let s = with_host(|h| h.as_str(&args[0]).unwrap_or_default());
                return match parse_iso_date(&s) {
                    Some(days) => Ok(with_host(|h| h.new_date(days))),
                    None => Err(raise_exc("ArgumentError", "invalid date")),
                };
            }
            _ => {}
        }
    }
    // `DateTime` constructors (proleptic Gregorian, UTC-only). Same calendar as
    // `Date`/`Time`, but carrying a time of day. `DateTime.parse` accepts an
    // ISO8601 `YYYY-MM-DDTHH:MM:SS` (optional time/zone), like `Date.parse`.
    if cls == "DateTime" {
        match name {
            "new" | "civil" => {
                let g = |i: usize, dflt: i64| args.get(i).map(as_i).unwrap_or(dflt);
                let (y, mo, d) = (g(0, -4712), g(1, 1), g(2, 1));
                let (hh, mi) = (g(3, 0), g(4, 0));
                let ss = args.get(5).map(as_f).unwrap_or(0.0);
                let days = crate::host::days_from_civil(y, mo, d);
                let secs = days as f64 * 86_400.0 + (hh * 3600 + mi * 60) as f64 + ss;
                return Ok(with_host(|h| h.new_datetime(secs)));
            }
            "now" => return Ok(with_host(|h| h.new_datetime(now_epoch_secs()))),
            "jd" => {
                let g = |i: usize, dflt: i64| args.get(i).map(as_i).unwrap_or(dflt);
                let jd = args
                    .first()
                    .map(as_i)
                    .unwrap_or(crate::host::UNIX_EPOCH_JDN);
                let days = jd - crate::host::UNIX_EPOCH_JDN;
                let (hh, mi) = (g(1, 0), g(2, 0));
                let ss = args.get(3).map(as_f).unwrap_or(0.0);
                let secs = days as f64 * 86_400.0 + (hh * 3600 + mi * 60) as f64 + ss;
                return Ok(with_host(|h| h.new_datetime(secs)));
            }
            "parse" => {
                let s = with_host(|h| h.as_str(&args[0]).unwrap_or_default());
                return match parse_iso_datetime(&s) {
                    Some(secs) => Ok(with_host(|h| h.new_datetime(secs))),
                    None => Err(raise_exc("ArgumentError", "invalid date")),
                };
            }
            _ => {}
        }
    }
    // `ERB.new(template, trim_mode: "-")` — compile a template into a buffer-
    // building Ruby program (stored in the `@src` ivar) and return an ERB
    // instance. `#result`/`#result_with_hash` later evaluate that program.
    if cls == "ERB" && name == "new" {
        let template = arg_str(&args[0]);
        // The trim mode may arrive as a `trim_mode:` keyword (modern form) or as
        // the deprecated 2nd/3rd positional string. Dash-trim is on when the mode
        // string contains '-'.
        let mode = args
            .iter()
            .skip(1)
            .find_map(|a| {
                with_host(|h| h.as_hash(a))
                    .and_then(|m| m.get(&RKey::Sym("trim_mode".into())).cloned())
                    .and_then(|v| with_host(|h| h.as_str(&v)))
            })
            .or_else(|| args.get(2).and_then(|a| with_host(|h| h.as_str(a))))
            .unwrap_or_default();
        let dash_trim = mode.contains('-');
        let src = erb_compile(&template, dash_trim);
        let src_val = new_str(src);
        let obj = with_host(|h| h.new_object("ERB"));
        with_host(|h| h.set_ivar_of(&obj, "src", src_val));
        return Ok(obj);
    }
    // The `Math` module: floating-point functions and the constants `PI`/`E`,
    // reached as `Math.sqrt(x)` and `Math::PI` (both a method send on the ref).
    if cls == "Math" {
        let x = || as_f(&args[0]);
        let y = || as_f(&args[1]);
        let f = match name {
            "PI" => Some(std::f64::consts::PI),
            "E" => Some(std::f64::consts::E),
            "sqrt" => Some(x().sqrt()),
            "cbrt" => Some(x().cbrt()),
            "sin" => Some(x().sin()),
            "cos" => Some(x().cos()),
            "tan" => Some(x().tan()),
            "asin" => Some(x().asin()),
            "acos" => Some(x().acos()),
            "atan" => Some(x().atan()),
            "atan2" => Some(x().atan2(y())),
            "sinh" => Some(x().sinh()),
            "cosh" => Some(x().cosh()),
            "tanh" => Some(x().tanh()),
            "asinh" => Some(x().asinh()),
            "acosh" => Some(x().acosh()),
            "atanh" => Some(x().atanh()),
            "exp" => Some(x().exp()),
            // `Math.log(x)` is natural log; `Math.log(x, base)` changes base.
            "log" if args.len() >= 2 => Some(x().log(y())),
            "log" => Some(x().ln()),
            "log2" => Some(x().log2()),
            "log10" => Some(x().log10()),
            "hypot" => Some(x().hypot(y())),
            "ldexp" => Some(x() * 2f64.powi(as_i(&args[1]) as i32)),
            "gamma" => Some(math_gamma(x())),
            "erf" => Some(math_erf(x())),
            "erfc" => Some(1.0 - math_erf(x())),
            _ => None,
        };
        if let Some(v) = f {
            return Ok(Value::Float(v));
        }
    }
    // The `JSON` module (dependency-free, hand-written over the host value model).
    // `generate`/`dump` encode; `parse`/`load` decode; `pretty_generate` indents.
    if cls == "JSON" {
        match name {
            "generate" | "dump" => {
                return match with_host(|h| json_encode(h, &args[0])) {
                    Ok(s) => Ok(new_str(s)),
                    Err(e) => Err(raise_exc("JSON::GeneratorError", &e)),
                };
            }
            "pretty_generate" => {
                return match with_host(|h| json_pretty(h, &args[0], 0)) {
                    Ok(s) => Ok(new_str(s)),
                    Err(e) => Err(raise_exc("JSON::GeneratorError", &e)),
                };
            }
            "parse" | "load" => {
                let src = with_host(|h| h.as_str(&args[0])).unwrap_or_default();
                let symbolize = args
                    .get(1)
                    .and_then(|a| with_host(|h| h.as_hash(a)))
                    .and_then(|m| m.get(&RKey::Sym("symbolize_names".to_string())).cloned())
                    .map(|v| with_host(|h| h.truthy(&v)))
                    .unwrap_or(false);
                return match with_host(|h| json_parse(h, &src, symbolize)) {
                    Ok(v) => Ok(v),
                    Err(e) => Err(raise_exc("JSON::ParserError", &e)),
                };
            }
            _ => {}
        }
    }
    // Dependency-free stdlib modules: SecureRandom, Base64, Digest::{MD5,SHA1,
    // SHA256}, OpenStruct. Each is reached as a class-method send on the module
    // reference (`SecureRandom.hex(8)`, `Base64.encode64(s)`, `Digest::MD5`).
    if let Some(r) = dispatch_stdlib_module(cls, name, args) {
        return r;
    }
    // `File` and `IO` share their class-level surface (`File < IO`): read/write,
    // the path predicates, `open`, `basename`/`dirname`/`extname`/`join`/
    // `expand_path`. `Dir` gets its own class-method dispatcher.
    if cls == "File" || cls == "IO" {
        if let Some(r) = dispatch_file_class(name, args, block.clone()) {
            return r;
        }
    }
    if cls == "Dir" {
        if let Some(r) = dispatch_dir_class(name, args, block.clone()) {
            return r;
        }
    }
    // `TCPServer.new([host,] port)` / `TCPSocket.new(host, port)` over std::net.
    if (cls == "TCPServer" || cls == "TCPSocket") && (name == "new" || name == "open") {
        return tcp_class_new(cls, args, block);
    }
    // SQLite3 — real database persistence via a bundled rusqlite. The `SQLite3`
    // module namespace resolves its sub-classes to class refs (mirroring the
    // `Digest` namespace); `SQLite3::Database.new`/`.open` open a connection.
    if cls == "SQLite3" {
        match name {
            "Database" => return Ok(with_host(|h| h.class_ref("SQLite3::Database"))),
            // The exception namespace: `SQLite3::SQLException` etc. resolve to a
            // class ref so `rescue SQLite3::SQLException` and `SQLite3::SQLException.new`
            // both work. `SQLException` is the base SQL error the runtime raises.
            "SQLException"
            | "Exception"
            | "CantOpenException"
            | "BusyException"
            | "ConstraintException" => {
                return Ok(with_host(|h| h.class_ref(&format!("SQLite3::{name}"))))
            }
            // The linked sqlite library version (`SQLite3::SQLITE_VERSION`).
            "SQLITE_VERSION" => return Ok(new_str(rusqlite::version().to_string())),
            _ => {}
        }
    }
    if cls == "SQLite3::Database" && (name == "new" || name == "open") {
        return sqlite_db_new(args, block);
    }
    // Fiddle — Ruby's built-in FFI. The `Fiddle` module namespace resolves its
    // type-code constants and sub-classes; `Fiddle.dlopen` opens a library;
    // `Fiddle::Function.new` / `Fiddle::Pointer.*` build the callable / pointer.
    if let Some(r) = dispatch_fiddle_classref(cls, name, args)? {
        return Ok(r);
    }
    match name {
        "new" => {
            // `Class.new([Super]) { body }` / `Module.new { body }` — an anonymous
            // class/module. The optional first arg is the superclass; the block is
            // run as a `class_eval` so its `def`s become instance methods.
            if cls == "Class" || cls == "Module" {
                let superclass = if cls == "Class" {
                    args.first()
                        .and_then(|a| with_host(|h| h.classref_name(a)))
                        .or_else(|| Some("Object".to_string()))
                } else {
                    None
                };
                let name = with_host(|h| h.define_anon_class(superclass.clone()));
                let cref = with_host(|h| h.class_ref(&name));
                // Fire the superclass's `inherited` hook — MRI runs it when the
                // class is created (before the body block), just as the compiler
                // does for a static `class X < Y`. Without this a runtime
                // `Class.new(Super)` skips per-subclass setup an `inherited` hook
                // performs (e.g. mustermann's per-subclass NodeTranslator).
                if let Some(sc) = &superclass {
                    if let Some(def) = with_host(|h| h.find_class_method(sc, "inherited")) {
                        let recv = with_host(|h| h.class_ref(sc));
                        crate::host::call_class_method(
                            recv,
                            &def,
                            "inherited",
                            sc,
                            &[cref.clone()],
                            None,
                        )?;
                    }
                }
                if let Some(bl) = block {
                    crate::host::eval_block_scoped(
                        &bl,
                        &cref,
                        crate::host::DefTarget::Instance(name),
                        std::slice::from_ref(&cref),
                    )?;
                }
                return Ok(cref);
            }
            // `Array.new(n)` / `Array.new(n, val)` / `Array.new(n) { |i| ... }`.
            if cls == "Array" {
                let n = args.first().map(as_i).unwrap_or(0).max(0) as usize;
                let items: Vec<Value> = if let Some(bl) = &block {
                    (0..n)
                        .map(|i| call_proc(bl, &[Value::Int(i as i64)]))
                        .collect::<Result<_, _>>()?
                } else {
                    let fill = args.get(1).cloned().unwrap_or(Value::Undef);
                    vec![fill; n]
                };
                return Ok(new_arr(items));
            }
            // `String.new` / `String.new("x")` — a real mutable string (backed by
            // `RObj::Str`), not an opaque object. Encoding keywords are accepted
            // and ignored (only UTF-8/binary bytes are modeled).
            if cls == "String" {
                let s = args
                    .first()
                    .and_then(|a| with_host(|h| h.as_str(a)))
                    .unwrap_or_default();
                return Ok(with_host(|h| h.new_string(s)));
            }
            // `Hash.new(default)` builds a real Hash whose `[]` returns `default`
            // for a missing key (the counter idiom `Hash.new(0)`); the block form
            // `Hash.new { |h,k| ... }` calls the block on each miss instead.
            if cls == "Hash" {
                if let Some(bl) = block {
                    return Ok(with_host(|h| h.new_hash_with_proc(IndexMap::new(), bl)));
                }
                let default = args.first().cloned().unwrap_or(Value::Undef);
                return Ok(with_host(|h| {
                    h.new_hash_with_default(IndexMap::new(), default)
                }));
            }
            // A user `initialize` wins. Otherwise an exception class's default
            // `new(msg)` stores the message (defaulting to the class name).
            if with_host(|h| h.find_method(cls, "initialize")).is_some() {
                let obj = with_host(|h| h.new_object(cls));
                call_instance_method(obj.clone(), cls, "initialize", args, block)?;
                Ok(obj)
            } else if is_exception_class(cls) {
                let msg = match args.first() {
                    Some(a) => with_host(|h| h.to_s(a)),
                    None => cls.to_string(),
                };
                Ok(with_host(|h| h.new_exception(cls, &msg)))
            } else {
                Ok(with_host(|h| h.new_object(cls)))
            }
        }
        "name" | "to_s" | "inspect" => Ok(new_str(cls.to_string())),
        // Runtime `Class#include`/`prepend`/`extend(Module, …)` — the same mixin
        // effect the compile-time class-body forms have, but for a conditional or
        // dynamic call (`prepend Correctable if Correctable`). Fires the module's
        // `included`/`prepended`/`extended` hook.
        // `C.attr_accessor(:x)` / `C.send(:attr_reader, :y)` — register a native
        // accessor directly on the class.
        "attr" | "attr_accessor" | "attr_reader" | "attr_writer" => {
            let reader = name != "attr_writer";
            let writer = matches!(name, "attr_accessor" | "attr_writer");
            for a in args {
                if matches!(a, Value::Bool(_)) {
                    continue;
                }
                let field = name_of(a);
                with_host(|h| h.add_attr(cls, &field, reader, writer));
            }
            Ok(Value::Undef)
        }
        "include" | "prepend" | "extend" if !args.is_empty() => {
            for a in args {
                if let Some(m) = with_host(|h| h.classref_name(a)) {
                    with_host(|h| h.class_mixin(cls, &m, name));
                    let hook = match name {
                        "include" => "included",
                        "prepend" => "prepended",
                        _ => "extended",
                    };
                    if with_host(|h| h.find_class_method(&m, hook)).is_some() {
                        let target = with_host(|h| h.class_ref(cls));
                        dispatch(a, hook, &[target], None)?;
                    }
                }
            }
            Ok(with_host(|h| h.class_ref(cls)))
        }
        "superclass" => Ok(with_host(|h| match h.class_superclass(cls) {
            Some(sc) => h.class_ref(&sc),
            None => Value::Undef,
        })),
        // `Module.nesting` — best-effort. The runtime does not track the lexical
        // nesting of the call site (class bodies are flattened at compile time),
        // so this returns the empty array (correct at the top level; a namespaced
        // call site would report its enclosing modules in MRI).
        "nesting" if cls == "Module" => Ok(new_arr(vec![])),
        // `Module#ancestors` — the class/module ancestor chain as class refs.
        "ancestors" => Ok(with_host(|h| {
            let refs: Vec<Value> = h
                .class_ancestry(cls)
                .iter()
                .map(|n| h.class_ref(n))
                .collect();
            h.new_array(refs)
        })),
        // `Module#instance_methods([include_inherited=true])` — the instance
        // method names as symbols. `false` gives only the class's own methods;
        // `true`/no arg walks the user-defined ancestor chain. Visibility is not
        // modeled, so `public_instance_methods` is the same set.
        "instance_methods" | "public_instance_methods" => {
            let inherited = args
                .first()
                .map(|a| with_host(|h| h.truthy(a)))
                .unwrap_or(true);
            Ok(with_host(|h| {
                let names = h.instance_method_names(cls, inherited);
                let syms: Vec<Value> = names.iter().map(|n| h.new_symbol(n)).collect();
                h.new_array(syms)
            }))
        }
        // `Module#method_defined?(sym)` — true if the method is defined on the
        // class or any ancestor. Visibility is not modeled, so this also serves
        // `public_method_defined?`.
        "method_defined?" | "public_method_defined?" => {
            let m = name_of(&args[0]);
            Ok(Value::Bool(with_host(|h| h.is_method_defined(cls, &m))))
        }
        // `Module#class_variable_get/set/defined?` and `class_variables`. The
        // reflective name arrives with its `@@` sigil (`:@@x`); the store keys
        // are bare, so strip it.
        "class_variable_get" => {
            let raw = name_of(&args[0]);
            let key = raw.strip_prefix("@@").unwrap_or(&raw);
            let v = with_host(|h| h.get_cvar(cls, key));
            if matches!(v, Value::Undef) {
                return Err(raise_exc(
                    "NameError",
                    &format!("uninitialized class variable {raw} in {cls}"),
                ));
            }
            Ok(v)
        }
        "class_variable_set" => {
            let raw = name_of(&args[0]);
            let key = raw.strip_prefix("@@").unwrap_or(&raw).to_string();
            let val = args[1].clone();
            with_host(|h| h.set_cvar(cls, &key, val.clone()));
            Ok(val)
        }
        "class_variable_defined?" => {
            let raw = name_of(&args[0]);
            let key = raw.strip_prefix("@@").unwrap_or(&raw);
            Ok(Value::Bool(with_host(|h| h.cvar_defined(cls, key))))
        }
        "class_variables" => Ok(with_host(|h| {
            let syms: Vec<Value> = h
                .class_var_names(cls)
                .iter()
                .map(|n| h.new_symbol(&format!("@@{n}")))
                .collect();
            h.new_array(syms)
        })),
        // `Module#singleton_class` — the class object's own metaclass. Modeled as
        // a classref named `#<Class:cls>`; `attr_accessor`/`def`/`define_method`
        // on it register class-level members on `cls` (see the accessor check in
        // the `_` arm below). activesupport: `singleton_class.attr_accessor :x`.
        "singleton_class" => Ok(with_host(|h| h.class_ref(&format!("#<Class:{cls}>")))),
        // `Module#instance_method(:name)` — an UnboundMethod, modeled as a
        // receiver-less Method (`recv` = nil) that `bind`/`bind_call` re-target.
        "instance_method" | "public_instance_method" => {
            let m = name_of(&args[0]);
            Ok(with_host(|h| h.new_method(Value::Undef, &m)))
        }
        // Numeric class constants (`Float::INFINITY`, `Float::NAN`, …), reached
        // via `::` (which lowers to a method call on the class reference).
        "INFINITY" if cls == "Float" => Ok(Value::Float(f64::INFINITY)),
        "NAN" if cls == "Float" => Ok(Value::Float(f64::NAN)),
        "MAX" if cls == "Float" => Ok(Value::Float(f64::MAX)),
        "MIN" if cls == "Float" => Ok(Value::Float(f64::MIN_POSITIVE)),
        "EPSILON" if cls == "Float" => Ok(Value::Float(f64::EPSILON)),
        "DIG" if cls == "Float" => Ok(Value::Int(15)),
        "MANT_DIG" if cls == "Float" => Ok(Value::Int(53)),
        // `Class === obj` and `obj.is_a?(Class)` — case/when matching.
        "===" => Ok(Value::Bool(with_host(|h| h.is_a(&args[0], cls)))),
        // `Hash[...]` / `Array[...]` constructors.
        "[]" if cls == "Hash" => {
            let mut map = IndexMap::new();
            // `Hash[[[k,v],...]]` (one array-of-pairs arg) vs `Hash[k,v,k,v]`.
            let pairs: Vec<Value> =
                if args.len() == 1 && with_host(|h| h.as_array(&args[0])).is_some() {
                    with_host(|h| h.as_array(&args[0]).unwrap())
                        .iter()
                        .flat_map(|p| with_host(|h| h.as_array(p)).unwrap_or_default())
                        .collect()
                } else {
                    args.to_vec()
                };
            for pair in pairs.chunks(2) {
                if pair.len() == 2 {
                    let k = with_host(|h| h.value_to_key(&pair[0]));
                    map.insert(k, pair[1].clone());
                }
            }
            Ok(with_host(|h| h.new_hash(map)))
        }
        "[]" if cls == "Array" => Ok(new_arr(args.to_vec())),
        // `Mod.const_get(sym)` — resolve a constant by name. A qualified string
        // (`"A::B"`) or a name relative to the receiver (`A.const_get("B")`) is
        // resolved under the fully-qualified path in the namespaced store.
        "const_get" => {
            let cname = name_of(&args[0]);
            match const_lookup_under(cls, &cname) {
                Some(v) => Ok(v),
                None => Err(raise_exc(
                    "NameError",
                    &format!("uninitialized constant {cname}"),
                )),
            }
        }
        // `Mod.const_set(sym, val)` — define a constant under the receiver's
        // namespace (`A.const_set(:B, v)` sets `A::B`; a top-level receiver sets
        // the bare name).
        "const_set" => {
            let cname = name_of(&args[0]);
            let val = args.get(1).cloned().unwrap_or(Value::Undef);
            let key = const_key_under(cls, &cname);
            // Naming an anonymous class/module via `const_set` gives it the
            // constant's qualified name (MRI behavior) so it registers under that
            // name — a subclass `class Sub < Mod::Anon` then resolves correctly.
            let val = with_host(|h| match h.classref_name(&val) {
                Some(cref) if h.is_anon_class(&cref) => {
                    h.rename_class(&cref, &key);
                    h.class_ref(&key)
                }
                _ => val.clone(),
            });
            with_host(|h| h.set_const(&key, val.clone()));
            Ok(val)
        }
        // `Mod.const_defined?(sym)`.
        "const_defined?" => {
            let cname = name_of(&args[0]);
            Ok(Value::Bool(const_lookup_under(cls, &cname).is_some()))
        }
        // `Mod.remove_const(:X)` — remove and return the constant (or class).
        "remove_const" => {
            let cname = name_of(&args[0]);
            let key = const_key_under(cls, &cname);
            Ok(with_host(|h| h.remove_const(&key)))
        }
        // `Klass.undef_method(:m)` / `remove_method(:m)` — drop the instance
        // method (activesupport undefs `#with` on immediate classes).
        "undef_method" | "remove_method" => {
            for a in args {
                let m = name_of(a);
                with_host(|h| h.remove_instance_method(cls, &m));
            }
            Ok(with_host(|h| h.class_ref(cls)))
        }
        // `Mod.constants` — the user-defined constant names (flat store), as
        // symbols. Class names are not enumerated (matching neither MRI exactly
        // nor hiding user constants set via assignment / const_set).
        "constants" => Ok(with_host(|h| {
            let syms: Vec<Value> = h.const_names().iter().map(|n| h.new_symbol(n)).collect();
            h.new_array(syms)
        })),
        // `Klass.define_method(:name) { body }` (or a Proc/Method 2nd arg) —
        // register an instance method whose body is the block, running with `self`
        // bound to the receiver when invoked (mirrors the class-body form).
        "define_method" => {
            let mname = name_of(&args[0]);
            let proc = block.or_else(|| args.get(1).cloned()).ok_or_else(|| {
                raise_exc("ArgumentError", "tried to create method without a block")
            })?;
            with_host(|h| h.add_define_method(cls, &mname, proc));
            Ok(with_host(|h| h.new_symbol(&mname)))
        }
        // `Klass.alias_method(:new, :old)` — register an alias on the class.
        "alias_method" if args.len() >= 2 => {
            let (new_name, old_name) = (name_of(&args[0]), name_of(&args[1]));
            with_host(|h| h.add_alias(cls, &new_name, &old_name));
            Ok(with_host(|h| h.new_symbol(&new_name)))
        }
        _ => {
            // A class method: `def self.m` runs with self bound to the class ref.
            // Bind `def_class` to the class that actually owns the method (not the
            // lookup-origin subclass) so `super` resumes above the defining class.
            if let Some((def, owner)) = with_host(|h| h.find_class_method_owner(cls, name)) {
                let recv = with_host(|h| h.class_ref(cls));
                return crate::host::call_class_method(recv, &def, name, &owner, args, block);
            }
            // A class-method alias registered on the singleton class via
            // `singleton_class.alias_method :new_name, :old` — re-dispatch to the
            // target (which may be a native class method like `new`, so this can't
            // resolve to a `MethodDef`). concurrent-ruby: `Node.[]` aliases `new`.
            if let Some(target) = with_host(|h| h.find_alias(&format!("#<Class:{cls}>"), name)) {
                if target != name {
                    return dispatch_classref(cls, &target, args, block);
                }
            }
            // A class/module object is an instance of `Class < Module`, so an
            // instance method added by reopening `class Module`/`class Class`
            // is callable on it, run with `self` bound to the class. This is how
            // Rails core-extensions attach class-body macros (`delegate`,
            // `mattr_accessor`, `thread_mattr_accessor`, …).
            for base in ["Class", "Module"] {
                if let Some((def, owner)) = with_host(|h| h.find_method_owner(base, name)) {
                    let recv = with_host(|h| h.class_ref(cls));
                    return crate::host::call_class_method(recv, &def, name, &owner, args, block);
                }
            }
            // Class-level attr accessor from `singleton_class.attr_accessor :x`
            // (registered on the `#<Class:cls>` metaclass): read/write the class
            // ivar `@field` on `cls`.
            let sing = format!("#<Class:{cls}>");
            if let Some((field, writer)) = with_host(|h| h.attr_access(&sing, name)) {
                let recv = with_host(|h| h.class_ref(cls));
                if writer {
                    let val = args[0].clone();
                    with_host(|h| h.set_ivar_of(&recv, &field, val.clone()));
                    return Ok(val);
                }
                return Ok(with_host(|h| h.ivar_of(&recv, &field)));
            }
            // `Mod::Const` — a namespaced constant or nested class/module. Both
            // `::` and `.` lower to a method call here, so a no-arg capitalized
            // name is a constant lookup under the fully-qualified path.
            if args.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                let qualified = format!("{cls}::{name}");
                // A registered constant — return its value even when nil (a
                // deliberately-nil constant like `File::ALT_SEPARATOR` reads back
                // as nil rather than falling through to `const_missing`/NameError).
                if with_host(|h| h.has_const(&qualified)) {
                    return Ok(with_host(|h| h.get_const(&qualified)));
                }
                if with_host(|h| h.class_exists(&qualified)) {
                    return Ok(with_host(|h| h.class_ref(&qualified)));
                }
                // A pending `autoload :Const, "path"` on this namespace: require
                // the file, then retry — the scoped `Mod::Const` form must fire
                // autoload just like the bare read in `b_getconst` does.
                if let Some(path) = with_host(|h| h.take_autoload(&qualified)) {
                    let pv = with_host(|h| h.new_string(path));
                    do_require(&[pv], ReqMode::Require)?;
                    let v = with_host(|h| h.get_const(&qualified));
                    if !matches!(v, Value::Undef) {
                        return Ok(v);
                    }
                    if with_host(|h| h.class_exists(&qualified)) {
                        return Ok(with_host(|h| h.class_ref(&qualified)));
                    }
                }
            }
            // `Mod::Const` for an unresolved constant calls `Mod.const_missing`
            // (Rails autoloading depends on this). The `::` form reaches here as a
            // no-arg call named like a constant (leading uppercase).
            if args.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()) {
                if let Some(def) = with_host(|h| h.find_class_method(cls, "const_missing")) {
                    let recv = with_host(|h| h.class_ref(cls));
                    let sym = with_host(|h| h.new_symbol(name));
                    return crate::host::call_class_method(
                        recv,
                        &def,
                        "const_missing",
                        cls,
                        &[sym],
                        None,
                    );
                }
                // An unresolved namespaced constant is a NameError, matching MRI
                // (`A::Nope` → `uninitialized constant A::Nope`), not a NoMethodError.
                return Err(raise_exc(
                    "NameError",
                    &format!("uninitialized constant {cls}::{name}"),
                ));
            }
            Err(raise_exc(
                "NoMethodError",
                &format!("undefined method '{name}' for class {cls}"),
            ))
        }
    }
}

/// The storage key for `receiver.const_set(name, …)`: qualified under the
/// receiver's namespace, except a top-level receiver (`Object`/`Kernel`), which
/// stores the bare (or already-qualified) name.
fn const_key_under(cls: &str, name: &str) -> String {
    if matches!(cls, "Object" | "Kernel" | "BasicObject") {
        name.to_string()
    } else {
        format!("{cls}::{name}")
    }
}

/// Resolve `receiver.const_get(name)`. Tries, in order: the receiver-qualified
/// path (`A.const_get("B")` → `A::B`), the name taken as an absolute top-level
/// path (`Object.const_get("A::B")`), and finally the bare leaf segment (legacy
/// flat lookup). Each is resolved through `const_lookup`.
fn const_lookup_under(cls: &str, name: &str) -> Option<Value> {
    // Ruby resolves a constant through the receiver's superclass chain, so a
    // subclass sees a constant defined on its ancestor (`Child.const_get(:Inner)`
    // finds `Base::Inner`). Walk from `cls` up its superclasses first.
    let mut cur = Some(cls.to_string());
    let mut guard = 0;
    while let Some(c) = cur {
        if let Some(v) = const_lookup(&const_key_under(&c, name)) {
            return Some(v);
        }
        guard += 1;
        if guard > 100 {
            break; // cycle guard
        }
        cur = with_host(|h| h.superclass_of(&c));
    }
    const_lookup(name).or_else(|| {
        let leaf = name.rsplit("::").next().unwrap_or(name);
        const_lookup(leaf)
    })
}

/// Resolve a constant name against the flat constant store, falling back to a
/// class reference for a name that denotes a (user or builtin) class.
fn const_lookup(name: &str) -> Option<Value> {
    with_host(|h| {
        // A registered constant — return its value even when nil (a deliberately
        // nil constant like `File::ALT_SEPARATOR` must read back as nil, not raise).
        if h.has_const(name) {
            return Some(h.get_const(name));
        }
        if h.class_exists(name) || h.is_builtin_class(name) {
            return Some(h.class_ref(name));
        }
        None
    })
    .or_else(|| {
        if is_builtin_exception(name) {
            Some(with_host(|h| h.class_ref(name)))
        } else {
            None
        }
    })
}

/// Instance methods generated for a `Struct` class. Returns `Ok(None)` for a
/// method not provided by Struct (so normal object dispatch can continue).
fn struct_method(
    recv: &Value,
    cls: &str,
    members: &[String],
    name: &str,
    args: &[Value],
    block: &Option<Value>,
) -> Result<Option<Value>, String> {
    let values = || -> Vec<Value> {
        members
            .iter()
            .map(|m| with_host(|h| h.ivar_of(recv, m)))
            .collect()
    };
    // A member reader / writer (`p.x` / `p.x = v`).
    if let Some(m) = name.strip_suffix('=') {
        if members.iter().any(|mm| mm == m) {
            with_host(|h| h.set_ivar_of(recv, m, args[0].clone()));
            return Ok(Some(args[0].clone()));
        }
    }
    if members.iter().any(|m| m == name) {
        return Ok(Some(with_host(|h| h.ivar_of(recv, name))));
    }
    let r = match name {
        "to_a" | "values" | "deconstruct" => new_arr(values()),
        "members" => new_arr(
            members
                .iter()
                .map(|m| with_host(|h| h.new_symbol(m)))
                .collect(),
        ),
        "size" | "length" => Value::Int(members.len() as i64),
        "to_h" => {
            let mut map = IndexMap::new();
            for (m, v) in members.iter().zip(values()) {
                map.insert(RKey::Sym(m.clone()), v);
            }
            with_host(|h| h.new_hash(map))
        }
        "deconstruct_keys" => {
            let mut map = IndexMap::new();
            match args.first().and_then(|a| with_host(|h| h.as_array(a))) {
                // An Array of symbols selects those members, in the requested
                // order; unknown keys are skipped. Matches MRI `deconstruct_keys`.
                Some(req) => {
                    for k in req {
                        if let Some(m) = with_host(|h| h.as_symbol(&k)) {
                            if members.contains(&m) {
                                let v = with_host(|h| h.ivar_of(recv, &m));
                                map.insert(RKey::Sym(m), v);
                            }
                        }
                    }
                }
                // `nil` (or no argument) returns every member in declaration order.
                None => {
                    for (m, v) in members.iter().zip(values()) {
                        map.insert(RKey::Sym(m.clone()), v);
                    }
                }
            }
            with_host(|h| h.new_hash(map))
        }
        "[]" => {
            let vals = values();
            match &args[0] {
                Value::Int(i) => norm_idx(*i, vals.len())
                    .and_then(|k| vals.get(k))
                    .cloned()
                    .unwrap_or(Value::Undef),
                other => {
                    let key = with_host(|h| h.as_symbol(other))
                        .or_else(|| with_host(|h| h.as_str(other)));
                    key.and_then(|k| members.iter().position(|m| *m == k))
                        .and_then(|i| vals.get(i))
                        .cloned()
                        .unwrap_or(Value::Undef)
                }
            }
        }
        "each" if block.is_some() => {
            let bl = block.clone().unwrap();
            for v in values() {
                call_proc(&bl, &[v])?;
            }
            recv.clone()
        }
        // Blockless `each` yields an Enumerator over the member values, so
        // `struct.each.to_a` / `.with_index` etc. work as in MRI.
        "each" => with_host(|h| h.new_enumerator(values(), "each")),
        // `#values_at(i, …)` — member values at the given indices (negative
        // counts from the end), in the requested order.
        "values_at" => {
            let vals = values();
            let mut out = Vec::new();
            for a in args {
                if let Value::Int(i) = a {
                    let v = norm_idx(*i, vals.len())
                        .and_then(|k| vals.get(k))
                        .cloned()
                        .unwrap_or(Value::Undef);
                    out.push(v);
                }
            }
            new_arr(out)
        }
        "each_pair" if block.is_some() => {
            let bl = block.clone().unwrap();
            for (m, v) in members.iter().zip(values()) {
                let sym = with_host(|h| h.new_symbol(m));
                call_proc(&bl, &[sym, v])?;
            }
            recv.clone()
        }
        "==" | "eql?" => {
            let same = with_host(|h| h.object_class(&args[0])).as_deref() == Some(cls)
                && members
                    .iter()
                    .zip(values())
                    .all(|(m, v)| with_host(|h| h.eq_values(&h.ivar_of(&args[0], m), &v)));
            Value::Bool(same)
        }
        "to_s" | "inspect" => {
            let parts: Vec<String> = members
                .iter()
                .zip(values())
                .map(|(m, v)| format!("{m}={}", with_host(|h| h.inspect(&v))))
                .collect();
            // `Data` instances inspect as `#<data Name x=…>`; Structs as `#<struct …>`.
            let kind = if with_host(|h| h.is_data_class(cls)) {
                "data"
            } else {
                "struct"
            };
            new_str(format!("#<{kind} {cls} {}>", parts.join(", ")))
        }
        // `Data#with(**changes)` — a copy of the receiver with the named members
        // replaced (unknown keys raise ArgumentError, as in MRI). Result frozen.
        "with" if with_host(|h| h.is_data_class(cls)) => {
            let obj = with_host(|h| h.new_object(cls));
            for m in members {
                let cur = with_host(|h| h.ivar_of(recv, m));
                with_host(|h| h.set_ivar_of(&obj, m, cur));
            }
            if let Some(k) = args.first().and_then(|a| with_host(|h| h.as_hash(a))) {
                for (key, v) in &k {
                    match key {
                        RKey::Sym(m) if members.iter().any(|mm| mm == m) => {
                            with_host(|h| h.set_ivar_of(&obj, m, v.clone()));
                        }
                        RKey::Sym(m) => {
                            return Err(raise_exc(
                                "ArgumentError",
                                &format!("unknown keyword: :{m}"),
                            ))
                        }
                        _ => {
                            return Err(raise_exc("ArgumentError", "unknown keyword"));
                        }
                    }
                }
            }
            with_host(|h| h.freeze_value(&obj));
            obj
        }
        // `#dig(key, *rest)` — select a member by name (Symbol/String) or index,
        // then dig into the result with any remaining keys (MRI Struct#dig).
        "dig" if !args.is_empty() => {
            let vals = values();
            let first = match &args[0] {
                Value::Int(i) => norm_idx(*i, vals.len()).and_then(|k| vals.get(k)).cloned(),
                other => {
                    let key = with_host(|h| h.as_symbol(other).or_else(|| h.as_str(other)));
                    key.and_then(|k| members.iter().position(|m| *m == k))
                        .and_then(|i| vals.get(i))
                        .cloned()
                }
            }
            .unwrap_or(Value::Undef);
            if args.len() == 1 || matches!(first, Value::Undef) {
                return Ok(Some(first));
            }
            return Ok(Some(dispatch(&first, "dig", &args[1..], None)?));
        }
        // Struct includes Enumerable: `map`/`select`/`reduce`/`sum`/`min`/`find`/…
        // run over the member values as an Array (matching MRI). An unrecognized
        // name falls through (Ok(None)) so normal resolution continues.
        _ => {
            let delegated = dispatch_array(&new_arr(values()), name, args, block.clone());
            return match delegated {
                Err(e)
                    if e.starts_with("undefined method") && e.contains("an instance of Array") =>
                {
                    Ok(None)
                }
                other => other.map(Some),
            };
        }
    };
    Ok(Some(r))
}

/// Methods on a user object: dispatch through the class chain, else the
/// exception `message` accessor, else an error.
fn dispatch_object(
    recv: &Value,
    cls: &str,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    if with_host(|h| h.find_method_owner(cls, name)).is_some() {
        return call_instance_method(recv.clone(), cls, name, args, block);
    }
    // A `define_method` block runs as an instance method with `self` = receiver.
    if let Some(proc) = with_host(|h| h.find_define_method(cls, name)) {
        return crate::host::call_proc_self(&proc, args, Some(recv));
    }
    // An `alias_method`/`alias` name forwards to its target method.
    if let Some(target) = with_host(|h| h.find_alias(cls, name)) {
        return dispatch(recv, &target, args, block);
    }
    // Struct instance methods (accessors, ==, to_a/to_h, members, [], each, …).
    if let Some((members, _)) = with_host(|h| h.struct_def(cls)) {
        if let Some(r) = struct_method(recv, cls, &members, name, args, &block)? {
            return Ok(r);
        }
    }
    // Comparable module: `< <= > >= between? clamp` derived from the class's `<=>`.
    if with_host(|h| h.is_a(recv, "Comparable")) {
        if let Some(r) = comparable_method(recv, name, args)? {
            return Ok(r);
        }
    }
    // Enumerable module: `map`, `select`, `reduce`, … derived from the class's `each`.
    if with_host(|h| h.is_a(recv, "Enumerable")) {
        if let Some(r) = enumerable_method(recv, name, args, block.clone())? {
            return Ok(r);
        }
    }
    // OpenStruct: dynamic attribute get/set plus `to_h`/`[]`/`[]=`/`each_pair`/…,
    // stored as the object's ivars. An unknown reader returns nil (never raises).
    if cls == "OpenStruct" {
        if let Some(r) = openstruct_method(recv, name, args, block.clone())? {
            return Ok(r);
        }
    }
    // ERB instance: `result`/`result_with_hash` evaluate the compiled template,
    // `src` returns the generated Ruby source.
    if cls == "ERB" {
        if let Some(r) = erb_method(recv, name, args)? {
            return Ok(r);
        }
    }
    // StringIO: a String-backed IO (buffer + read cursor in the `buf`/`pos` ivars).
    if cls == "StringIO" {
        return stringio_method(recv, name, args, block);
    }
    // Encoding object (from `String#encoding`): `name` reports "UTF-8" (the only
    // encoding we carry). `to_s`/`inspect` are handled by the host formatters,
    // which the universal dispatch reaches before this arm.
    if cls == "Encoding" && name == "name" {
        return Ok(with_host(|h| {
            let v = h.ivar_of(recv, "name");
            let s = h.to_s(&v);
            h.new_string(s)
        }));
    }
    // Random instance: its own reproducible PRNG stream (state in the `state` ivar).
    if cls == "Random" {
        if let Some(r) = random_method(recv, name, args)? {
            return Ok(r);
        }
    }
    // Mutex/Monitor: a lock flag; `synchronize` runs the block under the lock.
    if cls == "Mutex" || cls == "Thread::Mutex" || cls == "Monitor" {
        if let Some(r) = mutex_method(recv, name, block.clone())? {
            return Ok(r);
        }
    }
    // Queue/SizedQueue: thread-safe FIFO with blocking pop/push.
    if cls == "Queue" || cls == "SizedQueue" {
        if let Some(r) = queue_method(recv, name, args)? {
            return Ok(r);
        }
    }
    // ConditionVariable: wait releases the mutex + GVL and parks until signalled.
    if cls == "ConditionVariable" {
        if let Some(r) = condvar_method(recv, name, args)? {
            return Ok(r);
        }
    }
    match name {
        "message" | "to_s" => {
            let m = with_host(|h| h.ivar_of(recv, "message"));
            if matches!(m, Value::Undef) {
                Ok(new_str(cls.to_string()))
            } else {
                Ok(m)
            }
        }
        // `NameError#name` / `NoMethodError#name` — the missing method/constant
        // name (a Symbol). Stored as an ivar when raised natively, else parsed
        // from the message (`undefined method 'foo' …`).
        "name" => {
            let stored = with_host(|h| h.ivar_of(recv, "name"));
            if !matches!(stored, Value::Undef) {
                return Ok(stored);
            }
            let msg = with_host(|h| h.as_str(&h.ivar_of(recv, "message"))).unwrap_or_default();
            let nm = msg.split('\'').nth(1).unwrap_or("");
            Ok(with_host(|h| h.new_symbol(nm)))
        }
        // `NameError#receiver` / `NoMethodError#receiver`.
        "receiver" => Ok(with_host(|h| h.ivar_of(recv, "receiver"))),
        // A runtime-declared attribute accessor (`class_eval { attr_accessor :x }`)
        // reads/writes the `@field` ivar natively, ahead of method_missing.
        _ if with_host(|h| h.attr_access(cls, name)).is_some() => {
            let (field, is_writer) = with_host(|h| h.attr_access(cls, name)).unwrap();
            if is_writer {
                let v = args.first().cloned().unwrap_or(Value::Undef);
                with_host(|h| h.set_ivar_of(recv, &field, v.clone()));
                Ok(v)
            } else {
                Ok(with_host(|h| h.ivar_of(recv, &field)))
            }
        }
        // A class that defines `method_missing` handles any otherwise-undefined
        // method: `method_missing(:name, *args, &block)`.
        _ if with_host(|h| h.find_method_owner(cls, "method_missing")).is_some() => {
            let mut mm_args = Vec::with_capacity(args.len() + 1);
            mm_args.push(with_host(|h| h.new_symbol(name)));
            mm_args.extend_from_slice(args);
            call_instance_method(recv.clone(), cls, "method_missing", &mm_args, block)
        }
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for an instance of {cls}"),
        )),
    }
}

/// Comparable's derived instance methods (`< <= > >= between? clamp`), each built
/// on the class's own `<=>`. Returns `Ok(None)` when `name` is not one Comparable
/// provides. A `<=>` result of `nil` means the operands are incomparable, which
/// Ruby surfaces as `ArgumentError` for the ordering helpers.
fn comparable_method(recv: &Value, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
    let spaceship = |other: &Value| -> Result<Option<i64>, String> {
        match dispatch(recv, "<=>", std::slice::from_ref(other), None)? {
            Value::Undef => Ok(None),
            v => Ok(Some(as_i(&v))),
        }
    };
    let cmp_err = |other: &Value| -> String {
        raise_exc(
            "ArgumentError",
            &format!(
                "comparison of {} with {} failed",
                with_host(|h| h.class_of(recv)),
                with_host(|h| h.class_of(other)),
            ),
        )
    };
    match name {
        "<" | "<=" | ">" | ">=" => {
            let o = &args[0];
            let Some(c) = spaceship(o)? else {
                return Err(cmp_err(o));
            };
            let b = match name {
                "<" => c < 0,
                "<=" => c <= 0,
                ">" => c > 0,
                _ => c >= 0,
            };
            Ok(Some(Value::Bool(b)))
        }
        "between?" => {
            let (lo, hi) = (&args[0], &args[1]);
            let (Some(a), Some(b)) = (spaceship(lo)?, spaceship(hi)?) else {
                return Err(cmp_err(lo));
            };
            Ok(Some(Value::Bool(a >= 0 && b <= 0)))
        }
        "clamp" => {
            let (lo, hi) = (&args[0], &args[1]);
            let Some(a) = spaceship(lo)? else {
                return Err(cmp_err(lo));
            };
            if a < 0 {
                return Ok(Some(lo.clone()));
            }
            let Some(b) = spaceship(hi)? else {
                return Err(cmp_err(hi));
            };
            if b > 0 {
                return Ok(Some(hi.clone()));
            }
            Ok(Some(recv.clone()))
        }
        _ => Ok(None),
    }
}

/// Materialize a user `Enumerable`'s elements by driving its own `each` with a
/// native collector block, exactly how real Ruby's `Enumerable` derives every
/// method from `each`. Returns the yielded values in iteration order.
fn enum_to_vec(recv: &Value) -> Result<Vec<Value>, String> {
    let sink = with_host(|h| h.new_enum_sink());
    dispatch(recv, "each", &[], Some(sink))?;
    Ok(with_host(|h| h.take_enum_sink()))
}

/// Enumerable's derived instance methods (`map select reject reduce to_a find
/// count min max sort include? first`, plus their standard aliases), each built
/// on the class's own `each`: the elements are materialized once and the request
/// is delegated to the eager `Array` implementation so the block, symbol-proc,
/// and argument handling all match Ruby. Returns `Ok(None)` when `name` is not an
/// Enumerable method (so the caller raises `NoMethodError`).
fn enumerable_method(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Option<Value>, String> {
    // Only names Enumerable actually provides (and that `dispatch_array`
    // implements) are derived; anything else falls through to `NoMethodError`.
    const ENUM_METHODS: &[&str] = &[
        "map",
        "collect",
        "flat_map",
        "collect_concat",
        "select",
        "filter",
        "filter_map",
        "reject",
        "reduce",
        "inject",
        "to_a",
        "entries",
        "find",
        "detect",
        "find_index",
        "count",
        "min",
        "max",
        "minmax",
        "min_by",
        "max_by",
        "sort",
        "sort_by",
        "sum",
        "include?",
        "member?",
        "first",
        "take",
        "drop",
        "take_while",
        "drop_while",
        "each_with_index",
        "each_with_object",
        "group_by",
        "partition",
        "tally",
        "uniq",
        "zip",
        "any?",
        "all?",
        "none?",
        "one?",
        "each_slice",
        "each_cons",
        "chunk_while",
        "chunk",
        "slice_when",
        "reverse_each",
        "to_h",
    ];
    if !ENUM_METHODS.contains(&name) {
        return Ok(None);
    }
    let elems = enum_to_vec(recv)?;
    let arr = with_host(|h| h.new_array(elems));
    Ok(Some(dispatch_array(&arr, name, args, block)?))
}

fn as_i(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        Value::Float(f) => *f as i64,
        _ => 0,
    }
}

/// An integer argument as `i64` (an immediate or a small-enough BigInt), or
/// `None` when the value is not an integer at all (e.g. a Float).
fn int_arg(v: &Value) -> Option<i64> {
    use num_traits::ToPrimitive;
    match v {
        Value::Int(n) => Some(*n),
        Value::Obj(_) => with_host(|h| h.as_bigint(v)).and_then(|b| b.to_i64()),
        _ => None,
    }
}
fn as_f(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        // A heap number (BigInt / Rational) coerces to its f64 value, not 0 —
        // e.g. a Rational exponent in `5 ** (1/100**7r)`.
        _ => with_host(|h| {
            use num_traits::ToPrimitive as _;
            h.as_rational(v).and_then(|r| r.to_f64()).unwrap_or(0.0)
        }),
    }
}

/// The default tolerance for `Float#rationalize` with no argument: half the gap
/// to the adjacent representable f64, so the result is the simplest rational
/// that still rounds back to `f`.
fn default_rationalize_eps(f: f64) -> f64 {
    if f == 0.0 || !f.is_finite() {
        return 0.0;
    }
    let next = f64::from_bits(f.to_bits() + 1);
    (next - f).abs() * 0.5
}

/// The simplest rational within `eps` of `f` (the fraction with the smallest
/// denominator in `[f - eps, f + eps]`), via the continued-fraction / mediant
/// search Ruby uses for `rationalize`.
fn simplest_rational_within(f: f64, eps: f64) -> num_rational::BigRational {
    use num_bigint::BigInt;
    let rat = |n: i128, d: i128| num_rational::BigRational::new(BigInt::from(n), BigInt::from(d));
    if eps == 0.0 || !f.is_finite() {
        return num_rational::BigRational::from_float(f).unwrap_or_else(|| rat(0, 1));
    }
    if f < 0.0 {
        return -simplest_rational_within(-f, eps);
    }
    // Simplest fraction in the closed interval [lo, hi], with 0 <= lo <= hi.
    fn simplest(lo: f64, hi: f64) -> (i128, i128) {
        let fl = lo.floor();
        if fl >= lo {
            // `lo` is an integer — it is the simplest value in the interval.
            return (fl as i128, 1);
        }
        if fl < hi.floor() || hi.floor() >= hi {
            // An integer (fl + 1) lies within (lo, hi].
            return (fl as i128 + 1, 1);
        }
        // Recurse on the reciprocal of the fractional interval.
        let (n, d) = simplest(1.0 / (hi - fl), 1.0 / (lo - fl));
        (fl as i128 * n + d, n)
    }
    let (n, d) = simplest(f - eps, f + eps);
    rat(n, d)
}

/// Parse the leading rational of a string for `String#to_r`: `"3/4"` → `3/4`,
/// `"3.14"` → `157/50` (exact decimal), leading non-numeric text → `0/1`.
fn string_to_rational(s: &str) -> num_rational::BigRational {
    use num_bigint::BigInt;
    let zero = || num_rational::BigRational::from(BigInt::from(0));
    let s = s.trim_start();
    // `numerator/denominator` form.
    if let Some((n, d)) = s.split_once('/') {
        if let (Ok(n), Ok(d)) = (n.trim().parse::<BigInt>(), d.trim().parse::<BigInt>()) {
            if d != BigInt::from(0) {
                return num_rational::BigRational::new(n, d);
            }
        }
    }
    // Leading `[-]digits[.digits]` decimal.
    let mut end = 0;
    let bytes = s.as_bytes();
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    let int_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let mut frac_digits = 0usize;
    if end < bytes.len() && bytes[end] == b'.' {
        let dot = end;
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        frac_digits = end - dot - 1;
    }
    if end == int_start && frac_digits == 0 {
        return zero();
    }
    let digits: String = s[..end].chars().filter(|c| *c != '.').collect();
    match digits.parse::<BigInt>() {
        Ok(num) => {
            let den = BigInt::from(10).pow(frac_digits as u32);
            num_rational::BigRational::new(num, den)
        }
        Err(_) => zero(),
    }
}

/// Current wall-clock time as seconds since the Unix epoch, for `Time.now`.
fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Resolve the `(lo, hi)` bounds for a numeric `clamp`, accepting either the
/// two-argument form `clamp(lo, hi)` or the single-`Range` form `clamp(1..5)`.
/// An exclusive range is rejected, matching Ruby's `Comparable#clamp`.
/// Resolve clamp bounds. Either side is `None` for a beginless/endless range
/// (`5.clamp(..10)`, `5.clamp(3..)`), decoded from the range sentinels.
fn clamp_bounds(args: &[Value]) -> Result<(Option<Value>, Option<Value>), String> {
    if args.len() == 1 {
        if let Some((lo, hi, exclusive)) = with_host(|h| h.as_range(&args[0])) {
            if exclusive {
                return Err(raise_exc(
                    "ArgumentError",
                    "cannot clamp with an exclusive range",
                ));
            }
            let lo = (lo != crate::host::RANGE_BEGINLESS).then_some(Value::Int(lo));
            let hi = (hi != crate::host::RANGE_ENDLESS).then_some(Value::Int(hi));
            return Ok((lo, hi));
        }
        return Err(raise_exc("TypeError", "wrong argument type"));
    }
    Ok((Some(args[0].clone()), Some(args[1].clone())))
}

// ---- Integer / Float ------------------------------------------------------

/// Arbitrary-precision methods for a promoted `BigInt` receiver. Returns
/// `Ok(None)` for a method not handled here (so `dispatch_number` can try its
/// generic arms, which fall back to `<=>`/coerce behavior).
fn dispatch_bigint(
    b: &num_bigint::BigInt,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, String> {
    use num_integer::Integer as _;
    use num_traits::{Signed as _, ToPrimitive as _, Zero as _};
    let big = |v: num_bigint::BigInt| with_host(|h| h.new_bigint(v));
    let r = match name {
        "to_s" | "inspect" => {
            let radix = args.first().and_then(int_arg).unwrap_or(10);
            new_str(b.to_str_radix(radix as u32))
        }
        "to_i" | "to_int" | "floor" | "ceil" | "round" | "truncate" => big(b.clone()),
        "to_f" => Value::Float(b.to_f64().unwrap_or(f64::INFINITY)),
        "abs" | "magnitude" => big(b.abs()),
        "-@" => big(-b.clone()),
        "bit_length" => Value::Int(b.bits() as i64),
        "even?" => Value::Bool(b.is_even()),
        "odd?" => Value::Bool(b.is_odd()),
        "zero?" => Value::Bool(b.is_zero()),
        "positive?" => Value::Bool(b.is_positive()),
        "negative?" => Value::Bool(b.is_negative()),
        "integer?" => Value::Bool(true),
        "hash" => Value::Int(b.to_i64().unwrap_or(b.bits() as i64)),
        "succ" | "next" => big(b + 1),
        "pred" => big(b - 1),
        "digits" => {
            let base = args.first().and_then(int_arg).unwrap_or(10).max(2);
            let base = num_bigint::BigInt::from(base);
            let mut n = b.abs();
            let mut out = Vec::new();
            if n.is_zero() {
                out.push(Value::Int(0));
            }
            while n > num_bigint::BigInt::zero() {
                let (q, rem) = n.div_rem(&base);
                out.push(with_host(|h| h.new_bigint(rem)));
                n = q;
            }
            new_arr(out)
        }
        "/" | "div" => match with_host(|h| h.as_bigint(&args[0])) {
            Some(d) if !d.is_zero() => big(b.div_floor(&d)),
            Some(_) => return Err(raise_exc("ZeroDivisionError", "divided by 0")),
            None => return Ok(None),
        },
        "%" | "modulo" => match with_host(|h| h.as_bigint(&args[0])) {
            Some(d) if !d.is_zero() => big(b.mod_floor(&d)),
            Some(_) => return Err(raise_exc("ZeroDivisionError", "divided by 0")),
            None => return Ok(None),
        },
        "divmod" => match with_host(|h| h.as_bigint(&args[0])) {
            Some(d) if !d.is_zero() => {
                let (q, m) = b.div_mod_floor(&d);
                new_arr(vec![big(q), big(m)])
            }
            _ => return Err(raise_exc("ZeroDivisionError", "divided by 0")),
        },
        "<=>" => match with_host(|h| h.as_bigint(&args[0])) {
            Some(o) => Value::Int(match b.cmp(&o) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }),
            None => Value::Undef,
        },
        "coerce" => new_arr(vec![args[0].clone(), big(b.clone())]),
        _ => return Ok(None),
    };
    Ok(Some(r))
}

fn dispatch_number(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    // Promoted BigInt receivers need arbitrary-precision handling for the
    // methods that would otherwise truncate through `i64`.
    if let Some(b) = with_host(|h| h.as_promoted_bigint(recv)) {
        if let Some(r) = dispatch_bigint(&b, name, args)? {
            return Ok(r);
        }
    }
    match name {
        "times" => {
            let n = as_i(recv);
            if let Some(b) = &block {
                let mut i = 0;
                while i < n {
                    call_proc(b, &[Value::Int(i)])?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                    i += 1;
                }
                Ok(recv.clone())
            } else {
                // Block-less: an Enumerator over `0...n`, so `n.times.next` and
                // `n.times.to_a`/`.map` all work.
                Ok(with_host(|h| {
                    let items: Vec<Value> = (0..n.max(0)).map(Value::Int).collect();
                    h.new_enumerator(items, "each")
                }))
            }
        }
        "**" | "pow" => {
            // Two-arg `Integer#pow(e, mod)` is modular exponentiation.
            if name == "pow" && args.len() >= 2 {
                if let (Value::Int(base), Value::Int(exp), Value::Int(m)) =
                    (recv, &args[0], &args[1])
                {
                    if *m == 0 {
                        return Err(raise_exc("ZeroDivisionError", "divided by 0"));
                    }
                    if *exp >= 0 {
                        return Ok(Value::Int(mod_pow(*base, *exp, *m)));
                    }
                    // MRI rejects a negative exponent when a modulus is given
                    // (it does not compute a modular inverse).
                    return Err(raise_exc(
                        "RangeError",
                        "Integer#pow() 1st argument cannot be negative when 2nd argument specified",
                    ));
                }
            }
            // |base| == 1 short-circuits at any exponent magnitude/type (Ruby):
            // 1 ** x == 1 (Rational (1/1) for a Rational exponent); (-1) ** integer
            // is ±1 by parity. This also covers bignum exponents too big for i64.
            // A base of magnitude >= 2 raised to an integer exponent that overflows
            // i64 is rejected by Ruby — the result can't be materialized.
            if let Some(base) = with_host(|h| h.as_bigint(recv)) {
                use num_integer::Integer as _;
                use num_traits::{Signed as _, Zero as _};
                let one = || num_bigint::BigInt::from(1);
                // 0 ** 0 == 1; 0 ** positive == 0; 0 ** negative raises (a
                // negative Rational exponent too — `0 ** (-1/7r)`).
                if base.is_zero() {
                    if let Some(e) = with_host(|h| h.as_bigint(&args[0])) {
                        if e.is_zero() {
                            return Ok(Value::Int(1));
                        }
                        if e.is_positive() {
                            return Ok(Value::Int(0));
                        }
                        return Err(raise_exc("ZeroDivisionError", "divided by 0"));
                    }
                    if let Some(r) = with_host(|h| h.as_rational(&args[0])) {
                        if r.is_negative() {
                            return Err(raise_exc("ZeroDivisionError", "divided by 0"));
                        }
                        // 0 ** positive-Rational is the exact Rational zero (0/1),
                        // not a Float — the exponent is a Rational.
                        let zero =
                            num_rational::BigRational::new(num_bigint::BigInt::from(0), one());
                        return Ok(with_host(|h| h.new_rational(zero)));
                    }
                }
                if base == one() {
                    if with_host(|h| h.as_bigint(&args[0]).is_some()) {
                        return Ok(Value::Int(1));
                    }
                    if with_host(|h| h.as_rational(&args[0]).is_some()) {
                        let r = num_rational::BigRational::new(one(), one());
                        return Ok(with_host(|h| h.new_rational(r)));
                    }
                }
                if base == num_bigint::BigInt::from(-1) {
                    if let Some(e) = with_host(|h| h.as_bigint(&args[0])) {
                        return Ok(Value::Int(if e.is_even() { 1 } else { -1 }));
                    }
                }
                if (base > one() || base < num_bigint::BigInt::from(-1))
                    && int_arg(&args[0]).is_none()
                    && with_host(|h| {
                        h.as_bigint(&args[0])
                            .map(|e| e.abs() > one())
                            .unwrap_or(false)
                    })
                {
                    // An integer exponent of EITHER sign that overflows i64 makes
                    // base**exp (or its Rational reciprocal) too large to build.
                    return Err(raise_exc("ArgumentError", "exponent is too large"));
                }
            }
            // Integer ** non-negative Integer stays an exact Integer (promoting
            // to BigInt on overflow), like Ruby.
            if let (Some(base), Some(exp)) = (with_host(|h| h.as_bigint(recv)), int_arg(&args[0])) {
                // |base| >= 2 here (0 / ±1 short-circuited above). An exponent
                // magnitude that overflows u32 can't be materialized — Ruby raises,
                // and it would otherwise truncate in the `as u32` cast below.
                if exp.unsigned_abs() > u32::MAX as u64 {
                    return Err(raise_exc("ArgumentError", "exponent is too large"));
                }
                if exp >= 0 {
                    let r = base.pow(exp as u32);
                    return Ok(with_host(|h| h.new_bigint(r)));
                }
                // Integer ** -n is the exact Rational 1 / base**n (Ruby returns a
                // Rational, not a Float). 0 ** -n raises like division by zero.
                if base == num_bigint::BigInt::from(0) {
                    return Err(raise_exc("ZeroDivisionError", "divided by 0"));
                }
                // |base| == 1 yields an exact Integer ±1 (Ruby returns Integer,
                // not a Rational): 1 ** -n == 1, (-1) ** -n == 1 if n even else -1.
                if base == num_bigint::BigInt::from(1) {
                    return Ok(Value::Int(1));
                }
                if base == num_bigint::BigInt::from(-1) {
                    return Ok(Value::Int(if exp % 2 == 0 { 1 } else { -1 }));
                }
                let denom = base.pow((-exp) as u32);
                let r = num_rational::BigRational::new(num_bigint::BigInt::from(1), denom);
                return Ok(with_host(|h| h.new_rational(r)));
            }
            Ok(Value::Float(as_f(recv).powf(as_f(&args[0]))))
        }
        "/" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(floor_div(*a, *b))),
            // A numeric heap operand: `Integer / Rational` stays exact
            // (`3 / 4r == (3/4)`); `Integer / BigInt` is Integer floor division
            // (`7 / (42**42) == 0`). num_op picks the right one by operand type.
            _ if with_host(|h| h.as_rational(&args[0])).is_some()
                && matches!(recv, Value::Int(_)) =>
            {
                with_host(|h| h.num_op(fusevm::NumOp::Div, recv, &args[0]))
            }
            _ => Ok(Value::Float(as_f(recv) / as_f(&args[0]))),
        },
        "%" | "modulo" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(floor_mod(*a, *b))),
            // `Integer % Rational` stays exact (`10 % (1/729r) == (0/1)`), like `/`.
            _ if with_host(|h| h.as_rational(&args[0])).is_some()
                && matches!(recv, Value::Int(_)) =>
            {
                with_host(|h| h.num_op(fusevm::NumOp::Mod, recv, &args[0]))
            }
            _ => {
                let (x, y) = (as_f(recv), as_f(&args[0]));
                Ok(Value::Float(x - (x / y).floor() * y))
            }
        },
        "step" => {
            // All-Integer receiver/limit/step iterate over Integers; any Float
            // participant switches to Float stepping (matching Numeric#step).
            let all_int = matches!(recv, Value::Int(_))
                && matches!(&args[0], Value::Int(_))
                && args
                    .get(1)
                    .map(|v| matches!(v, Value::Int(_)))
                    .unwrap_or(true);
            if all_int {
                let limit = as_i(&args[0]);
                let by = args.get(1).map(as_i).unwrap_or(1);
                if by != 0 {
                    if let Some(bl) = &block {
                        let mut i = as_i(recv);
                        while (by > 0 && i <= limit) || (by < 0 && i >= limit) {
                            call_proc(bl, &[Value::Int(i)])?;
                            if has_pending_signal() {
                                if let Some(bv) = take_break() {
                                    return Ok(bv);
                                }
                                break;
                            }
                            i += by;
                        }
                        return Ok(recv.clone());
                    }
                    // Block-less: an Enumerator over the stepped values.
                    let mut vals = Vec::new();
                    let mut i = as_i(recv);
                    while (by > 0 && i <= limit) || (by < 0 && i >= limit) {
                        vals.push(Value::Int(i));
                        i += by;
                    }
                    return Ok(with_host(|h| h.new_enumerator(vals, "each")));
                }
            } else {
                let from = as_f(recv);
                let to = as_f(&args[0]);
                let by = args.get(1).map(as_f).unwrap_or(1.0);
                if by != 0.0 && by.is_finite() {
                    // Ruby's float-step count: floor((to-from)/by + err) + 1,
                    // yielding `from + i*by` with the final value clamped to `to`.
                    let mut err =
                        (from.abs() + to.abs() + (to - from).abs()) / by.abs() * f64::EPSILON;
                    if err > 0.5 {
                        err = 0.5;
                    }
                    let n = ((to - from) / by + err).floor() + 1.0;
                    let clamp = |i: f64| -> f64 {
                        let d = i * by + from;
                        if by >= 0.0 {
                            if d > to {
                                to
                            } else {
                                d
                            }
                        } else if d < to {
                            to
                        } else {
                            d
                        }
                    };
                    if let Some(bl) = &block {
                        let mut i = 0.0f64;
                        while i < n {
                            call_proc(bl, &[Value::Float(clamp(i))])?;
                            if has_pending_signal() {
                                if let Some(bv) = take_break() {
                                    return Ok(bv);
                                }
                                break;
                            }
                            i += 1.0;
                        }
                        return Ok(recv.clone());
                    }
                    // Block-less: an Enumerator over the stepped floats.
                    let mut vals = Vec::new();
                    let mut i = 0.0f64;
                    while i < n {
                        vals.push(Value::Float(clamp(i)));
                        i += 1.0;
                    }
                    return Ok(with_host(|h| h.new_enumerator(vals, "each")));
                }
            }
            Ok(recv.clone())
        }
        "upto" => iter_int_range(recv, as_i(&args[0]), 1, &block),
        "downto" => iter_int_range(recv, as_i(&args[0]), -1, &block),
        "to_s" => {
            // `Integer#to_s(base)` renders in the given radix (2..=36).
            if let (Value::Int(n), Some(base)) = (recv, args.first().map(as_i)) {
                if (2..=36).contains(&base) {
                    return Ok(new_str(to_radix(*n, base as u32)));
                }
            }
            Ok(with_host(|h| {
                let s = h.to_s(recv);
                h.new_string(s)
            }))
        }
        "to_i" | "to_int" | "floor" if name != "floor" => {
            // A non-finite Float can't convert to an Integer (Ruby FloatDomainError).
            if let Value::Float(f) = recv {
                if !f.is_finite() {
                    let name = if f.is_nan() {
                        "NaN"
                    } else if *f > 0.0 {
                        "Infinity"
                    } else {
                        "-Infinity"
                    };
                    return Err(raise_exc("FloatDomainError", name));
                }
                return Ok(float_to_int_value(f.trunc()));
            }
            Ok(Value::Int(as_i(recv)))
        }
        "to_f" => Ok(Value::Float(as_f(recv))),
        // `Integer#to_r` is `n/1`; `Float#to_r` is the *exact* rational the f64
        // represents (`0.5` → `1/2`, `0.1` → `3602879701896397/36028797018963968`).
        "to_r" => match recv {
            Value::Int(n) => Ok(with_host(|h| {
                h.new_rational(num_rational::BigRational::from(num_bigint::BigInt::from(
                    *n,
                )))
            })),
            Value::Float(f) => match num_rational::BigRational::from_float(*f) {
                Some(r) => Ok(with_host(|h| h.new_rational(r))),
                None => Err(raise_exc(
                    "FloatDomainError",
                    if f.is_nan() {
                        "NaN"
                    } else if *f > 0.0 {
                        "Infinity"
                    } else {
                        "-Infinity"
                    },
                )),
            },
            _ => Ok(recv.clone()),
        },
        // `rationalize([eps])` finds the simplest rational within `eps` of the
        // value (default: the tightest interval that still round-trips the f64).
        "rationalize" => {
            let f = as_f(recv);
            let eps = match args.first() {
                Some(v) => as_f(v).abs(),
                None => default_rationalize_eps(f),
            };
            let r = simplest_rational_within(f, eps);
            Ok(with_host(|h| h.new_rational(r)))
        }
        // `Numeric#to_c` is `(n+0i)`.
        "to_c" => Ok(with_host(|h| h.new_complex(recv.clone(), Value::Int(0)))),
        "coerce" => match (recv, &args[0]) {
            // Integer#coerce keeps both as Integer when the other is an Integer,
            // otherwise both become Float (matching Numeric#coerce).
            (Value::Int(a), Value::Int(b)) => Ok(new_arr(vec![Value::Int(*b), Value::Int(*a)])),
            _ => Ok(new_arr(vec![
                Value::Float(as_f(&args[0])),
                Value::Float(as_f(recv)),
            ])),
        },
        "abs" | "magnitude" => Ok(match recv {
            Value::Int(n) => Value::Int(n.abs()),
            Value::Float(f) => Value::Float(f.abs()),
            _ => recv.clone(),
        }),
        // `abs2` is the square of the magnitude (self * self), preserving the
        // Integer/Float distinction like Ruby's `Numeric#abs2`.
        "abs2" => Ok(match recv {
            Value::Int(n) => Value::Int(n * n),
            Value::Float(f) => Value::Float(f * f),
            _ => recv.clone(),
        }),
        "even?" => Ok(Value::Bool(as_i(recv) % 2 == 0)),
        "odd?" => Ok(Value::Bool(as_i(recv) % 2 != 0)),
        "zero?" => Ok(Value::Bool(as_f(recv) == 0.0)),
        // `nonzero?` returns self when non-zero, else nil (`Value::Undef`).
        "nonzero?" => Ok(if as_f(recv) != 0.0 {
            recv.clone()
        } else {
            Value::Undef
        }),
        "positive?" => Ok(Value::Bool(as_f(recv) > 0.0)),
        "negative?" => Ok(Value::Bool(as_f(recv) < 0.0)),
        // `Integer#integer?` is true; `Float#integer?` is false.
        "integer?" => Ok(Value::Bool(matches!(recv, Value::Int(_)))),
        "succ" | "next" => Ok(Value::Int(as_i(recv) + 1)),
        "pred" => Ok(Value::Int(as_i(recv) - 1)),
        "floor" => round_like(recv, args.first().map(as_i), f64::floor),
        "ceil" => round_like(recv, args.first().map(as_i), f64::ceil),
        "round" => round_like(recv, args.first().map(as_i), f64::round),
        "truncate" => round_like(recv, args.first().map(as_i), f64::trunc),
        "nan?" if matches!(recv, Value::Float(_)) => {
            Ok(Value::Bool(matches!(recv, Value::Float(f) if f.is_nan())))
        }
        "infinite?" => Ok(match recv {
            Value::Float(f) if f.is_infinite() => Value::Int(if *f > 0.0 { 1 } else { -1 }),
            _ => Value::Undef,
        }),
        "finite?" => Ok(Value::Bool(match recv {
            Value::Float(f) => f.is_finite(),
            _ => true,
        })),
        "divmod" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            (Value::Int(a), Value::Int(b)) => Ok(new_arr(vec![
                Value::Int(floor_div(*a, *b)),
                Value::Int(floor_mod(*a, *b)),
            ])),
            _ => {
                let (x, y) = (as_f(recv), as_f(&args[0]));
                let q = (x / y).floor();
                Ok(new_arr(vec![Value::Int(q as i64), Value::Float(x - q * y)]))
            }
        },
        "chr" => Ok(with_host(|h| {
            let c = (as_i(recv) as u8) as char;
            h.new_string(c.to_string())
        })),
        // These require exactly one argument; Ruby raises ArgumentError rather
        // than crashing when it is omitted.
        "gcd" | "lcm" | "gcdlcm" | "ceildiv" if args.is_empty() => Err(raise_exc(
            "ArgumentError",
            "wrong number of arguments (given 0, expected 1)",
        )),
        "gcd" => Ok(Value::Int(gcd(as_i(recv), as_i(&args[0])))),
        "lcm" => {
            let (a, b) = (as_i(recv), as_i(&args[0]));
            Ok(Value::Int(if a == 0 || b == 0 {
                0
            } else {
                (a / gcd(a, b) * b).abs()
            }))
        }
        "ceildiv" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            // Ruby: `n.ceildiv(d)` == `-(-n / d)` using floor division.
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(-floor_div(-*a, *b))),
            _ => Ok(Value::Int(
                as_f(recv).div_euclid(as_f(&args[0])).ceil() as i64
            )),
        },
        "gcdlcm" => {
            let (a, b) = (as_i(recv), as_i(&args[0]));
            let g = gcd(a, b);
            let l = if a == 0 || b == 0 {
                0
            } else {
                (a / g * b).abs()
            };
            Ok(new_arr(vec![Value::Int(g), Value::Int(l)]))
        }
        "[]" => {
            // `Integer#[i]` reads bit i of the two's-complement representation;
            // beyond the value's range positives read 0 and negatives read 1.
            let n = as_i(recv);
            let i = as_i(&args[0]);
            let bit = if i < 0 {
                0
            } else if i >= 64 {
                if n < 0 {
                    1
                } else {
                    0
                }
            } else {
                (n >> i) & 1
            };
            Ok(Value::Int(bit))
        }
        "digits" => {
            let mut n = as_i(recv);
            if n < 0 {
                // Ruby raises for a negative receiver rather than using abs.
                return Err(raise_exc("Math::DomainError", "out of domain"));
            }
            let base = args.first().map(as_i).unwrap_or(10).max(2);
            let mut out = Vec::new();
            if n == 0 {
                out.push(Value::Int(0));
            }
            while n > 0 {
                out.push(Value::Int(n % base));
                n /= base;
            }
            Ok(new_arr(out))
        }
        "bit_length" => {
            let n = as_i(recv);
            // Ruby: negative n has the bit length of ~n (its complement).
            let m = if n >= 0 { n as u64 } else { (!n) as u64 };
            Ok(Value::Int((64 - m.leading_zeros()) as i64))
        }
        // Bytes in the machine word (a fixnum is 8 bytes on a 64-bit build).
        "size" => Ok(Value::Int(8)),
        "fdiv" => Ok(Value::Float(as_f(recv) / as_f(&args[0]))),
        "clamp" => {
            // Ruby returns the receiver when in range, otherwise the bound
            // itself (preserving its type, so `(-2.7).clamp(-1.0, 1.0)` is a Float).
            // Accepts `clamp(lo, hi)` or `clamp(range)`; a beginless/endless range
            // clamps only one side. An exclusive range is rejected like
            // `Comparable#clamp`.
            let (lo, hi) = clamp_bounds(args)?;
            let x = as_f(recv);
            if let Some(lo) = &lo {
                if x < as_f(lo) {
                    return Ok(lo.clone());
                }
            }
            if let Some(hi) = &hi {
                if x > as_f(hi) {
                    return Ok(hi.clone());
                }
            }
            Ok(recv.clone())
        }
        "<=>" => {
            let (x, y) = (as_f(recv), as_f(&args[0]));
            Ok(match x.partial_cmp(&y) {
                Some(std::cmp::Ordering::Less) => Value::Int(-1),
                Some(std::cmp::Ordering::Equal) => Value::Int(0),
                Some(std::cmp::Ordering::Greater) => Value::Int(1),
                None => Value::Undef,
            })
        }
        // Bit operations promote to BigInt on overflow (`1 << 64`) and accept
        // already-promoted operands.
        "<<" | ">>" | "&" | "|" | "^" => {
            let (a, b) = (
                with_host(|h| h.as_bigint(recv)),
                with_host(|h| h.as_bigint(&args[0])),
            );
            match (a, b) {
                (Some(a), Some(b)) => {
                    use num_traits::ToPrimitive;
                    let r = match name {
                        "<<" => a << b.to_i64().unwrap_or(0).max(0) as usize,
                        ">>" => a >> b.to_i64().unwrap_or(0).max(0) as usize,
                        "&" => a & b,
                        "|" => a | b,
                        _ => a ^ b,
                    };
                    Ok(with_host(|h| h.new_bigint(r)))
                }
                _ => Err(raise_exc("TypeError", "no implicit conversion to Integer")),
            }
        }
        "between?" => {
            let n = as_f(recv);
            Ok(Value::Bool(n >= as_f(&args[0]) && n <= as_f(&args[1])))
        }
        // Arithmetic/comparison operators normally lower to native VM ops, so they
        // only reach here through an explicit send (`5.method(:+).call(3)`,
        // `5.send(:+, 3)`). Compute them the same way the VM would: an Int/Int pair
        // stays Integer, any Float operand promotes to Float.
        "+" | "-" | "*" | "<" | ">" | "<=" | ">=" => match (recv, &args[0]) {
            (Value::Int(x), Value::Int(y)) => Ok(match name {
                "+" => Value::Int(x + y),
                "-" => Value::Int(x - y),
                "*" => Value::Int(x * y),
                "<" => Value::Bool(x < y),
                ">" => Value::Bool(x > y),
                "<=" => Value::Bool(x <= y),
                _ => Value::Bool(x >= y),
            }),
            _ => {
                let (x, y) = (as_f(recv), as_f(&args[0]));
                Ok(match name {
                    "+" => Value::Float(x + y),
                    "-" => Value::Float(x - y),
                    "*" => Value::Float(x * y),
                    "<" => Value::Bool(x < y),
                    ">" => Value::Bool(x > y),
                    "<=" => Value::Bool(x <= y),
                    _ => Value::Bool(x >= y),
                })
            }
        },
        _ => Err(no_method_error(recv, name)),
    }
}

/// Shared impl for `Float#round/ceil/floor/truncate(ndigits)`.
///
/// Ruby returns a `Float` only when `ndigits > 0`; with no argument or a
/// non-positive count the result is an `Integer`. Integers are returned
/// unchanged unless `ndigits < 0` (round to a power of ten).
/// Convert a whole-valued f64 to a Ruby Integer, promoting past the i64 range to
/// a BigInt (`(2.0**70).round`, `1e20.to_i`) instead of saturating.
fn float_to_int_value(f: f64) -> Value {
    use num_traits::{FromPrimitive as _, ToPrimitive as _};
    match num_bigint::BigInt::from_f64(f.trunc()) {
        Some(b) => match b.to_i64() {
            Some(n) => Value::Int(n),
            None => with_host(|h| h.new_bigint(b)),
        },
        None => Value::Int(0),
    }
}

fn round_like(recv: &Value, ndigits: Option<i64>, op: fn(f64) -> f64) -> Result<Value, String> {
    match recv {
        Value::Float(f) => {
            // A non-finite Float raises FloatDomainError when the result would be
            // an Integer (round with no / negative ndigits, floor/ceil/truncate);
            // round(positive ndigits) returns a Float, so Infinity/NaN pass through.
            if !f.is_finite() {
                if matches!(ndigits, Some(d) if d > 0) {
                    return Ok(Value::Float(*f));
                }
                let name = if f.is_nan() {
                    "NaN"
                } else if *f > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                return Err(raise_exc("FloatDomainError", name));
            }
            Ok(match ndigits {
                Some(d) if d > 0 => {
                    // Natural IEEE rounding preserves the sign of a value that
                    // rounds to zero (`-0.01.round(1)` -> -0.0), matching MRI for
                    // normal magnitudes. (MRI returns +0.0 only in the deep
                    // underflow case where the value is far below the rounding
                    // unit — an implementation-defined dtoa quirk left as-is.)
                    let m = 10f64.powi(d as i32);
                    Value::Float(op(f * m) / m)
                }
                Some(d) if d < 0 => {
                    let m = 10f64.powi((-d) as i32);
                    float_to_int_value(op(f / m) * m)
                }
                _ => float_to_int_value(op(*f)),
            })
        }
        Value::Int(n) => Ok(match ndigits {
            Some(d) if d < 0 => {
                let m = 10f64.powi((-d) as i32);
                Value::Int((op(*n as f64 / m) * m) as i64)
            }
            _ => Value::Int(*n),
        }),
        _ => Ok(recv.clone()),
    }
}

fn iter_int_range(
    recv: &Value,
    bound: i64,
    step: i64,
    block: &Option<Value>,
) -> Result<Value, String> {
    let start = as_i(recv);
    if let Some(b) = block {
        let mut i = start;
        while (step > 0 && i <= bound) || (step < 0 && i >= bound) {
            call_proc(b, &[Value::Int(i)])?;
            if has_pending_signal() {
                if let Some(bv) = take_break() {
                    return Ok(bv);
                }
                break;
            }
            i += step;
        }
        return Ok(recv.clone());
    }
    // Block-less: an Enumerator over the range, so `n.upto(m).next` and
    // `n.upto(m).to_a` / `n.downto(m).map` all work.
    let mut vals = Vec::new();
    let mut i = start;
    while (step > 0 && i <= bound) || (step < 0 && i >= bound) {
        vals.push(Value::Int(i));
        i += step;
    }
    Ok(with_host(|h| h.new_enumerator(vals, "each")))
}

/// Ruby integer division floors toward negative infinity (`-7 / 2 == -4`).
fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    let r = a % b;
    if r != 0 && ((r < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Ruby integer modulo takes the sign of the divisor (`-7 % 3 == 2`).
fn floor_mod(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    }
}

/// Render an integer in base 2..=36 (Ruby `Integer#to_s(base)`).
fn to_radix(mut n: i64, base: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let neg = n < 0;
    let mut digits = Vec::new();
    let b = base as i64;
    n = n.abs();
    while n > 0 {
        let d = (n % b) as u32;
        digits.push(std::char::from_digit(d, base).unwrap());
        n /= b;
    }
    if neg {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Modular exponentiation `base**exp mod m` for `exp >= 0` (Ruby `Integer#pow`).
/// Uses i128 intermediates and floored modulo, so the result carries the sign of
/// `m` exactly as Ruby's floored `%` does.
fn mod_pow(base: i64, exp: i64, m: i64) -> i64 {
    let mm = m as i128;
    let fmod = |x: i128| {
        let r = x % mm;
        if r != 0 && ((r < 0) != (mm < 0)) {
            r + mm
        } else {
            r
        }
    };
    let mut b = fmod(base as i128);
    let mut e = exp;
    let mut r = fmod(1);
    while e > 0 {
        if e & 1 == 1 {
            r = fmod(r * b);
        }
        e >>= 1;
        if e > 0 {
            b = fmod(b * b);
        }
    }
    r as i64
}

/// Gamma function via the Lanczos approximation (g=7, n=9) with the reflection
/// formula for x < 0.5. Accurate to ~1e-9 on the tested domain; does NOT match
/// MRI's libm gamma bit-for-bit (excluded from the parity corpus).
fn math_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.5203681218851,
        -1259.1392167224028,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507343278686905,
        -0.13857109526572012,
        9.984_369_578_019_572e-6,
        1.5056327351493116e-7,
    ];
    if x < 0.5 {
        std::f64::consts::PI / ((std::f64::consts::PI * x).sin() * math_gamma(1.0 - x))
    } else {
        let x = x - 1.0;
        let t = x + 7.5;
        let mut a = G[0];
        for (i, g) in G.iter().enumerate().skip(1) {
            a += g / (x + i as f64);
        }
        (2.0 * std::f64::consts::PI).sqrt() * t.powf(x + 0.5) * (-t).exp() * a
    }
}

/// Error function via Abramowitz & Stegun 7.1.26 (max abs error ~1.5e-7). Does
/// NOT match MRI's libm erf bit-for-bit (excluded from the parity corpus).
fn math_erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t)
            * (-x * x).exp();
    sign * y
}

// ---- String ---------------------------------------------------------------

fn dispatch_string(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    frozen_guard(recv, name, STRING_MUTATORS)?;
    let s = with_host(|h| h.as_str(recv).unwrap_or_default());
    match name {
        // In-place bang variants delegate to the non-bang transform, write the
        // result back into the receiver, and return self — or nil when nothing
        // changed (Ruby's String mutator convention).
        "gsub!" | "sub!" | "upcase!" | "downcase!" | "capitalize!" | "swapcase!"
        | "strip!" | "lstrip!" | "rstrip!" | "chomp!" | "chop!" | "reverse!"
        | "squeeze!" | "tr!" | "tr_s!" | "delete!" => {
            let base = name.strip_suffix('!').unwrap();
            let result = dispatch_string(recv, base, args, block)?;
            let new = with_host(|h| h.as_str(&result).unwrap_or_default());
            let changed = new != s;
            with_host(|h| h.set_str(recv, new));
            Ok(if changed { recv.clone() } else { Value::Undef })
        }
        "length" | "size" => Ok(Value::Int(s.chars().count() as i64)),
        // The `:ascii` option restricts case conversion to the ASCII letters,
        // leaving non-ASCII code points (ß, ü, …) untouched.
        "upcase" => Ok(new_str(if is_ascii_case_opt(args) {
            s.to_ascii_uppercase()
        } else {
            s.to_uppercase()
        })),
        "downcase" => Ok(new_str(if is_ascii_case_opt(args) {
            s.to_ascii_lowercase()
        } else {
            s.to_lowercase()
        })),
        "swapcase" => {
            let out: String = s
                .chars()
                .map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().collect::<String>()
                    } else if c.is_lowercase() {
                        c.to_uppercase().collect::<String>()
                    } else {
                        c.to_string()
                    }
                })
                .collect();
            Ok(new_str(out))
        }
        "capitalize" => {
            let mut c = s.chars();
            let out = match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            };
            Ok(new_str(out))
        }
        "reverse" => Ok(new_str(s.chars().rev().collect())),
        "strip" => Ok(new_str(s.trim().to_string())),
        "lstrip" => Ok(new_str(s.trim_start().to_string())),
        "rstrip" => Ok(new_str(s.trim_end().to_string())),
        "chomp" => {
            let out = match args.first() {
                Some(a) => {
                    let sep = arg_str(a);
                    if sep.is_empty() {
                        // Paragraph mode: strip all trailing record separators.
                        s.trim_end_matches(['\n', '\r']).to_string()
                    } else {
                        s.strip_suffix(&sep).unwrap_or(&s).to_string()
                    }
                }
                None => {
                    // Remove one trailing "\r\n", "\n", or "\r".
                    if let Some(x) = s.strip_suffix("\r\n") {
                        x.to_string()
                    } else if let Some(x) = s.strip_suffix('\n').or_else(|| s.strip_suffix('\r')) {
                        x.to_string()
                    } else {
                        s.clone()
                    }
                }
            };
            Ok(new_str(out))
        }
        "chop" => {
            // Remove the last character; a trailing "\r\n" counts as one.
            let out = if let Some(x) = s.strip_suffix("\r\n") {
                x.to_string()
            } else {
                let mut c = s.chars();
                c.next_back();
                c.as_str().to_string()
            };
            Ok(new_str(out))
        }
        "chars" => Ok(new_arr(s.chars().map(|c| new_str(c.to_string())).collect())),
        // UAX #29 extended grapheme clusters (`"é"` counts as one even when stored
        // as base + combining mark). `grapheme_clusters` returns the array.
        "grapheme_clusters" => Ok(new_arr(
            UnicodeSegmentation::graphemes(s.as_str(), true)
                .map(|g| new_str(g.to_string()))
                .collect(),
        )),
        "each_grapheme_cluster" => {
            if let Some(b) = &block {
                for g in UnicodeSegmentation::graphemes(s.as_str(), true) {
                    call_proc(b, &[new_str(g.to_string())])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // Block-less: an Enumerator over the clusters, so external
                // iteration and chained Enumerable calls work.
                let gs: Vec<Value> = UnicodeSegmentation::graphemes(s.as_str(), true)
                    .map(|g| new_str(g.to_string()))
                    .collect();
                Ok(with_host(|h| h.new_enumerator(gs, "each")))
            }
        }
        // Unicode normalization to NFC (default), NFD, NFKC, or NFKD.
        "unicode_normalize" => {
            let form = args.first().map(arg_str).unwrap_or_else(|| "nfc".into());
            let out: String = match form.as_str() {
                "nfc" => s.nfc().collect(),
                "nfd" => s.nfd().collect(),
                "nfkc" => s.nfkc().collect(),
                "nfkd" => s.nfkd().collect(),
                other => {
                    return Err(raise_exc(
                        "ArgumentError",
                        &format!("Invalid normalization form {other}."),
                    ))
                }
            };
            Ok(new_str(out))
        }
        // Whether the string already equals its normalization (NFC default).
        "unicode_normalized?" => {
            let form = args.first().map(arg_str).unwrap_or_else(|| "nfc".into());
            let normalized: String = match form.as_str() {
                "nfc" => s.nfc().collect(),
                "nfd" => s.nfd().collect(),
                "nfkc" => s.nfkc().collect(),
                "nfkd" => s.nfkd().collect(),
                other => {
                    return Err(raise_exc(
                        "ArgumentError",
                        &format!("Invalid normalization form {other}."),
                    ))
                }
            };
            Ok(Value::Bool(normalized == s))
        }
        "bytes" => Ok(new_arr(s.bytes().map(|b| Value::Int(b as i64)).collect())),
        // `String#unpack(fmt)` / `#unpack1(fmt)` — the inverse of `Array#pack`.
        // The string is read as a byte sequence via the Latin-1 convention (each
        // codepoint's low byte), so a `pack`-produced binary string round-trips.
        "unpack" => {
            let fmt = arg_str(&args[0]);
            Ok(new_arr(unpack_bytes(&binstr_to_bytes(&s), &fmt)?))
        }
        "unpack1" => {
            let fmt = arg_str(&args[0]);
            let out = unpack_bytes(&binstr_to_bytes(&s), &fmt)?;
            Ok(out.into_iter().next().unwrap_or(Value::Undef))
        }
        // Number of bytes in the UTF-8 encoding (not the character count).
        "bytesize" => Ok(Value::Int(s.len() as i64)),
        // With a block, yield each byte and return self; without a block, yield
        // the bytes array so `.each_byte.to_a` works without a real Enumerator.
        "each_byte" => match &block {
            Some(bl) => {
                for b in s.bytes() {
                    call_proc(bl, &[Value::Int(b as i64)])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            }
            None => Ok(with_host(|h| {
                let bytes: Vec<Value> = s.bytes().map(|b| Value::Int(b as i64)).collect();
                h.new_enumerator(bytes, "each")
            })),
        },
        // Byte at index `i` (supports negatives); nil when out of range.
        "getbyte" => {
            let bytes = s.as_bytes();
            let raw = as_i(&args[0]);
            let idx = if raw < 0 {
                raw + bytes.len() as i64
            } else {
                raw
            };
            if idx < 0 || idx >= bytes.len() as i64 {
                Ok(Value::Undef)
            } else {
                Ok(Value::Int(bytes[idx as usize] as i64))
            }
        }
        // `b` returns a copy of the string tagged ASCII-8BIT. Byte content is
        // unchanged (we store UTF-8 bytes); the BINARY encoding is recorded in a
        // side table so `#encoding` on the copy answers ASCII-8BIT.
        "b" => {
            let copy = new_str(s.clone());
            with_host(|h| h.mark_binary_string(&copy));
            Ok(copy)
        }
        // True when every byte is 7-bit ASCII.
        "ascii_only?" => Ok(Value::Bool(s.is_ascii())),
        // We only carry valid UTF-8 Strings, so this is always true.
        "valid_encoding?" => Ok(Value::Bool(true)),
        // No real multi-encoding: `force_encoding` returns self but records/clears
        // the ASCII-8BIT tag so `#encoding` tracks it; `encode` is a copy shim.
        "force_encoding" => {
            // The argument is a name string or an `Encoding` object (whose `name`
            // ivar holds the canonical name).
            let raw = match with_host(|h| h.ivar_of(&args[0], "name")) {
                Value::Undef => arg_str(&args[0]),
                nm => arg_str(&nm),
            };
            match normalize_encoding_name(&raw).as_deref() {
                Some("ASCII-8BIT") => with_host(|h| h.mark_binary_string(recv)),
                _ => with_host(|h| h.unmark_binary_string(recv)),
            }
            Ok(recv.clone())
        }
        "encode" => Ok(new_str(s.clone())),
        // We store UTF-8 byte content; `encoding` names UTF-8 unless the string was
        // tagged ASCII-8BIT (`String#b` / `force_encoding("BINARY")`). The returned
        // Encoding object answers `name`/`to_s`/`inspect` (dispatched in
        // `dispatch_object` for the `Encoding` class).
        "encoding" => {
            if with_host(|h| h.is_binary_string(recv)) {
                Ok(encoding_object("ASCII-8BIT"))
            } else {
                Ok(encoding_object("UTF-8"))
            }
        }
        "lines" => Ok(new_arr(split_lines(&s).into_iter().map(new_str).collect())),
        "each_line" => {
            // With a block, iterate the lines and return self; without one,
            // return an Enumerator over the lines (external iteration + chains).
            match &block {
                Some(bl) => {
                    for line in split_lines(&s) {
                        call_proc(bl, &[new_str(line)])?;
                        if has_pending_signal() {
                            take_break();
                            break;
                        }
                    }
                    Ok(recv.clone())
                }
                None => {
                    let lines: Vec<Value> = split_lines(&s).into_iter().map(new_str).collect();
                    Ok(with_host(|h| h.new_enumerator(lines, "each")))
                }
            }
        }
        "center" => {
            let width = as_i(&args[0]).max(0) as usize;
            let padstr = pad_str(args);
            let len = s.chars().count();
            if len >= width || padstr.is_empty() {
                Ok(new_str(s.clone()))
            } else {
                let total = width - len;
                let left = total / 2;
                let right = total - left;
                let lp: String = padstr.chars().cycle().take(left).collect();
                let rp: String = padstr.chars().cycle().take(right).collect();
                Ok(new_str(format!("{lp}{s}{rp}")))
            }
        }
        "tr" => {
            let (neg, from) = expand_tr_spec(&arg_str(&args[0]), true)?;
            let (_, to) = expand_tr_spec(&arg_str(&args[1]), false)?;
            let out: String = s
                .chars()
                .filter_map(|c| tr_map(c, neg, &from, &to))
                .collect();
            Ok(new_str(out))
        }
        "delete" => {
            let m = char_matcher(args);
            Ok(new_str(s.chars().filter(|&c| !m(c)).collect()))
        }
        // `delete_prefix`/`delete_suffix` strip one exact leading/trailing match
        // (no globbing); a non-match returns an unchanged copy (MRI:
        // `"hello".delete_prefix("hel")`→`"lo"`, `.delete_suffix("xyz")`→`"hello"`).
        "delete_prefix" => {
            let p = arg_str(&args[0]);
            Ok(new_str(s.strip_prefix(&p).unwrap_or(&s).to_string()))
        }
        "delete_suffix" => {
            let p = arg_str(&args[0]);
            Ok(new_str(s.strip_suffix(&p).unwrap_or(&s).to_string()))
        }
        "count" => {
            let m = char_matcher(args);
            Ok(Value::Int(s.chars().filter(|&c| m(c)).count() as i64))
        }
        "squeeze" => {
            let m = char_matcher(args);
            let all = args.is_empty();
            let mut prev: Option<char> = None;
            let mut out = String::new();
            for c in s.chars() {
                if Some(c) == prev && (all || m(c)) {
                    continue;
                }
                out.push(c);
                prev = Some(c);
            }
            Ok(new_str(out))
        }
        "empty?" => Ok(Value::Bool(s.is_empty())),
        "to_i" => {
            let base = args.first().map(as_i).unwrap_or(10);
            if base != 0 && !(2..=36).contains(&base) {
                return Err(raise_exc("ArgumentError", &format!("invalid radix {base}")));
            }
            Ok(scan_int_value(&s, base).unwrap_or(Value::Int(0)))
        }
        "hex" => Ok(scan_int_value(&s, 16).unwrap_or(Value::Int(0))),
        "oct" => {
            // Ruby `String#oct` defaults to base 8 but honours a
            // `0x`/`0b`/`0o`/`0d` prefix, which flips it to auto-detect.
            let base = if has_radix_prefix(&s) { 0 } else { 8 };
            Ok(scan_int_value(&s, base).unwrap_or(Value::Int(0)))
        }
        "to_f" => Ok(Value::Float(parse_leading_float(&s))),
        // `String#to_r` parses a leading rational: `"3/4"` → `3/4`, `"3.14"` →
        // `157/50` (exact decimal), other leading text → `0/1`.
        "to_r" => Ok(with_host(|h| h.new_rational(string_to_rational(&s)))),
        "to_s" | "to_str" => Ok(recv.clone()),
        "to_sym" => Ok(with_host(|h| h.new_symbol(&s))),
        "include?" => Ok(Value::Bool(s.contains(&arg_str(&args[0])))),
        "start_with?" => Ok(Value::Bool(args.iter().any(|a| {
            match str_regex(a) {
                // A Regexp prefix matches when it matches at the very start.
                Some(re) => re
                    .find(&s)
                    .ok()
                    .flatten()
                    .map(|m| m.start() == 0)
                    .unwrap_or(false),
                None => s.starts_with(&arg_str(a)),
            }
        }))),
        "end_with?" => Ok(Value::Bool(args.iter().any(|a| s.ends_with(&arg_str(a))))),
        "match?" => {
            let m = str_regex(&args[0])
                .map(|re| re.is_match(&s).unwrap_or(false))
                .unwrap_or(false);
            Ok(Value::Bool(m))
        }
        "=~" => match str_regex(&args[0]) {
            // `=~` sets `$~`/`$1`.. as a side effect, then yields the char offset.
            Some(re) => {
                match_data(&re, &s);
                Ok(re
                    .find(&s)
                    .ok()
                    .flatten()
                    .map(|m| Value::Int(s[..m.start()].chars().count() as i64))
                    .unwrap_or(Value::Undef))
            }
            None => Ok(Value::Undef),
        },
        "match" => match str_regex(&args[0]) {
            Some(re) => Ok(match_data(&re, &s)),
            None => Ok(Value::Undef),
        },
        "scan" => match str_regex(&args[0]) {
            // With a block, yield each match and return the string (self); each
            // yielded value is the whole match, or the capture-group array for a
            // grouped pattern. Without a block, collect them into an array.
            Some(re) => match &block {
                Some(bl) => {
                    scan_each(&re, &s, bl)?;
                    Ok(recv.clone())
                }
                None => Ok(scan_regex(&re, &s)),
            },
            None => Ok(new_arr(vec![])),
        },
        "split" => {
            let limit = args.get(1).map(as_i).unwrap_or(0);
            // Awk mode: no separator, or a single-space string. Splits on runs of
            // whitespace, ignoring leading whitespace and (unless a limit is
            // given) trailing empty fields.
            let awk =
                args.is_empty() || (str_regex(&args[0]).is_none() && arg_str(&args[0]) == " ");
            let parts: Vec<String> = if awk {
                if limit > 0 {
                    // Keep at most `limit` fields; the last holds the remainder.
                    let trimmed = s.trim_start();
                    split_ws_limit(trimmed, limit as usize)
                } else {
                    s.split_whitespace().map(str::to_string).collect()
                }
            } else if let Some(re) = str_regex(&args[0]) {
                regex_split(&re, &s, limit)
            } else {
                let sep = arg_str(&args[0]);
                if sep.is_empty() {
                    s.chars().map(|c| c.to_string()).collect()
                } else {
                    string_split(&s, &sep, limit)
                }
            };
            Ok(new_arr(parts.into_iter().map(new_str).collect()))
        }
        "sub" | "gsub" => {
            let all = name == "gsub";
            if let Some(re) = str_regex(&args[0]) {
                return regex_replace(&re, &s, &args[1..], &block, all);
            }
            let from = arg_str(&args[0]);
            let to = arg_str(&args[1]);
            Ok(new_str(if all {
                s.replace(&from, &to)
            } else {
                s.replacen(&from, &to, 1)
            }))
        }
        "replace" => {
            let n = arg_str(&args[0]);
            with_host(|h| h.set_str(recv, n));
            Ok(recv.clone())
        }
        "<<" | "+" => {
            let other = with_host(|h| h.to_s(&args[0]));
            if name == "+" {
                Ok(new_str(format!("{s}{other}")))
            } else {
                with_host(|h| h.set_str(recv, format!("{s}{other}")));
                Ok(recv.clone())
            }
        }
        // `concat(*strs)` appends every argument in order, mutating and
        // returning the receiver: `"a".concat("b", "c")` => "abc".
        "concat" => {
            let joined: String = args.iter().map(|a| with_host(|h| h.to_s(a))).collect();
            with_host(|h| h.set_str(recv, format!("{s}{joined}")));
            Ok(recv.clone())
        }
        "<=>" => match with_host(|h| h.as_str(&args[0])) {
            Some(other) => Ok(Value::Int(match s.cmp(&other) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })),
            None => Ok(Value::Undef),
        },
        "between?" => {
            let (lo, hi) = (arg_str(&args[0]), arg_str(&args[1]));
            Ok(Value::Bool(s >= lo && s <= hi))
        }
        "clamp" => {
            // `clamp(lo, hi)` or `clamp("a".."z")`; exclusive ranges are rejected
            // just as `Comparable#clamp` does.
            let (lo, hi) = if args.len() == 1 {
                match with_host(|h| h.as_str_range(&args[0])) {
                    Some((lo, hi, exclusive)) => {
                        if exclusive {
                            return Err(raise_exc(
                                "ArgumentError",
                                "cannot clamp with an exclusive range",
                            ));
                        }
                        (lo, hi)
                    }
                    None => return Err(raise_exc("TypeError", "wrong argument type")),
                }
            } else {
                (arg_str(&args[0]), arg_str(&args[1]))
            };
            if s < lo {
                Ok(new_str(lo))
            } else if s > hi {
                Ok(new_str(hi))
            } else {
                Ok(recv.clone())
            }
        }
        "*" => Ok(new_str(s.repeat(as_i(&args[0]).max(0) as usize))),
        "%" => {
            // `"%d-%d" % [1, 2]` or `"%d" % 5`; a Hash operand feeds named
            // references `%<name>s` / `%{name}`.
            if let Some(map) = with_host(|h| h.as_hash(&args[0])) {
                Ok(new_str(sprintf(&s, &[], Some(&map))))
            } else {
                let fargs =
                    with_host(|h| h.as_array(&args[0])).unwrap_or_else(|| vec![args[0].clone()]);
                Ok(new_str(sprintf(&s, &fargs, None)))
            }
        }
        "ljust" => Ok(new_str(pad(
            &s,
            as_i(&args[0]).max(0) as usize,
            pad_str(args),
            true,
        ))),
        "rjust" => Ok(new_str(pad(
            &s,
            as_i(&args[0]).max(0) as usize,
            pad_str(args),
            false,
        ))),
        "each_char" => {
            if let Some(b) = &block {
                for c in s.chars() {
                    call_proc(b, &[new_str(c.to_string())])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // Block-less: an Enumerator over the characters, so external
                // iteration (`next`) and chained Enumerable calls work.
                let chars: Vec<Value> = s.chars().map(|c| new_str(c.to_string())).collect();
                Ok(with_host(|h| h.new_enumerator(chars, "each")))
            }
        }
        "[]" => Ok(str_index(&s, args)),
        "slice" => Ok(str_index(&s, args)),
        // `eql?` is content equality with no type coercion: only another String
        // (not a Symbol) can be equal.
        "eql?" => Ok(Value::Bool(
            with_host(|h| h.as_str(&args[0]))
                .map(|o| o == s)
                .unwrap_or(false),
        )),
        // `slice!` removes the sliced portion in place and returns it.
        "slice!" => match str_slice_remove(&s, args) {
            Some((removed, rest)) => {
                with_host(|h| h.set_str(recv, rest));
                Ok(new_str(removed))
            }
            None => Ok(Value::Undef),
        },
        "ord" => match s.chars().next() {
            Some(c) => Ok(Value::Int(c as i64)),
            None => Err(raise_exc("ArgumentError", "empty string")),
        },
        "chr" => Ok(new_str(
            s.chars().next().map(|c| c.to_string()).unwrap_or_default(),
        )),
        // `"a-b-c".partition("-")` => ["a", "-", "b-c"]; no match => [whole, "", ""].
        "partition" => {
            let sep = arg_str(&args[0]);
            Ok(match s.find(&sep) {
                Some(i) => new_arr(vec![
                    new_str(s[..i].to_string()),
                    new_str(sep.clone()),
                    new_str(s[i + sep.len()..].to_string()),
                ]),
                None => new_arr(vec![
                    new_str(s.clone()),
                    new_str(String::new()),
                    new_str(String::new()),
                ]),
            })
        }
        // `rpartition` splits on the LAST occurrence; no match => ["", "", whole].
        "rpartition" => {
            let sep = arg_str(&args[0]);
            Ok(match s.rfind(&sep) {
                Some(i) => new_arr(vec![
                    new_str(s[..i].to_string()),
                    new_str(sep.clone()),
                    new_str(s[i + sep.len()..].to_string()),
                ]),
                None => new_arr(vec![
                    new_str(String::new()),
                    new_str(String::new()),
                    new_str(s.clone()),
                ]),
            })
        }
        // Case-insensitive compare: `casecmp` => -1/0/1, `casecmp?` => bool.
        "casecmp" => {
            let o = arg_str(&args[0]);
            let ord = s.to_lowercase().cmp(&o.to_lowercase());
            Ok(Value::Int(ord as i64))
        }
        "casecmp?" => {
            let o = arg_str(&args[0]);
            Ok(Value::Bool(s.to_lowercase() == o.to_lowercase()))
        }
        // `tr_s`: translate like `tr`, then squeeze runs of chars that were
        // translated (adjacent duplicates produced by the translation collapse).
        "tr_s" => {
            let (neg, from) = expand_tr_spec(&arg_str(&args[0]), true)?;
            let (_, to) = expand_tr_spec(&arg_str(&args[1]), false)?;
            let mut out = String::new();
            let mut last_translated: Option<char> = None;
            for c in s.chars() {
                let matched = if neg {
                    !from.contains(&c)
                } else {
                    from.contains(&c)
                };
                if matched {
                    // Translated (or deleted when `to` is empty); squeeze runs
                    // of the same replacement char.
                    if let Some(r) = tr_map(c, neg, &from, &to) {
                        if last_translated != Some(r) {
                            out.push(r);
                        }
                        last_translated = Some(r);
                    }
                } else {
                    out.push(c);
                    last_translated = None;
                }
            }
            Ok(new_str(out))
        }
        "succ" | "next" => Ok(new_str(str_succ(&s))),
        "insert" => {
            let mut chars: Vec<char> = s.chars().collect();
            let i = as_i(&args[0]);
            // Ruby inserts BEFORE index `i`; a negative index counts from the
            // end and inserts AFTER that character (so -1 appends).
            let at = if i < 0 {
                (chars.len() as i64 + i + 1).max(0) as usize
            } else {
                i as usize
            };
            if at > chars.len() {
                return Err(raise_exc("IndexError", &format!("index {i} out of string")));
            }
            chars.splice(at..at, arg_str(&args[1]).chars());
            let out: String = chars.into_iter().collect();
            with_host(|h| h.set_str(recv, out));
            Ok(recv.clone())
        }
        "prepend" => {
            let pre: String = args.iter().map(arg_str).collect();
            with_host(|h| h.set_str(recv, format!("{pre}{s}")));
            Ok(recv.clone())
        }
        "index" => Ok(str_find(
            &s,
            &arg_str(&args[0]),
            args.get(1).map(as_i),
            false,
        )),
        "rindex" => Ok(str_find(
            &s,
            &arg_str(&args[0]),
            args.get(1).map(as_i),
            true,
        )),
        "[]=" => str_index_set(recv, &s, args),
        _ => Err(no_method_error(recv, name)),
    }
}

/// Ruby `String#index`/`rindex`: the char offset of substring `needle`, or `nil`.
/// `pos` is an optional start (index) or end (rindex) char offset; `rev` picks
/// the last match instead of the first.
fn str_find(s: &str, needle: &str, pos: Option<i64>, rev: bool) -> Value {
    let byte_to_char = |b: usize| s[..b].chars().count() as i64;
    let hit = if rev {
        match pos {
            Some(p) => {
                let len = s.chars().count() as i64;
                let end = if p < 0 { len + p } else { p };
                if end < 0 {
                    None
                } else {
                    // Search only the prefix up to (and including) char `end`.
                    let cut: String = s.chars().take((end as usize) + 1).collect();
                    cut.rfind(needle).map(byte_to_char)
                }
            }
            None => s.rfind(needle).map(byte_to_char),
        }
    } else {
        match pos {
            Some(p) => {
                let len = s.chars().count() as i64;
                let start = if p < 0 { len + p } else { p };
                if start < 0 || start > len {
                    None
                } else {
                    let bstart = s
                        .char_indices()
                        .nth(start as usize)
                        .map(|(b, _)| b)
                        .unwrap_or(s.len());
                    s[bstart..].find(needle).map(|b| byte_to_char(bstart + b))
                }
            }
            None => s.find(needle).map(byte_to_char),
        }
    };
    hit.map(Value::Int).unwrap_or(Value::Undef)
}

/// Ruby `String#[]=`: replace the char range selected by the index args (int,
/// int+len, or Range) with the trailing replacement string, mutating `recv`.
fn str_index_set(recv: &Value, s: &str, args: &[Value]) -> Result<Value, String> {
    let (sel, repl) = args.split_at(args.len() - 1);
    let val = arg_str(&repl[0]);
    let mut chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let (start, end) = match sel {
        [Value::Int(i)] => {
            let k = match norm_idx(*i, len) {
                Some(k) if k < len => k,
                _ => return Err(raise_exc("IndexError", &format!("index {i} out of string"))),
            };
            (k, k + 1)
        }
        [Value::Int(i), Value::Int(n)] => {
            let st = match norm_idx(*i, len) {
                Some(k) if k <= len => k,
                _ => return Err(raise_exc("IndexError", &format!("index {i} out of string"))),
            };
            let e = (st + (*n).max(0) as usize).min(len);
            (st, e)
        }
        [rng] => match with_host(|h| h.as_range(rng)) {
            Some((lo, hi, excl)) => {
                let st = norm_idx(lo, len).unwrap_or(len).min(len);
                let mut e = norm_idx(hi, len).unwrap_or(len);
                if !excl {
                    e += 1;
                }
                let e = e.clamp(st, len);
                (st, e)
            }
            None => return Err("String#[]= bad index".to_string()),
        },
        _ => return Err("String#[]= bad index".to_string()),
    };
    chars.splice(start..end, val.chars());
    let out: String = chars.into_iter().collect();
    with_host(|h| h.set_str(recv, out));
    Ok(repl[0].clone())
}

/// The compiled regex for a value that is a Regexp, else `None`.
fn str_regex(v: &Value) -> Option<fancy_regex::Regex> {
    with_host(|h| h.as_regex(v)).map(|(re, _)| re)
}

/// Build a `MatchData` value for the first match of `re` in `s`, or `nil`, and
/// update the match globals (`$~`, `$&`, `` $` ``, `$'`, `$+`, `$1`..`$9`).
fn match_data(re: &fancy_regex::Regex, s: &str) -> Value {
    // fancy-regex's backtracking `captures` returns a Result; a match error is
    // treated as no match (MRI raises nothing here — it just fails to match).
    let caps = re.captures(s).ok().flatten();
    set_match_globals(caps.as_ref().map(|c| (c, s)), re)
}

/// `(name, group_index)` for each named capture `(?<name>…)` in `re`, in group
/// order. fancy-regex reports the whole group-name table via `capture_names()`;
/// index 0 (the whole match) and unnamed groups yield `None` and are skipped.
fn capture_name_map(re: &fancy_regex::Regex) -> Vec<(String, usize)> {
    re.capture_names()
        .enumerate()
        .filter_map(|(i, n)| n.map(|n| (n.to_string(), i)))
        .collect()
}

/// Set the Ruby match globals from a set of captures (or clear them to `nil` on a
/// failed match), and return the corresponding `MatchData` value (or `nil`).
/// Ruby names these `$~` (the MatchData), `$&` (whole match), `` $` ``/`$'`
/// (pre/post text), `$+` (last matched group), and `$1`..`$9` (numbered groups).
fn set_match_globals(m: Option<(&fancy_regex::Captures, &str)>, re: &fancy_regex::Regex) -> Value {
    with_host(|h| {
        // Clear the numbered globals first so a failed match leaves no stale
        // captures behind.
        for n in 1..=9 {
            h.set_global(&n.to_string(), Value::Undef);
        }
        let Some((c, s)) = m else {
            for g in ["~", "&", "`", "'", "+"] {
                h.set_global(g, Value::Undef);
            }
            return Value::Undef;
        };
        let whole = c.get(0).unwrap();
        let groups: Vec<Option<String>> = (0..c.len())
            .map(|i| c.get(i).map(|g| g.as_str().to_string()))
            .collect();
        // Build each global's string value first, then store — never borrow the
        // host mutably twice in one expression.
        let to_val = |h: &mut RubyHost, o: &Option<String>| -> Value {
            o.clone().map(|s| h.new_string(s)).unwrap_or(Value::Undef)
        };
        let pre = s[..whole.start()].to_string();
        let post = s[whole.end()..].to_string();
        let md = h.new_matchdata(
            groups.clone(),
            capture_name_map(re),
            pre.clone(),
            post.clone(),
        );
        h.set_global("~", md.clone());
        let g0 = to_val(h, groups.first().unwrap_or(&None));
        h.set_global("&", g0);
        let pre_v = h.new_string(pre);
        h.set_global("`", pre_v);
        let post_v = h.new_string(post);
        h.set_global("'", post_v);
        // `$+` is the last group that actually matched.
        let last = groups
            .iter()
            .skip(1)
            .rposition(|g| g.is_some())
            .map(|i| i + 1);
        let plus = to_val(h, last.and_then(|i| groups.get(i)).unwrap_or(&None));
        h.set_global("+", plus);
        for i in 1..=9 {
            let v = to_val(h, groups.get(i).unwrap_or(&None));
            h.set_global(&i.to_string(), v);
        }
        md
    })
}

/// `MatchData#[n]`, `#pre_match`, `#post_match`, `#to_a`, `#captures`, `#to_s`.
fn dispatch_matchdata(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let (groups, names, pre, post) = match with_host(|h| h.as_matchdata(recv)) {
        Some(t) => t,
        None => return Err("not a MatchData".to_string()),
    };
    let strv = |o: &Option<String>| o.clone().map(new_str).unwrap_or(Value::Undef);
    match name {
        "[]" => {
            // A Symbol or String key selects a named capture (?<name>…); an
            // integer key selects by group number (negative counts from the end).
            if let Some(key) = with_host(|h| h.as_symbol(&args[0]).or_else(|| h.as_str(&args[0]))) {
                return match names.iter().find(|(n, _)| *n == key) {
                    Some((_, idx)) => Ok(groups.get(*idx).map(strv).unwrap_or(Value::Undef)),
                    None => Err(raise_exc(
                        "IndexError",
                        &format!("undefined group name reference: {key}"),
                    )),
                };
            }
            let i = as_i(&args[0]);
            let idx = if i < 0 { groups.len() as i64 + i } else { i };
            if idx < 0 {
                return Ok(Value::Undef);
            }
            Ok(groups.get(idx as usize).map(strv).unwrap_or(Value::Undef))
        }
        "pre_match" => Ok(new_str(pre)),
        "post_match" => Ok(new_str(post)),
        "to_a" => Ok(new_arr(groups.iter().map(strv).collect())),
        "captures" => Ok(new_arr(groups.iter().skip(1).map(strv).collect())),
        "to_s" => Ok(strv(groups.first().unwrap_or(&None))),
        "size" | "length" => Ok(Value::Int(groups.len() as i64)),
        // `#names` — the (?<name>…) capture names in declaration order.
        "names" => Ok(new_arr(
            names.iter().map(|(n, _)| new_str(n.clone())).collect(),
        )),
        // `#named_captures` — a Hash mapping each named-capture name (String key,
        // per MRI) to its captured substring (nil when the group did not match).
        "named_captures" => {
            let mut map: IndexMap<RKey, Value> = IndexMap::new();
            for (n, idx) in &names {
                map.insert(
                    RKey::Str(n.clone()),
                    groups.get(*idx).map(strv).unwrap_or(Value::Undef),
                );
            }
            Ok(with_host(|h| h.new_hash(map)))
        }
        _ => Err(no_method_error(recv, name)),
    }
}

/// `String#scan(re)`: every match. With capture groups, each element is an array
/// of the captured groups; otherwise each element is the whole matched string.
fn scan_regex(re: &fancy_regex::Regex, s: &str) -> Value {
    let ngroups = re.captures_len(); // includes the whole-match group 0
    if ngroups <= 1 {
        // fancy-regex iterators yield `Result` (backtracking can error); a match
        // error ends the scan, so drop errored items with `filter_map(Result::ok)`.
        let out: Vec<Value> = re
            .find_iter(s)
            .filter_map(Result::ok)
            .map(|m| new_str(m.as_str().to_string()))
            .collect();
        new_arr(out)
    } else {
        let out: Vec<Value> = re
            .captures_iter(s)
            .filter_map(Result::ok)
            .map(|c| {
                let groups: Vec<Value> = (1..ngroups)
                    .map(|i| {
                        c.get(i)
                            .map(|m| new_str(m.as_str().to_string()))
                            .unwrap_or(Value::Undef)
                    })
                    .collect();
                new_arr(groups)
            })
            .collect();
        new_arr(out)
    }
}

/// Awk-mode split keeping at most `limit` fields (the last holds the remainder,
/// with its internal whitespace intact). `s` should already be left-trimmed.
fn split_ws_limit(s: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while out.len() + 1 < limit {
        match rest.find(char::is_whitespace) {
            Some(i) => {
                out.push(rest[..i].to_string());
                rest = rest[i..].trim_start();
            }
            None => break,
        }
    }
    if !rest.is_empty() || !out.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Split `s` on the string `sep` with Ruby's limit rules: `limit > 0` keeps at
/// most `limit` fields (last is the remainder); `limit == 0` drops trailing empty
/// fields; `limit < 0` keeps them all. An empty subject yields no fields.
fn string_split(s: &str, sep: &str, limit: i64) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<String> = if limit > 0 {
        s.splitn(limit as usize, sep).map(str::to_string).collect()
    } else {
        s.split(sep).map(str::to_string).collect()
    };
    if limit == 0 {
        while parts.last().is_some_and(|p| p.is_empty()) {
            parts.pop();
        }
    }
    parts
}

/// Split `s` on a regex. Capture groups in the pattern are interleaved into the
/// result (Ruby behavior). `limit` follows the same rules as `string_split`.
fn regex_split(re: &fancy_regex::Regex, s: &str, limit: i64) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut last = 0;
    let ngroups = re.captures_len();
    for caps in re.captures_iter(s).filter_map(Result::ok) {
        if limit > 0 && out.len() as i64 >= limit - 1 {
            break;
        }
        let m = caps.get(0).unwrap();
        // A zero-width match would loop forever; skip it.
        if m.start() == m.end() {
            continue;
        }
        out.push(s[last..m.start()].to_string());
        for i in 1..ngroups {
            if let Some(g) = caps.get(i) {
                out.push(g.as_str().to_string());
            }
        }
        last = m.end();
    }
    out.push(s[last..].to_string());
    if limit == 0 {
        while out.last().is_some_and(|p| p.is_empty()) {
            out.pop();
        }
    }
    out
}

/// `String#scan(re) { ... }`: yield each match (setting `$~`), passing the whole
/// match for an ungrouped pattern or the capture-group array for a grouped one.
fn scan_each(re: &fancy_regex::Regex, s: &str, bl: &Value) -> Result<(), String> {
    let ngroups = re.captures_len();
    for caps in re.captures_iter(s).filter_map(Result::ok) {
        set_match_globals(Some((&caps, s)), re);
        let arg = if ngroups <= 1 {
            new_str(caps.get(0).unwrap().as_str().to_string())
        } else {
            let groups: Vec<Value> = (1..ngroups)
                .map(|i| {
                    caps.get(i)
                        .map(|m| new_str(m.as_str().to_string()))
                        .unwrap_or(Value::Undef)
                })
                .collect();
            new_arr(groups)
        };
        call_proc(bl, &[arg])?;
    }
    Ok(())
}

/// `String#sub`/`gsub` with a Regexp: a replacement string (with `\1` group
/// refs) or a block that receives each match.
fn regex_replace(
    re: &fancy_regex::Regex,
    s: &str,
    rest: &[Value],
    block: &Option<Value>,
    all: bool,
) -> Result<Value, String> {
    let mut out = String::new();
    let mut last = 0;
    for (count, caps) in re.captures_iter(s).filter_map(Result::ok).enumerate() {
        if !all && count >= 1 {
            break;
        }
        let m = caps.get(0).unwrap();
        out.push_str(&s[last..m.start()]);
        if let Some(bl) = block {
            // Expose `$~`/`$1`.. to the block for the current match.
            set_match_globals(Some((&caps, s)), re);
            let r = call_proc(bl, &[new_str(m.as_str().to_string())])?;
            out.push_str(&with_host(|h| h.to_s(&r)));
        } else if let Some(map) = rest.first().and_then(|v| with_host(|h| h.as_hash(v))) {
            // `gsub(re, hash)`: each match is replaced by `hash[match]` (empty for
            // a key the hash lacks).
            let matched = new_str(m.as_str().to_string());
            let key = with_host(|h| h.value_to_key(&matched));
            if let Some(v) = map.get(&key) {
                out.push_str(&with_host(|h| h.to_s(v)));
            }
        } else {
            let repl = arg_str(&rest[0]);
            out.push_str(&expand_backrefs(&repl, &caps));
        }
        last = m.end();
    }
    out.push_str(&s[last..]);
    Ok(new_str(out))
}

/// Expand `\1`..`\9` (and `\0`) group back-references in a replacement string.
fn expand_backrefs(repl: &str, caps: &fancy_regex::Captures) -> String {
    let mut out = String::new();
    let mut chars = repl.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                chars.next();
                out.push_str(caps.get(d as usize).map(|m| m.as_str()).unwrap_or(""));
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Ruby `dig`: index into nested Arrays/Hashes by each key in turn, short-
/// circuiting to `nil` the moment a step is `nil`.
fn dig(recv: &Value, keys: &[Value]) -> Value {
    let mut cur = recv.clone();
    for k in keys {
        if matches!(cur, Value::Undef) {
            return Value::Undef;
        }
        cur = if with_host(|h| h.as_array(&cur)).is_some() {
            arr_index(
                &with_host(|h| h.as_array(&cur).unwrap()),
                std::slice::from_ref(k),
            )
        } else if let Some(m) = with_host(|h| h.as_hash(&cur)) {
            let key = with_host(|h| h.value_to_key(k));
            m.get(&key).cloned().unwrap_or(Value::Undef)
        } else {
            return Value::Undef;
        };
    }
    cur
}

/// Split a string into lines, keeping each trailing `\n` (Ruby `String#lines`).
fn split_lines(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        cur.push(c);
        if c == '\n' {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parse one Ruby character-selector spec (as used by `count`/`delete`/`squeeze`)
/// into `(negated, set)`. A char `c` matches the spec when `negated != set.contains(c)`.
/// Supports leading `^` negation, `a-z` ranges, and `\` escaping.
fn parse_char_selector(spec: &str) -> (bool, std::collections::HashSet<char>) {
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut negated = false;
    if chars.len() > 1 && chars[0] == '^' {
        negated = true;
        i = 1;
    }
    let mut set = std::collections::HashSet::new();
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            set.insert(chars[i + 1]);
            i += 2;
        } else if i + 2 < chars.len() && chars[i + 1] == '-' {
            let (start, end) = (chars[i], chars[i + 2]);
            if start <= end {
                for ch in start..=end {
                    set.insert(ch);
                }
            }
            i += 3;
        } else {
            set.insert(chars[i]);
            i += 1;
        }
    }
    (negated, set)
}

/// Expand a `tr`/`tr_s` character spec into an ordered list of chars, honouring
/// `a-z` ranges, `\`-escapes, and (when `allow_neg`) a leading `^` negation.
/// Unlike `parse_char_selector` this preserves order for positional mapping.
/// Whether a case method was passed the `:ascii` option (`"x".upcase(:ascii)`).
fn is_ascii_case_opt(args: &[Value]) -> bool {
    args.first()
        .and_then(|v| with_host(|h| h.as_symbol(v)))
        .is_some_and(|s| s == "ascii")
}

fn expand_tr_spec(spec: &str, allow_neg: bool) -> Result<(bool, Vec<char>), String> {
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut negated = false;
    if allow_neg && chars.len() > 1 && chars[0] == '^' {
        negated = true;
        i = 1;
    }
    let mut out: Vec<char> = Vec::new();
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
        } else if i + 2 < chars.len() && chars[i + 1] == '-' {
            let (start, end) = (chars[i], chars[i + 2]);
            if start > end {
                return Err(raise_exc(
                    "ArgumentError",
                    &format!("invalid range \"{start}-{end}\" in string transliteration"),
                ));
            }
            for ch in start..=end {
                out.push(ch);
            }
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Ok((negated, out))
}

/// Translate a single char for `tr`/`tr_s`. `None` means the char is deleted
/// (empty `to` spec). With `neg`, every char NOT in `from` maps to the last
/// char of `to`; otherwise a matched char maps positionally.
fn tr_map(c: char, neg: bool, from: &[char], to: &[char]) -> Option<char> {
    if neg {
        if from.contains(&c) {
            Some(c)
        } else {
            to.last().copied()
        }
    } else {
        match from.iter().position(|&f| f == c) {
            Some(i) => to.get(i).or_else(|| to.last()).copied(),
            None => Some(c),
        }
    }
}

/// Build a predicate matching a char against ALL selector specs (Ruby intersects
/// multiple args). With no args every char matches.
fn char_matcher(args: &[Value]) -> impl Fn(char) -> bool {
    let parsed: Vec<(bool, std::collections::HashSet<char>)> = args
        .iter()
        .map(|v| parse_char_selector(&arg_str(v)))
        .collect();
    move |c| parsed.iter().all(|(neg, set)| *neg != set.contains(&c))
}

fn pad_str(args: &[Value]) -> String {
    args.get(1).map(arg_str).unwrap_or_else(|| " ".to_string())
}
fn pad(s: &str, width: usize, p: String, left: bool) -> String {
    let len = s.chars().count();
    if len >= width || p.is_empty() {
        return s.to_string();
    }
    let need = width - len;
    let fill: String = p.chars().cycle().take(need).collect();
    if left {
        format!("{s}{fill}")
    } else {
        format!("{fill}{s}")
    }
}

/// For `String#slice!`: the removed substring and what remains after removing
/// it. Handles the index/(index,len)/range/string/regex argument forms, mirroring
/// `str_index`. Returns `None` when nothing matches.
fn str_slice_remove(s: &str, args: &[Value]) -> Option<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let cut = |start: usize, end: usize| -> (String, String) {
        let removed: String = chars[start..end].iter().collect();
        let mut rest: String = chars[..start].iter().collect();
        rest.extend(&chars[end..]);
        (removed, rest)
    };
    match args {
        [Value::Int(i)] => {
            let k = norm_idx(*i, n)?;
            (k < n).then(|| cut(k, k + 1))
        }
        [Value::Int(i), Value::Int(len)] => {
            if *len < 0 {
                return None;
            }
            let start = norm_idx(*i, n)?;
            if start > n {
                return None;
            }
            let end = (start + *len as usize).min(n);
            Some(cut(start, end))
        }
        [rng] if with_host(|h| h.as_range(rng)).is_some() => {
            let (lo, hi, excl) = with_host(|h| h.as_range(rng)).unwrap();
            let (start, e) = range_bounds(lo, hi, excl, n)?;
            Some(cut(start, e))
        }
        // A string or regex argument removes its first occurrence.
        [pat] => {
            if let Some(re) = str_regex(pat) {
                let m = re.find(s).ok().flatten()?;
                let start = s[..m.start()].chars().count();
                let end = start + m.as_str().chars().count();
                Some(cut(start, end))
            } else {
                let needle = arg_str(pat);
                let bpos = s.find(&needle)?;
                let start = s[..bpos].chars().count();
                let end = start + needle.chars().count();
                Some(cut(start, end))
            }
        }
        _ => None,
    }
}

fn str_index(s: &str, args: &[Value]) -> Value {
    let chars: Vec<char> = s.chars().collect();
    match args {
        [Value::Int(i)] => {
            let idx = norm_idx(*i, chars.len());
            idx.and_then(|k| chars.get(k))
                .map(|c| new_str(c.to_string()))
                .unwrap_or(Value::Undef)
        }
        [Value::Int(i), Value::Int(len)] => {
            // `str[start, len]` is nil when start is out of range (a negative
            // start that underflows past 0, or start > length) or len < 0.
            // start == length is valid and yields "".
            match norm_idx(*i, chars.len()) {
                Some(start) if start <= chars.len() && *len >= 0 => {
                    let end = (start + *len as usize).min(chars.len());
                    new_str(chars[start..end].iter().collect())
                }
                _ => Value::Undef,
            }
        }
        [rng] => {
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(rng)) {
                match range_bounds(lo, hi, excl, chars.len()) {
                    Some((start, e)) => new_str(chars[start..e].iter().collect()),
                    None => Value::Undef,
                }
            } else if let Some((re, _)) = with_host(|h| h.as_regex(rng)) {
                // `s[/re/]` — the whole match, or nil.
                re.find(s)
                    .ok()
                    .flatten()
                    .map(|m| new_str(m.as_str().to_string()))
                    .unwrap_or(Value::Undef)
            } else if let Some(sub) = with_host(|h| h.as_str(rng)) {
                // `s["sub"]` — the substring itself if present, else nil.
                if s.contains(&sub) {
                    new_str(sub)
                } else {
                    Value::Undef
                }
            } else {
                Value::Undef
            }
        }
        // `s[/re/, n]` — the nth capture group of the match (0 = whole match).
        [re, Value::Int(n)] if with_host(|h| h.as_regex(re)).is_some() => {
            let (re, _) = with_host(|h| h.as_regex(re)).unwrap();
            match re.captures(s).ok().flatten() {
                Some(caps) => caps
                    .get((*n).max(0) as usize)
                    .map(|m| new_str(m.as_str().to_string()))
                    .unwrap_or(Value::Undef),
                None => Value::Undef,
            }
        }
        _ => Value::Undef,
    }
}

/// Ruby `String#succ` / `Symbol#succ`: increment the rightmost alphanumeric,
/// carrying leftward across alphanumerics only; on carry-out of the leftmost
/// alnum, a new leading char of the same class is inserted (`"Zz"` -> `"AAa"`).
/// A string with no alphanumerics increments its last code point with carry.
fn str_succ(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut chars: Vec<char> = s.chars().collect();
    if chars.iter().any(|c| c.is_ascii_alphanumeric()) {
        let mut idx = chars.len() as isize - 1;
        let mut prepend: Option<char> = None;
        loop {
            while idx >= 0 && !chars[idx as usize].is_ascii_alphanumeric() {
                idx -= 1;
            }
            if idx < 0 {
                break;
            }
            let c = chars[idx as usize];
            let (next, carry) = match c {
                '0'..='8' | 'a'..='y' | 'A'..='Y' => ((c as u8 + 1) as char, false),
                '9' => ('0', true),
                'z' => ('a', true),
                'Z' => ('A', true),
                _ => (c, false),
            };
            chars[idx as usize] = next;
            if !carry {
                prepend = None;
                break;
            }
            prepend = Some(match c {
                '0'..='9' => '1',
                'a'..='z' => 'a',
                _ => 'A',
            });
            idx -= 1;
        }
        if let Some(pc) = prepend {
            chars.insert((idx + 1).max(0) as usize, pc);
        }
    } else {
        // No alphanumerics: increment the last code point, carrying leftward.
        let mut idx = chars.len() as isize - 1;
        loop {
            if idx < 0 {
                chars.insert(0, '\u{1}');
                break;
            }
            match char::from_u32(chars[idx as usize] as u32 + 1) {
                Some(nc) => {
                    chars[idx as usize] = nc;
                    break;
                }
                None => {
                    chars[idx as usize] = '\u{0}';
                    idx -= 1;
                }
            }
        }
    }
    chars.into_iter().collect()
}

// ---- Array ----------------------------------------------------------------

/// Remove duplicates by `==`, keeping the first occurrence (for `Array#&`/`|`).
fn dedup_keep(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for x in items {
        if !out.iter().any(|y| with_host(|h| h.eq_values(&x, y))) {
            out.push(x);
        }
    }
    out
}

fn dispatch_array(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    frozen_guard(recv, name, ARRAY_MUTATORS)?;
    let arr = with_host(|h| h.as_array(recv).unwrap_or_default());
    match name {
        // `arr <=> other` — element-wise comparison: the first non-equal pair
        // decides; if one is a prefix of the other, the shorter is less. Returns
        // nil when the operand is not an Array.
        "<=>" if !args.is_empty() => match with_host(|h| h.as_array(&args[0])) {
            Some(other) => {
                use std::cmp::Ordering;
                let n = arr.len().min(other.len());
                for i in 0..n {
                    match cmp_values(&arr[i], &other[i]) {
                        Ordering::Equal => {}
                        Ordering::Less => return Ok(Value::Int(-1)),
                        Ordering::Greater => return Ok(Value::Int(1)),
                    }
                }
                Ok(Value::Int(match arr.len().cmp(&other.len()) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                }))
            }
            None => Ok(Value::Undef),
        },
        // Set-like operators: `&` intersection (deduped), `|` union (deduped),
        // `-` difference. `-` also arrives via the native path (num_op).
        "&" | "intersection" | "|" | "union" | "-" | "difference" if !args.is_empty() => {
            let other = with_host(|h| h.as_array(&args[0]).unwrap_or_default());
            let has = |xs: &[Value], v: &Value| xs.iter().any(|w| with_host(|h| h.eq_values(v, w)));
            let out = match name {
                "&" | "intersection" => {
                    dedup_keep(arr.iter().filter(|v| has(&other, v)).cloned().collect())
                }
                "-" | "difference" => arr.iter().filter(|v| !has(&other, v)).cloned().collect(),
                _ => dedup_keep(arr.iter().chain(other.iter()).cloned().collect()),
            };
            Ok(new_arr(out))
        }
        // `Array#intersect?(other)` — true if any element is shared (Ruby 3.1+).
        "intersect?" if !args.is_empty() => {
            let other = with_host(|h| h.as_array(&args[0]).unwrap_or_default());
            Ok(Value::Bool(
                arr.iter()
                    .any(|v| other.iter().any(|w| with_host(|h| h.eq_values(v, w)))),
            ))
        }
        "length" | "size" | "count"
            if !(name == "count" && (!args.is_empty() || block.is_some())) =>
        {
            Ok(Value::Int(arr.len() as i64))
        }
        "push" | "append" | "<<" => {
            let mut a = arr;
            a.extend(args.iter().cloned());
            with_host(|h| h.set_array(recv, a));
            Ok(recv.clone())
        }
        "pop" => {
            let mut a = arr;
            let v = a.pop().unwrap_or(Value::Undef);
            with_host(|h| h.set_array(recv, a));
            Ok(v)
        }
        "shift" => {
            let mut a = arr;
            let v = if a.is_empty() {
                Value::Undef
            } else {
                a.remove(0)
            };
            with_host(|h| h.set_array(recv, a));
            Ok(v)
        }
        "unshift" | "prepend" => {
            let mut a = arr;
            for (i, x) in args.iter().enumerate() {
                a.insert(i, x.clone());
            }
            with_host(|h| h.set_array(recv, a));
            Ok(recv.clone())
        }
        "first" => match args.first() {
            Some(n) => Ok(new_arr(
                arr.iter().take(as_i(n).max(0) as usize).cloned().collect(),
            )),
            None => Ok(arr.first().cloned().unwrap_or(Value::Undef)),
        },
        "last" => match args.first() {
            Some(n) => {
                let k = as_i(n).max(0) as usize;
                let start = arr.len().saturating_sub(k);
                Ok(new_arr(arr[start..].to_vec()))
            }
            None => Ok(arr.last().cloned().unwrap_or(Value::Undef)),
        },
        "each_cons" => {
            let n = as_i(&args[0]).max(1) as usize;
            let windows: Vec<Value> = arr.windows(n).map(|w| new_arr(w.to_vec())).collect();
            if let Some(bl) = &block {
                for w in &windows {
                    call_proc(bl, std::slice::from_ref(w))?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // No enumerator type yet: return the windows array (usable with
                // `.to_a`/`.map`/`.each`, unlike MRI's lazy Enumerator).
                Ok(new_arr(windows))
            }
        }
        "dig" => Ok(dig(recv, args)),
        "empty?" => Ok(Value::Bool(arr.is_empty())),
        "reverse" => Ok(new_arr(arr.into_iter().rev().collect())),
        "reverse!" => {
            // In-place reverse; returns the receiver.
            let rev: Vec<Value> = arr.into_iter().rev().collect();
            with_host(|h| h.set_array(recv, rev));
            Ok(recv.clone())
        }
        "to_a" | "to_ary" | "dup" | "clone" | "deconstruct" => Ok(new_arr(arr)),
        "include?" => Ok(Value::Bool(
            arr.iter().any(|x| with_host(|h| h.eq_values(x, &args[0]))),
        )),
        "index" | "find_index" => {
            let pos = if let Some(bl) = &block {
                let mut found = None;
                for (i, x) in arr.iter().enumerate() {
                    let r = call_proc(bl, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        found = Some(i);
                        break;
                    }
                }
                found
            } else {
                arr.iter()
                    .position(|x| with_host(|h| h.eq_values(x, &args[0])))
            };
            Ok(pos.map(|p| Value::Int(p as i64)).unwrap_or(Value::Undef))
        }
        "rindex" => {
            let pos = if let Some(bl) = &block {
                let mut found = None;
                for (i, x) in arr.iter().enumerate().rev() {
                    let r = call_proc(bl, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        found = Some(i);
                        break;
                    }
                }
                found
            } else {
                arr.iter()
                    .rposition(|x| with_host(|h| h.eq_values(x, &args[0])))
            };
            Ok(pos.map(|p| Value::Int(p as i64)).unwrap_or(Value::Undef))
        }
        "bsearch" => {
            // Binary search over a partitioned array. A block returning true/false
            // selects find-minimum mode (smallest element satisfying the block);
            // a block returning an Integer selects find-any mode (0 == match,
            // negative => search left, positive => search right). Returns nil when
            // nothing is found. Assumes the receiver is already sorted/partitioned.
            let bl = match &block {
                Some(b) => b,
                // No Enumerator type yet: mirror MRI's find-minimum shape by
                // returning the receiver so a subsequent chain still sees the data.
                None => return Ok(recv.clone()),
            };
            let mut lo = 0usize;
            let mut hi = arr.len();
            let mut satisfied: Option<usize> = None;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let r = call_proc(bl, std::slice::from_ref(&arr[mid]))?;
                match r {
                    Value::Int(n) => {
                        if n == 0 {
                            return Ok(arr[mid].clone());
                        } else if n < 0 {
                            hi = mid;
                        } else {
                            lo = mid + 1;
                        }
                    }
                    Value::Float(f) => {
                        if f == 0.0 {
                            return Ok(arr[mid].clone());
                        } else if f < 0.0 {
                            hi = mid;
                        } else {
                            lo = mid + 1;
                        }
                    }
                    other => {
                        if with_host(|h| h.truthy(&other)) {
                            satisfied = Some(mid);
                            hi = mid;
                        } else {
                            lo = mid + 1;
                        }
                    }
                }
            }
            Ok(satisfied.map(|i| arr[i].clone()).unwrap_or(Value::Undef))
        }
        "values_at" => {
            let mut out = Vec::new();
            for a in args {
                if let Some((lo, hi, excl)) = with_host(|h| h.as_range(a)) {
                    let s = norm_idx(lo, arr.len()).unwrap_or(arr.len());
                    let mut e = norm_idx(hi, arr.len()).unwrap_or(arr.len());
                    if !excl {
                        e += 1;
                    }
                    let e = e.min(arr.len());
                    for k in s..e.max(s) {
                        out.push(arr.get(k).cloned().unwrap_or(Value::Undef));
                    }
                } else {
                    let v = norm_idx(as_i(a), arr.len())
                        .and_then(|k| arr.get(k))
                        .cloned()
                        .unwrap_or(Value::Undef);
                    out.push(v);
                }
            }
            Ok(new_arr(out))
        }
        "each_index" => {
            if let Some(b) = &block {
                for i in 0..arr.len() {
                    call_proc(b, &[Value::Int(i as i64)])?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // No Enumerator type yet: return the indices array so a subsequent
                // `.to_a`/`.map`/`.each` chain still works.
                Ok(new_arr((0..arr.len() as i64).map(Value::Int).collect()))
            }
        }
        "rotate!" => {
            let n = args.first().map(as_i).unwrap_or(1);
            let len = arr.len() as i64;
            if len == 0 {
                return Ok(recv.clone());
            }
            let k = ((n % len) + len) % len;
            let mut out = arr[k as usize..].to_vec();
            out.extend_from_slice(&arr[..k as usize]);
            with_host(|h| h.set_array(recv, out));
            Ok(recv.clone())
        }
        "flatten!" => {
            let depth = match args.first() {
                Some(Value::Undef) | None => -1,
                Some(n) => as_i(n),
            };
            // Only flatten (and report a change) if a nested array is present.
            let has_nested = arr.iter().any(|x| with_host(|h| h.as_array(x)).is_some());
            if !has_nested {
                return Ok(Value::Undef);
            }
            let mut out = Vec::new();
            flatten_depth_into(&arr, depth, &mut out);
            with_host(|h| h.set_array(recv, out));
            Ok(recv.clone())
        }
        "compact!" => {
            let had_nil = arr.iter().any(|x| matches!(x, Value::Undef));
            if !had_nil {
                return Ok(Value::Undef);
            }
            let out: Vec<Value> = arr
                .into_iter()
                .filter(|x| !matches!(x, Value::Undef))
                .collect();
            with_host(|h| h.set_array(recv, out));
            Ok(recv.clone())
        }
        "join" => {
            let sep = args.first().map(arg_str).unwrap_or_default();
            let parts: Vec<String> = arr.iter().map(|x| with_host(|h| h.to_s(x))).collect();
            Ok(new_str(parts.join(&sep)))
        }
        // `Array#pack(fmt)` — serialize elements to a byte string per the template
        // directives (C/c a/A N/n V/v H/h). Bytes are stored as Latin-1 codepoints
        // (byte b => char U+00xx), so the result round-trips through `String#unpack`
        // (see `pack_bytes`/`unpack_bytes`).
        "pack" => {
            let fmt = arg_str(&args[0]);
            let bytes = pack_bytes(&arr, &fmt)?;
            Ok(new_str(bytes_to_binstr(&bytes)))
        }
        "sort" | "sort!" => {
            let a = match &block {
                Some(bl) => sort_with_block(arr, bl)?,
                None => {
                    let mut a = arr;
                    a.sort_by(cmp_values);
                    a
                }
            };
            // `sort!` sorts in place and returns the receiver.
            if name == "sort!" {
                with_host(|h| h.set_array(recv, a));
                return Ok(recv.clone());
            }
            Ok(new_arr(a))
        }
        "minmax" => {
            if arr.is_empty() {
                return Ok(new_arr(vec![Value::Undef, Value::Undef]));
            }
            let (mut lo, mut hi) = (arr[0].clone(), arr[0].clone());
            let cmp = |x: &Value, y: &Value| -> Result<i64, String> {
                Ok(match &block {
                    Some(bl) => as_i(&call_proc(bl, &[x.clone(), y.clone()])?),
                    None => match cmp_values(x, y) {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => 1,
                    },
                })
            };
            for x in &arr[1..] {
                if cmp(x, &lo)? < 0 {
                    lo = x.clone();
                }
                if cmp(x, &hi)? > 0 {
                    hi = x.clone();
                }
            }
            Ok(new_arr(vec![lo, hi]))
        }
        "min" | "max" => {
            if arr.is_empty() {
                return Ok(if args.is_empty() {
                    Value::Undef
                } else {
                    new_arr(vec![])
                });
            }
            let want_max = name == "max";
            // `min(n)` / `max(n)` returns the n extremes, sorted.
            if let Some(n) = args.first().filter(|_| block.is_none()) {
                let mut sorted = arr.clone();
                sorted.sort_by(cmp_values);
                let k = (as_i(n).max(0) as usize).min(sorted.len());
                let picked: Vec<Value> = if want_max {
                    sorted.iter().rev().take(k).cloned().collect()
                } else {
                    sorted.iter().take(k).cloned().collect()
                };
                return Ok(new_arr(picked));
            }
            let mut best = arr[0].clone();
            for x in &arr[1..] {
                let c = match &block {
                    Some(bl) => as_i(&call_proc(bl, &[x.clone(), best.clone()])?),
                    None => match cmp_values(x, &best) {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => 1,
                    },
                };
                if (want_max && c > 0) || (!want_max && c < 0) {
                    best = x.clone();
                }
            }
            Ok(best)
        }
        "sum" => {
            let mut acc = args.first().cloned().unwrap_or(Value::Int(0));
            for x in &arr {
                let v = match &block {
                    Some(bl) => call_proc(bl, std::slice::from_ref(x))?,
                    None => x.clone(),
                };
                acc = add_values(&acc, &v);
            }
            Ok(acc)
        }
        "uniq" => {
            // With a block, uniqueness is by the block's return value (the key);
            // without one, by the element itself. The first element for each key
            // is kept, in original order.
            let mut out: Vec<Value> = Vec::new();
            let mut keys: Vec<Value> = Vec::new();
            for x in arr {
                let key = match &block {
                    Some(b) => call_proc(b, std::slice::from_ref(&x))?,
                    None => x.clone(),
                };
                if !keys.iter().any(|k| with_host(|h| h.eq_values(&key, k))) {
                    keys.push(key);
                    out.push(x);
                }
            }
            Ok(new_arr(out))
        }
        "uniq!" => {
            // In-place `uniq`. Returns the receiver if any duplicate was removed,
            // else nil (MRI semantics), mirroring `compact!`/`flatten!`.
            let mut out: Vec<Value> = Vec::new();
            let mut keys: Vec<Value> = Vec::new();
            for x in &arr {
                let key = match &block {
                    Some(b) => call_proc(b, std::slice::from_ref(x))?,
                    None => x.clone(),
                };
                if !keys.iter().any(|k| with_host(|h| h.eq_values(&key, k))) {
                    keys.push(key);
                    out.push(x.clone());
                }
            }
            if out.len() == arr.len() {
                return Ok(Value::Undef);
            }
            with_host(|h| h.set_array(recv, out));
            Ok(recv.clone())
        }
        "compact" => Ok(new_arr(
            arr.into_iter()
                .filter(|x| !matches!(x, Value::Undef))
                .collect(),
        )),
        "flatten" => {
            // `flatten` recurses fully; `flatten(n)` flattens at most n levels.
            // A negative/nil depth means unbounded, matching MRI.
            let depth = match args.first() {
                Some(Value::Undef) | None => -1,
                Some(n) => as_i(n),
            };
            let mut out = Vec::new();
            flatten_depth_into(&arr, depth, &mut out);
            Ok(new_arr(out))
        }
        "concat" => {
            let mut a = arr;
            for x in args {
                if let Some(xs) = with_host(|h| h.as_array(x)) {
                    a.extend(xs);
                }
            }
            with_host(|h| h.set_array(recv, a));
            Ok(recv.clone())
        }
        // `replace(other)` — swap this array's contents for `other`'s (in place).
        "replace" => {
            let other = with_host(|h| h.as_array(&args[0])).unwrap_or_default();
            with_host(|h| h.set_array(recv, other));
            Ok(recv.clone())
        }
        // `clear` — empty the array in place, returning self.
        "clear" => {
            with_host(|h| h.set_array(recv, Vec::new()));
            Ok(recv.clone())
        }
        // `to_set` — a Set of the elements (from `require "set"`).
        "to_set" => Ok(with_host(|h| h.new_set(arr))),
        // `each_entry` yields each element as an entry; for a plain Array that is
        // just each element (MRI: `[1,2,3].each_entry{…}`→`[1,2,3]`). Shares the
        // `each` body; blockless returns an Enumerator carrying the method name.
        "each" | "each_entry" => {
            if let Some(b) = &block {
                for x in &arr {
                    call_proc(b, std::slice::from_ref(x))?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // Block-less: an Enumerator yielding each element, so external
                // iteration (`next`/`peek`) and chained Enumerable calls work.
                Ok(with_host(|h| h.new_enumerator(arr, name)))
            }
        }
        // `chain(*enums)` returns an Enumerator over the receiver followed by each
        // argument's elements (MRI: `[1,2,3].chain([4,5]).to_a`→`[1,2,3,4,5]`).
        "chain" => {
            let mut chained = arr.clone();
            for a in args {
                match with_host(|h| h.as_array(a)) {
                    Some(xs) => chained.extend(xs),
                    None => chained.push(a.clone()),
                }
            }
            Ok(with_host(|h| h.new_enumerator(chained, "each")))
        }
        "reverse_each" => {
            if let Some(b) = &block {
                for x in arr.iter().rev() {
                    call_proc(b, std::slice::from_ref(x))?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // Block-less: an Enumerator yielding the elements in reverse.
                let rev: Vec<Value> = arr.iter().rev().cloned().collect();
                Ok(with_host(|h| h.new_enumerator(rev, "reverse_each")))
            }
        }
        "each_with_index" => {
            if let Some(b) = &block {
                for (i, x) in arr.iter().enumerate() {
                    call_proc(b, &[x.clone(), Value::Int(i as i64)])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // Block-less: an Enumerator of `[elem, index]` pairs, so both
                // `each_with_index.next` and `each_with_index.map { … }` work.
                let pairs = arr
                    .iter()
                    .enumerate()
                    .map(|(i, x)| new_arr(vec![x.clone(), Value::Int(i as i64)]))
                    .collect();
                Ok(with_host(|h| h.new_enumerator(pairs, "each_with_index")))
            }
        }
        "map" | "collect" | "flat_map" | "collect_concat" => {
            if block.is_none() {
                // Block-less `map`/`collect` yields the original elements as an
                // Enumerator (re-attaching a block later maps them).
                return Ok(with_host(|h| h.new_enumerator(arr, name)));
            }
            let mut out = Vec::with_capacity(arr.len());
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if has_pending_signal() {
                        // `break value` short-circuits and becomes the result.
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                    if name == "flat_map" || name == "collect_concat" {
                        // `collect_concat` is the documented alias of `flat_map`:
                        // one level of array results is spliced into the output.
                        if let Some(xs) = with_host(|h| h.as_array(&r)) {
                            out.extend(xs);
                            continue;
                        }
                    }
                    out.push(r);
                }
            }
            Ok(new_arr(out))
        }
        // `grep(pat)` keeps elements matching `pat === e` (`grep_v` the rest); a
        // block maps each kept element. pat is a Class/Regexp/Range/value.
        "grep" | "grep_v" if !args.is_empty() => {
            let invert = name == "grep_v";
            let mut out = Vec::new();
            for x in &arr {
                let m = dispatch(&args[0], "===", std::slice::from_ref(x), None)
                    .map(|v| with_host(|h| h.truthy(&v)))
                    .unwrap_or(false);
                if m != invert {
                    match &block {
                        Some(b) => out.push(call_proc(b, std::slice::from_ref(x))?),
                        None => out.push(x.clone()),
                    }
                }
            }
            Ok(new_arr(out))
        }
        "select" | "filter" | "find_all" | "reject" => {
            if block.is_none() {
                // Block-less: an Enumerator over the elements, tagged with the
                // filtering method so `select.with_index { … }` filters.
                return Ok(with_host(|h| h.new_enumerator(arr, name)));
            }
            let keep_when = name != "reject";
            let mut out = Vec::new();
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                    let t = with_host(|h| h.truthy(&r));
                    if t == keep_when {
                        out.push(x.clone());
                    }
                }
            }
            Ok(new_arr(out))
        }
        // `filter_map` maps each element and keeps only the truthy results.
        "filter_map" => {
            let mut out = Vec::new();
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                    if with_host(|h| h.truthy(&r)) {
                        out.push(r);
                    }
                }
            }
            Ok(new_arr(out))
        }
        // `transpose` turns an array of equal-length rows into columns.
        "transpose" => {
            let rows: Vec<Vec<Value>> = arr
                .iter()
                .map(|r| with_host(|h| h.as_array(r).unwrap_or_default()))
                .collect();
            let width = rows.first().map(|r| r.len()).unwrap_or(0);
            if let Some(bad) = rows.iter().position(|r| r.len() != width) {
                return Err(raise_exc(
                    "IndexError",
                    &format!(
                        "element size differs ({} should be {width})",
                        rows[bad].len()
                    ),
                ));
            }
            let cols: Vec<Value> = (0..width)
                .map(|c| new_arr(rows.iter().map(|r| r[c].clone()).collect()))
                .collect();
            Ok(new_arr(cols))
        }
        "find" | "detect" => {
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        return Ok(x.clone());
                    }
                }
            }
            Ok(Value::Undef)
        }
        "any?" => {
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            } else {
                Ok(Value::Bool(arr.iter().any(|x| with_host(|h| h.truthy(x)))))
            }
        }
        "all?" => {
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if !with_host(|h| h.truthy(&r)) {
                        return Ok(Value::Bool(false));
                    }
                }
            }
            Ok(Value::Bool(true))
        }
        "none?" => {
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        return Ok(Value::Bool(false));
                    }
                }
            }
            Ok(Value::Bool(true))
        }
        "count" => {
            if let Some(b) = &block {
                let mut n = 0i64;
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        n += 1;
                    }
                }
                Ok(Value::Int(n))
            } else if let Some(target) = args.first() {
                // `count(obj)` counts elements equal to `obj`.
                let n = arr
                    .iter()
                    .filter(|x| with_host(|h| h.eq_values(x, target)))
                    .count();
                Ok(Value::Int(n as i64))
            } else {
                Ok(Value::Int(arr.len() as i64))
            }
        }
        "reduce" | "inject" => {
            // Symbol form: `inject(:+)` / `reduce(init, :+)` — send the operator
            // (or method) symbol between the accumulator and each element.
            if block.is_none() {
                if let Some(op) = args.last().and_then(|a| with_host(|h| h.as_symbol(a))) {
                    let mut items = arr.iter();
                    let mut acc = if args.len() >= 2 {
                        args[0].clone()
                    } else {
                        match items.next() {
                            Some(v) => v.clone(),
                            None => return Ok(Value::Undef),
                        }
                    };
                    for x in items {
                        acc = reduce_sym(&acc, &op, x)?;
                    }
                    return Ok(acc);
                }
            }
            let mut acc = args.first().cloned();
            let mut items = arr.iter();
            if acc.is_none() {
                acc = items.next().cloned();
            }
            let mut acc = acc.unwrap_or(Value::Undef);
            if let Some(b) = &block {
                for x in items {
                    acc = call_proc(b, &[acc.clone(), x.clone()])?;
                    if has_pending_signal() {
                        // `break value` short-circuits and becomes the result.
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
            }
            Ok(acc)
        }
        "min_by" | "max_by" | "sort_by" | "minmax_by" => sort_by_family(recv, name, &arr, &block),
        "[]" => Ok(arr_index(&arr, args)),
        "fetch" => {
            let len = arr.len();
            let raw = as_i(&args[0]);
            match norm_idx(raw, len).filter(|&i| i < len) {
                Some(i) => Ok(arr[i].clone()),
                // Out of bounds: an explicit default, else a block, else IndexError.
                None if args.len() > 1 => Ok(args[1].clone()),
                None => match &block {
                    Some(b) => call_proc(b, &[Value::Int(raw)]),
                    None => Err(raise_exc(
                        "IndexError",
                        &format!("index {raw} outside of array bounds: -{len}...{len}"),
                    )),
                },
            }
        }
        "[]=" => {
            let mut a = arr;
            let idx = norm_idx(as_i(&args[0]), a.len()).unwrap_or(a.len());
            while a.len() <= idx {
                a.push(Value::Undef);
            }
            a[idx] = args[args.len() - 1].clone();
            with_host(|h| h.set_array(recv, a));
            Ok(args[args.len() - 1].clone())
        }
        "take" => Ok(new_arr(
            arr.into_iter()
                .take(as_i(&args[0]).max(0) as usize)
                .collect(),
        )),
        "drop" => Ok(new_arr(
            arr.into_iter()
                .skip(as_i(&args[0]).max(0) as usize)
                .collect(),
        )),
        "partition" => {
            let (mut yes, mut no) = (Vec::new(), Vec::new());
            if let Some(bl) = &block {
                for x in &arr {
                    let r = call_proc(bl, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        yes.push(x.clone());
                    } else {
                        no.push(x.clone());
                    }
                }
            }
            let y = new_arr(yes);
            let n = new_arr(no);
            Ok(new_arr(vec![y, n]))
        }
        "group_by" => {
            let mut groups: IndexMap<RKey, Vec<Value>> = IndexMap::new();
            if let Some(bl) = &block {
                for x in &arr {
                    let k = call_proc(bl, std::slice::from_ref(x))?;
                    let key = with_host(|h| h.value_to_key(&k));
                    groups.entry(key).or_default().push(x.clone());
                }
            }
            Ok(with_host(|h| {
                let m: IndexMap<RKey, Value> = groups
                    .into_iter()
                    .map(|(k, v)| (k, h.new_array(v)))
                    .collect();
                h.new_hash(m)
            }))
        }
        "tally" => {
            let mut counts: IndexMap<RKey, i64> = IndexMap::new();
            for x in &arr {
                let key = with_host(|h| h.value_to_key(x));
                *counts.entry(key).or_insert(0) += 1;
            }
            Ok(with_host(|h| {
                let m: IndexMap<RKey, Value> = counts
                    .into_iter()
                    .map(|(k, n)| (k, Value::Int(n)))
                    .collect();
                h.new_hash(m)
            }))
        }
        "each_with_object" => {
            let memo = args[0].clone();
            if let Some(bl) = &block {
                for x in &arr {
                    call_proc(bl, &[x.clone(), memo.clone()])?;
                }
            }
            Ok(memo)
        }
        "zip" => {
            let others: Vec<Vec<Value>> = args
                .iter()
                .map(|a| with_host(|h| h.as_array(a).unwrap_or_default()))
                .collect();
            let rows: Vec<Value> = arr
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let mut row = vec![x.clone()];
                    for o in &others {
                        row.push(o.get(i).cloned().unwrap_or(Value::Undef));
                    }
                    new_arr(row)
                })
                .collect();
            // With a block, MRI yields each row and returns nil.
            if let Some(bl) = &block {
                for row in &rows {
                    call_proc(bl, std::slice::from_ref(row))?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                return Ok(Value::Undef);
            }
            Ok(new_arr(rows))
        }
        "product" => {
            // Cartesian product of self with each argument array.
            let lists: Vec<Vec<Value>> = std::iter::once(arr.clone())
                .chain(
                    args.iter()
                        .map(|a| with_host(|h| h.as_array(a).unwrap_or_default())),
                )
                .collect();
            let mut rows: Vec<Vec<Value>> = vec![Vec::new()];
            for list in &lists {
                let mut next = Vec::with_capacity(rows.len() * list.len());
                for row in &rows {
                    for item in list {
                        let mut r = row.clone();
                        r.push(item.clone());
                        next.push(r);
                    }
                }
                rows = next;
            }
            Ok(new_arr(rows.into_iter().map(new_arr).collect()))
        }
        "combination" => {
            let n = as_i(&args[0]);
            let combos = combinations(&arr, n);
            let out: Vec<Value> = combos.into_iter().map(new_arr).collect();
            if let Some(bl) = &block {
                for c in &out {
                    call_proc(bl, std::slice::from_ref(c))?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // No Enumerator type: return the combinations array directly so
                // `.to_a`/`.map`/`.each` all work.
                Ok(new_arr(out))
            }
        }
        "permutation" => {
            let n = args.first().map(as_i).unwrap_or(arr.len() as i64);
            let perms = permutations(&arr, n);
            let out: Vec<Value> = perms.into_iter().map(new_arr).collect();
            if let Some(bl) = &block {
                for p in &out {
                    call_proc(bl, std::slice::from_ref(p))?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                Ok(new_arr(out))
            }
        }
        "assoc" => {
            for x in &arr {
                if let Some(sub) = with_host(|h| h.as_array(x)) {
                    if let Some(first) = sub.first() {
                        if with_host(|h| h.eq_values(first, &args[0])) {
                            return Ok(x.clone());
                        }
                    }
                }
            }
            Ok(Value::Undef)
        }
        "rassoc" => {
            for x in &arr {
                if let Some(sub) = with_host(|h| h.as_array(x)) {
                    if let Some(second) = sub.get(1) {
                        if with_host(|h| h.eq_values(second, &args[0])) {
                            return Ok(x.clone());
                        }
                    }
                }
            }
            Ok(Value::Undef)
        }
        "fill" => {
            let mut a = arr;
            let len = a.len() as i64;
            if let Some(bl) = &block {
                // fill { |index| ... } / fill(start) { } / fill(start, length) { }
                let start = args.first().map(as_i).unwrap_or(0);
                let start = if start < 0 {
                    (start + len).max(0)
                } else {
                    start
                };
                let end = match args.get(1) {
                    Some(l) => start + as_i(l),
                    None => len,
                };
                let mut i = start;
                while i < end {
                    let idx = i as usize;
                    if idx >= a.len() {
                        a.push(Value::Undef);
                    }
                    a[idx] = call_proc(bl, &[Value::Int(i)])?;
                    i += 1;
                }
            } else {
                // fill(value) / fill(value, start) / fill(value, start, length)
                let val = args[0].clone();
                let start = args.get(1).map(as_i).unwrap_or(0);
                let start = if start < 0 {
                    (start + len).max(0)
                } else {
                    start
                };
                let end = match args.get(2) {
                    Some(l) => start + as_i(l),
                    None => len.max(start),
                };
                let mut i = start;
                while i < end {
                    let idx = i as usize;
                    if idx >= a.len() {
                        a.push(Value::Undef);
                    }
                    a[idx] = val.clone();
                    i += 1;
                }
            }
            with_host(|h| h.set_array(recv, a));
            Ok(recv.clone())
        }
        "insert" => {
            let mut a = arr;
            let idx = as_i(&args[0]);
            let vals = &args[1..];
            let pos = if idx < 0 {
                // Negative index inserts AFTER the referenced element.
                (a.len() as i64 + idx + 1).max(0) as usize
            } else {
                idx as usize
            };
            if pos > a.len() {
                // Pad with nil up to the insertion point.
                a.resize(pos, Value::Undef);
            }
            for (k, v) in vals.iter().enumerate() {
                a.insert(pos + k, v.clone());
            }
            with_host(|h| h.set_array(recv, a));
            Ok(recv.clone())
        }
        // `Array#delete(obj)` — remove every element equal to `obj`; return the
        // object if any were removed, else the block's value (if given) or nil.
        "delete" => {
            let mut a = arr;
            let before = a.len();
            a.retain(|x| !with_host(|h| h.eq_values(x, &args[0])));
            let removed = a.len() < before;
            with_host(|h| h.set_array(recv, a));
            if removed {
                Ok(args[0].clone())
            } else if let Some(bl) = &block {
                call_proc(bl, std::slice::from_ref(&args[0]))
            } else {
                Ok(Value::Undef)
            }
        }
        "delete_at" => {
            let mut a = arr;
            let len = a.len() as i64;
            let mut idx = as_i(&args[0]);
            if idx < 0 {
                idx += len;
            }
            let v = if idx < 0 || idx >= len {
                Value::Undef
            } else {
                a.remove(idx as usize)
            };
            with_host(|h| h.set_array(recv, a));
            Ok(v)
        }
        "delete_if" | "reject!" => {
            let mut a = arr;
            if let Some(bl) = &block {
                let mut kept = Vec::with_capacity(a.len());
                for x in a.drain(..) {
                    let r = call_proc(bl, std::slice::from_ref(&x))?;
                    if !with_host(|h| h.truthy(&r)) {
                        kept.push(x);
                    }
                }
                a = kept;
            }
            with_host(|h| h.set_array(recv, a));
            Ok(recv.clone())
        }
        "take_while" => {
            let mut out = Vec::new();
            if let Some(bl) = &block {
                for x in &arr {
                    let r = call_proc(bl, std::slice::from_ref(x))?;
                    if with_host(|h| h.truthy(&r)) {
                        out.push(x.clone());
                    } else {
                        break;
                    }
                }
            }
            Ok(new_arr(out))
        }
        "drop_while" => {
            let mut out = Vec::new();
            let mut dropping = true;
            if let Some(bl) = &block {
                for x in &arr {
                    if dropping {
                        let r = call_proc(bl, std::slice::from_ref(x))?;
                        if with_host(|h| h.truthy(&r)) {
                            continue;
                        }
                        dropping = false;
                    }
                    out.push(x.clone());
                }
            }
            Ok(new_arr(out))
        }
        "rotate" => {
            let n = args.first().map(as_i).unwrap_or(1);
            let len = arr.len() as i64;
            if len == 0 {
                return Ok(new_arr(arr));
            }
            let k = ((n % len) + len) % len;
            let mut out = arr[k as usize..].to_vec();
            out.extend_from_slice(&arr[..k as usize]);
            Ok(new_arr(out))
        }
        "each_slice" => {
            let n = as_i(&args[0]).max(1) as usize;
            let slices: Vec<Value> = arr.chunks(n).map(|c| new_arr(c.to_vec())).collect();
            if let Some(bl) = &block {
                for s in &slices {
                    call_proc(bl, std::slice::from_ref(s))?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                // MRI returns the receiver when a block is given.
                Ok(recv.clone())
            } else {
                // No lazy Enumerator yet: return the slices array (usable with
                // `.to_a`/`.map`/`.each`).
                Ok(new_arr(slices))
            }
        }
        "chunk_while" => {
            // Split into runs; a new run starts whenever the block returns
            // falsey for an adjacent pair (elem[i-1], elem[i]).
            let mut chunks: Vec<Value> = Vec::new();
            if let Some(bl) = &block {
                let mut cur: Vec<Value> = Vec::new();
                for x in &arr {
                    if let Some(prev) = cur.last() {
                        let r = call_proc(bl, &[prev.clone(), x.clone()])?;
                        if !with_host(|h| h.truthy(&r)) {
                            chunks.push(new_arr(std::mem::take(&mut cur)));
                        }
                    }
                    cur.push(x.clone());
                }
                if !cur.is_empty() {
                    chunks.push(new_arr(cur));
                }
            }
            // No lazy Enumerator yet: return the chunk array (usable with `.to_a`).
            Ok(new_arr(chunks))
        }
        "slice_when" => {
            // Split into runs; a new run starts whenever the block returns
            // truthy for an adjacent pair (elem[i-1], elem[i]). Inverse of
            // chunk_while.
            let mut chunks: Vec<Value> = Vec::new();
            if let Some(bl) = &block {
                let mut cur: Vec<Value> = Vec::new();
                for x in &arr {
                    if let Some(prev) = cur.last() {
                        let r = call_proc(bl, &[prev.clone(), x.clone()])?;
                        if with_host(|h| h.truthy(&r)) {
                            chunks.push(new_arr(std::mem::take(&mut cur)));
                        }
                    }
                    cur.push(x.clone());
                }
                if !cur.is_empty() {
                    chunks.push(new_arr(cur));
                }
            }
            // No lazy Enumerator yet: return the chunk array (usable with `.to_a`).
            Ok(new_arr(chunks))
        }
        "to_h" => {
            let mut m = IndexMap::new();
            for x in &arr {
                // With a block each element is mapped to the `[key, value]` pair.
                let elem = match &block {
                    Some(b) => call_proc(b, std::slice::from_ref(x))?,
                    None => x.clone(),
                };
                if let Some(pair) = with_host(|h| h.as_array(&elem)) {
                    if pair.len() == 2 {
                        let k = with_host(|h| h.value_to_key(&pair[0]));
                        m.insert(k, pair[1].clone());
                    }
                }
            }
            Ok(with_host(|h| h.new_hash(m)))
        }
        "cycle" => {
            let Some(bl) = &block else {
                // Block-less `cycle(n)` is a finite Enumerator over the elements
                // repeated `n` times (`[1,2].cycle(3).to_a == [1,2,1,2,1,2]`).
                // Block-less endless `cycle` (no count) is an infinite Enumerator
                // backed by a native cycling generator, so `first(n)`/`take(n)`
                // draw as many repeats as needed.
                return match args.first() {
                    Some(n) => {
                        let times = as_i(n).max(0) as usize;
                        let mut out: Vec<Value> = Vec::with_capacity(arr.len() * times);
                        for _ in 0..times {
                            out.extend(arr.iter().cloned());
                        }
                        Ok(with_host(|h| h.new_enumerator(out, "each")))
                    }
                    None => Ok(with_host(|h| h.new_cycle_enumerator(arr.clone()))),
                };
            };
            match args.first() {
                Some(n) => {
                    let times = as_i(n).max(0);
                    for _ in 0..times {
                        for x in &arr {
                            call_proc(bl, std::slice::from_ref(x))?;
                            if has_pending_signal() {
                                if let Some(bv) = take_break() {
                                    return Ok(bv);
                                }
                                return Ok(Value::Undef);
                            }
                        }
                    }
                    Ok(Value::Undef)
                }
                None => {
                    // Endless cycle: only `break`/`return` inside the block exits.
                    if arr.is_empty() {
                        return Ok(Value::Undef);
                    }
                    loop {
                        for x in &arr {
                            call_proc(bl, std::slice::from_ref(x))?;
                            if has_pending_signal() {
                                if let Some(bv) = take_break() {
                                    return Ok(bv);
                                }
                                return Ok(Value::Undef);
                            }
                        }
                    }
                }
            }
        }
        "chunk" => {
            // MRI returns an Enumerator; with no Enumerator type we return the
            // `[key, [elems…]]` pairs array (so `.to_a`/`.each`/`.map` still work).
            let mut out: Vec<Value> = Vec::new();
            if let Some(bl) = &block {
                let mut cur_key: Option<Value> = None;
                let mut group: Vec<Value> = Vec::new();
                for x in &arr {
                    let k = call_proc(bl, std::slice::from_ref(x))?;
                    match &cur_key {
                        Some(pk) if with_host(|h| h.eq_values(pk, &k)) => group.push(x.clone()),
                        _ => {
                            if let Some(pk) = cur_key.take() {
                                out.push(new_arr(vec![pk, new_arr(std::mem::take(&mut group))]));
                            }
                            cur_key = Some(k);
                            group.push(x.clone());
                        }
                    }
                }
                if let Some(pk) = cur_key.take() {
                    out.push(new_arr(vec![pk, new_arr(group)]));
                }
            }
            Ok(new_arr(out))
        }
        _ => Err(no_method_error(recv, name)),
    }
}

fn sort_by_family(
    recv: &Value,
    name: &str,
    arr: &[Value],
    block: &Option<Value>,
) -> Result<Value, String> {
    let Some(b) = block else {
        return Ok(recv.clone());
    };
    let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(arr.len());
    for x in arr {
        let k = call_proc(b, std::slice::from_ref(x))?;
        keyed.push((k, x.clone()));
    }
    match name {
        "min_by" => Ok(keyed
            .into_iter()
            .min_by(|a, c| cmp_values(&a.0, &c.0))
            .map(|p| p.1)
            .unwrap_or(Value::Undef)),
        // Ruby's max_by returns the FIRST element on a key tie; Rust's `max_by`
        // returns the last. Iterating reversed makes the last-of-ties the first
        // in original order. (min_by already returns first-of-ties, like Ruby.)
        "max_by" => Ok(keyed
            .into_iter()
            .rev()
            .max_by(|a, c| cmp_values(&a.0, &c.0))
            .map(|p| p.1)
            .unwrap_or(Value::Undef)),
        "minmax_by" => {
            let lo = keyed
                .iter()
                .min_by(|a, c| cmp_values(&a.0, &c.0))
                .map(|p| p.1.clone())
                .unwrap_or(Value::Undef);
            let hi = keyed
                .iter()
                .rev()
                .max_by(|a, c| cmp_values(&a.0, &c.0))
                .map(|p| p.1.clone())
                .unwrap_or(Value::Undef);
            Ok(new_arr(vec![lo, hi]))
        }
        _ => {
            keyed.sort_by(|a, c| cmp_values(&a.0, &c.0));
            Ok(new_arr(keyed.into_iter().map(|p| p.1).collect()))
        }
    }
}

/// Flatten at most `depth` levels; a negative `depth` means unbounded.
fn flatten_depth_into(arr: &[Value], depth: i64, out: &mut Vec<Value>) {
    for x in arr {
        match with_host(|h| h.as_array(x)) {
            Some(inner) if depth != 0 => flatten_depth_into(&inner, depth - 1, out),
            _ => out.push(x.clone()),
        }
    }
}

fn arr_index(arr: &[Value], args: &[Value]) -> Value {
    match args {
        [Value::Int(i)] => norm_idx(*i, arr.len())
            .and_then(|k| arr.get(k))
            .cloned()
            .unwrap_or(Value::Undef),
        [Value::Int(i), Value::Int(len)] => {
            let start = norm_idx(*i, arr.len()).unwrap_or(arr.len());
            let end = (start + (*len).max(0) as usize).min(arr.len());
            if start > arr.len() {
                Value::Undef
            } else {
                new_arr(arr[start..end].to_vec())
            }
        }
        [rng] => {
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(rng)) {
                match range_bounds(lo, hi, excl, arr.len()) {
                    Some((s, e)) => new_arr(arr[s..e].to_vec()),
                    None => Value::Undef,
                }
            } else {
                Value::Undef
            }
        }
        _ => Value::Undef,
    }
}

// ---- Hash -----------------------------------------------------------------

/// `Complex` methods. Arithmetic arrives via the numeric hook; this handles the
/// queries, conversions, and the operator methods (`reduce(:+)`).
fn dispatch_complex(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let (re, im) = with_host(|h| h.complex_parts(recv)).unwrap();
    match name {
        "real" => Ok(re),
        "imaginary" | "imag" => Ok(im),
        "to_s" => Ok(new_str(with_host(|h| h.complex_to_s(&re, &im)))),
        "abs" | "magnitude" => {
            // sqrt(re^2 + im^2) as a Float.
            let rf = as_f(&re);
            let imf = as_f(&im);
            Ok(Value::Float((rf * rf + imf * imf).sqrt()))
        }
        "abs2" => {
            let sq = |v: &Value| with_host(|h| h.num_op(fusevm::NumOp::Mul, v, v));
            let rr = sq(&re)?;
            let ii = sq(&im)?;
            with_host(|h| h.num_op(fusevm::NumOp::Add, &rr, &ii))
        }
        "conjugate" | "conj" => {
            let neg_im = with_host(|h| h.num_op(fusevm::NumOp::Sub, &Value::Int(0), &im))?;
            Ok(with_host(|h| h.new_complex(re.clone(), neg_im)))
        }
        "rectangular" | "rect" => Ok(new_arr(vec![re.clone(), im.clone()])),
        // Numeric conversions succeed only for a real Complex (imaginary == 0),
        // delegating to the real part; otherwise MRI raises RangeError.
        "to_i" | "to_int" | "to_f" | "to_r" if as_f(&im) == 0.0 => dispatch(&re, name, &[], None),
        "to_i" | "to_int" | "to_f" | "to_r" => Err(raise_exc(
            "RangeError",
            &format!(
                "can't convert {} into {}",
                with_host(|h| h.complex_to_s(&re, &im)),
                match name {
                    "to_f" => "Float",
                    "to_r" => "Rational",
                    _ => "Integer",
                }
            ),
        )),
        "-@" => {
            let nr = with_host(|h| h.num_op(fusevm::NumOp::Sub, &Value::Int(0), &re))?;
            let ni = with_host(|h| h.num_op(fusevm::NumOp::Sub, &Value::Int(0), &im))?;
            Ok(with_host(|h| h.new_complex(nr, ni)))
        }
        "+@" => Ok(recv.clone()),
        "==" => Ok(Value::Bool(with_host(|h| h.eq_values(recv, &args[0])))),
        "+" | "-" | "*" => {
            let op = match name {
                "+" => fusevm::NumOp::Add,
                "-" => fusevm::NumOp::Sub,
                _ => fusevm::NumOp::Mul,
            };
            with_host(|h| h.num_op(op, recv, &args[0]))
        }
        "**" | "pow" => {
            // Non-negative integer exponent by repeated multiplication.
            match int_arg(&args[0]) {
                Some(e) if e >= 0 => {
                    let mut acc = with_host(|h| h.new_complex(Value::Int(1), Value::Int(0)));
                    for _ in 0..e {
                        acc = with_host(|h| h.num_op(fusevm::NumOp::Mul, &acc, recv))?;
                    }
                    Ok(acc)
                }
                _ => Err(raise_exc(
                    "NotImplementedError",
                    "Complex ** non-integer is not supported",
                )),
            }
        }
        _ => Err(no_method_error(recv, name)),
    }
}

/// `Rational` methods. Arithmetic (`+`/`-`/`*`) arrives via the numeric hook;
/// this handles `/`, `**`, queries, and conversions.
fn dispatch_rational(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    use num_traits::{Signed as _, ToPrimitive as _, Zero as _};
    let r = with_host(|h| h.as_rational(recv)).unwrap();
    let rat = |v: num_rational::BigRational| with_host(|h| h.new_rational(v));
    match name {
        "numerator" => Ok(with_host(|h| h.new_bigint(r.numer().clone()))),
        "denominator" => Ok(with_host(|h| h.new_bigint(r.denom().clone()))),
        "to_f" => Ok(Value::Float(r.to_f64().unwrap_or(f64::NAN))),
        "to_i" | "to_int" | "truncate" => Ok(with_host(|h| h.new_bigint(r.to_integer()))),
        // `#floor`/`#ceil`/`#round` with no digits argument round to the nearest
        // Integer (toward -inf / +inf / nearest). `num_rational::Ratio` provides
        // exact rounding; `to_integer` then extracts the BigInt.
        "floor" if args.is_empty() => Ok(with_host(|h| h.new_bigint(r.floor().to_integer()))),
        "ceil" if args.is_empty() => Ok(with_host(|h| h.new_bigint(r.ceil().to_integer()))),
        "round" if args.is_empty() => Ok(with_host(|h| h.new_bigint(r.round().to_integer()))),
        "to_r" => Ok(recv.clone()),
        "abs" | "magnitude" => Ok(rat(r.abs())),
        "-@" => Ok(rat(-r)),
        "+@" => Ok(recv.clone()),
        "zero?" => Ok(Value::Bool(r.is_zero())),
        "positive?" => Ok(Value::Bool(r.is_positive())),
        "negative?" => Ok(Value::Bool(r.is_negative())),
        "/" | "quo" => match with_host(|h| h.as_rational(&args[0])) {
            Some(d) if !d.is_zero() => Ok(rat(r / d)),
            Some(_) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            None => Ok(Value::Float(
                r.to_f64().unwrap_or(f64::NAN) / as_f(&args[0]),
            )),
        },
        "**" | "pow" => {
            if let Some(exp) = int_arg(&args[0]) {
                let p = if exp >= 0 {
                    num_traits::pow::pow(r, exp as usize)
                } else {
                    num_traits::pow::pow(r.recip(), (-exp) as usize)
                };
                Ok(rat(p))
            } else {
                Ok(Value::Float(
                    r.to_f64().unwrap_or(f64::NAN).powf(as_f(&args[0])),
                ))
            }
        }
        "<=>" => match with_host(|h| h.as_rational(&args[0])) {
            Some(o) => Ok(Value::Int(match r.cmp(&o) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })),
            None => Ok(Value::Undef),
        },
        "coerce" => Ok(new_arr(vec![
            with_host(|h| h.new_rational(h.as_rational(&args[0]).unwrap_or_else(|| r.clone()))),
            recv.clone(),
        ])),
        "hash" => Ok(Value::Int(r.to_i64().unwrap_or(0))),
        "integer?" => Ok(Value::Bool(r.is_integer())),
        // The arithmetic/comparison operators reach here when invoked as methods
        // (`r.+(x)`, e.g. `reduce(:+)`); delegate to the numeric hook.
        "+" | "-" | "*" | "%" | "==" | "<" | ">" | "<=" | ">=" => {
            let op = match name {
                "+" => fusevm::NumOp::Add,
                "-" => fusevm::NumOp::Sub,
                "*" => fusevm::NumOp::Mul,
                "%" => fusevm::NumOp::Mod,
                "==" => fusevm::NumOp::Eq,
                "<" => fusevm::NumOp::Lt,
                ">" => fusevm::NumOp::Gt,
                "<=" => fusevm::NumOp::Le,
                _ => fusevm::NumOp::Ge,
            };
            with_host(|h| h.num_op(op, recv, &args[0]))
        }
        _ => Err(no_method_error(recv, name)),
    }
}

/// `Set` methods. Membership/mutation go through the host `RObj::Set`; set
/// algebra (`|`, `&`, `-`, subset queries) builds fresh sets. Enumerable methods
/// fall through to the Array implementation over the element list.
/// Call `p` with `elem` and return whether the result is truthy.
fn proc_truthy(p: &Value, elem: &Value) -> Result<bool, String> {
    let r = call_proc(p, std::slice::from_ref(elem))?;
    Ok(with_host(|h| h.truthy(&r)))
}

/// `Enumerator::Lazy` methods. Intermediate operations (`map`, `select`, …)
/// append to the deferred pipeline and return a new lazy enumerator; terminal
/// operations (`first`, `force`, `to_a`) pull elements on demand.
fn dispatch_lazy(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    use crate::host::LazyOp;
    let (source, ops) = with_host(|h| h.lazy_parts(recv)).unwrap();
    let extend = |op: LazyOp| -> Value {
        let mut next = ops.clone();
        next.push(op);
        with_host(|h| h.new_lazy(source.clone(), next))
    };
    let blk = || block.clone().ok_or_else(|| "no block given".to_string());
    match name {
        "map" | "collect" => Ok(extend(LazyOp::Map(blk()?))),
        "select" | "filter" => Ok(extend(LazyOp::Select(blk()?))),
        "reject" => Ok(extend(LazyOp::Reject(blk()?))),
        "filter_map" => Ok(extend(LazyOp::FilterMap(blk()?))),
        "flat_map" | "collect_concat" => Ok(extend(LazyOp::FlatMap(blk()?))),
        "take_while" => Ok(extend(LazyOp::TakeWhile(blk()?))),
        "drop_while" => Ok(extend(LazyOp::DropWhile(blk()?))),
        "take" => Ok(extend(LazyOp::Take(as_i(&args[0]).max(0)))),
        "drop" => Ok(extend(LazyOp::Drop(as_i(&args[0]).max(0)))),
        "zip" => {
            let others: Vec<Vec<Value>> = args
                .iter()
                .map(|a| with_host(|h| h.as_array(a)).unwrap_or_default())
                .collect();
            Ok(extend(LazyOp::Zip(others)))
        }
        "lazy" => Ok(recv.clone()),
        "first" => {
            let out = lazy_pull(
                &source,
                &ops,
                args.first().map(|v| as_i(v).max(0) as usize).unwrap_or(1),
            )?;
            match args.first() {
                Some(_) => Ok(new_arr(out)),
                None => Ok(out.into_iter().next().unwrap_or(Value::Undef)),
            }
        }
        "force" | "to_a" | "entries" => Ok(new_arr(lazy_pull(&source, &ops, usize::MAX)?)),
        "each" if block.is_some() => {
            let bl = block.unwrap();
            for v in lazy_pull(&source, &ops, usize::MAX)? {
                call_proc(&bl, std::slice::from_ref(&v))?;
            }
            Ok(recv.clone())
        }
        _ => Err(no_method_error(recv, name)),
    }
}

/// Per-op mutable state during a pull.
enum LazyState {
    Take(i64),
    Drop(i64),
    Dropping(bool),
    /// `zip` cursor: the index of the next element to pair.
    Zip(usize),
    None,
}

/// Feed one element through `ops[i..]`, appending outputs to `out`; returns
/// `false` to stop the whole pull (take-while ended, take exhausted, or limit
/// reached).
fn lazy_feed(
    ops: &[crate::host::LazyOp],
    state: &mut [LazyState],
    i: usize,
    elem: Value,
    out: &mut Vec<Value>,
    limit: usize,
) -> Result<bool, String> {
    use crate::host::LazyOp;
    if out.len() >= limit {
        return Ok(false);
    }
    if i == ops.len() {
        out.push(elem);
        return Ok(out.len() < limit);
    }
    match &ops[i] {
        LazyOp::Map(p) => {
            let v = call_proc(p, std::slice::from_ref(&elem))?;
            lazy_feed(ops, state, i + 1, v, out, limit)
        }
        LazyOp::Select(p) => {
            if proc_truthy(p, &elem)? {
                lazy_feed(ops, state, i + 1, elem, out, limit)
            } else {
                Ok(true)
            }
        }
        LazyOp::Reject(p) => {
            if proc_truthy(p, &elem)? {
                Ok(true)
            } else {
                lazy_feed(ops, state, i + 1, elem, out, limit)
            }
        }
        LazyOp::FilterMap(p) => {
            let v = call_proc(p, std::slice::from_ref(&elem))?;
            if with_host(|h| h.truthy(&v)) {
                lazy_feed(ops, state, i + 1, v, out, limit)
            } else {
                Ok(true)
            }
        }
        LazyOp::FlatMap(p) => {
            let v = call_proc(p, std::slice::from_ref(&elem))?;
            let items = with_host(|h| h.as_array(&v)).unwrap_or_else(|| vec![v]);
            for sub in items {
                if !lazy_feed(ops, state, i + 1, sub, out, limit)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        LazyOp::TakeWhile(p) => {
            if proc_truthy(p, &elem)? {
                lazy_feed(ops, state, i + 1, elem, out, limit)
            } else {
                Ok(false)
            }
        }
        LazyOp::DropWhile(p) => {
            let dropping = matches!(state[i], LazyState::Dropping(true));
            if dropping && proc_truthy(p, &elem)? {
                Ok(true)
            } else {
                state[i] = LazyState::Dropping(false);
                lazy_feed(ops, state, i + 1, elem, out, limit)
            }
        }
        LazyOp::Take(_) => {
            let LazyState::Take(rem) = &mut state[i] else {
                return Ok(false);
            };
            if *rem <= 0 {
                return Ok(false);
            }
            *rem -= 1;
            let last = *rem == 0;
            let cont = lazy_feed(ops, state, i + 1, elem, out, limit)?;
            Ok(cont && !last)
        }
        LazyOp::Drop(_) => {
            let LazyState::Drop(rem) = &mut state[i] else {
                return Ok(true);
            };
            if *rem > 0 {
                *rem -= 1;
                Ok(true)
            } else {
                lazy_feed(ops, state, i + 1, elem, out, limit)
            }
        }
        LazyOp::Zip(others) => {
            let LazyState::Zip(idx) = &mut state[i] else {
                return Ok(false);
            };
            let n = *idx;
            *idx += 1;
            let mut row = Vec::with_capacity(others.len() + 1);
            row.push(elem);
            for other in others {
                row.push(other.get(n).cloned().unwrap_or(Value::Undef));
            }
            let paired = new_arr(row);
            lazy_feed(ops, state, i + 1, paired, out, limit)
        }
    }
}

/// Pull up to `limit` values through the lazy pipeline over `source` (an array
/// or a possibly-endless range).
fn lazy_pull(
    source: &Value,
    ops: &[crate::host::LazyOp],
    limit: usize,
) -> Result<Vec<Value>, String> {
    use crate::host::LazyOp;
    let mut out: Vec<Value> = Vec::new();
    let mut state: Vec<LazyState> = ops
        .iter()
        .map(|op| match op {
            LazyOp::Take(n) => LazyState::Take(*n),
            LazyOp::Drop(n) => LazyState::Drop(*n),
            LazyOp::DropWhile(_) => LazyState::Dropping(true),
            LazyOp::Zip(_) => LazyState::Zip(0),
            _ => LazyState::None,
        })
        .collect();

    if let Some(items) = with_host(|h| h.as_array(source)) {
        for elem in items {
            if !lazy_feed(ops, &mut state, 0, elem, &mut out, limit)? {
                break;
            }
        }
    } else if let Some((lo, hi, excl)) = with_host(|h| h.as_range(source)) {
        let endless = hi == crate::host::RANGE_ENDLESS;
        let mut n = lo;
        loop {
            if endless {
                if out.len() >= limit {
                    break;
                }
            } else if (excl && n >= hi) || (!excl && n > hi) {
                break;
            }
            if !lazy_feed(ops, &mut state, 0, Value::Int(n), &mut out, limit)? {
                break;
            }
            n += 1;
        }
    } else if let Some(gblock) = with_host(|h| h.generator_block(source)) {
        // A generator source: drive it in growing raw-value batches, feeding
        // each value through the pipeline, until it yields `limit` outputs or the
        // generator is exhausted. Re-driving re-runs the (pure) block from the
        // start. A pipeline that never reaches `limit` over an infinite generator
        // (e.g. `.select { false }.first(1)`) loops forever, matching MRI.
        let mut raw_bound = limit.saturating_mul(2).max(16);
        loop {
            out.clear();
            for (op, st) in ops.iter().zip(state.iter_mut()) {
                *st = match op {
                    LazyOp::Take(n) => LazyState::Take(*n),
                    LazyOp::Drop(n) => LazyState::Drop(*n),
                    LazyOp::DropWhile(_) => LazyState::Dropping(true),
                    LazyOp::Zip(_) => LazyState::Zip(0),
                    _ => LazyState::None,
                };
            }
            let raws = drive_generator(&gblock, raw_bound)?;
            let produced = raws.len();
            let mut stopped = false;
            for elem in raws {
                if !lazy_feed(ops, &mut state, 0, elem, &mut out, limit)? {
                    stopped = true;
                    break;
                }
            }
            if out.len() >= limit || produced < raw_bound || stopped {
                break;
            }
            raw_bound = raw_bound.saturating_mul(2);
        }
    }
    Ok(out)
}

/// A concrete `Enumerator` (from a block-less `each`/`map`/…). External
/// iteration (`next`/`peek`/`rewind`/`size`) reads the materialized buffer with
/// a cursor; every other message is delegated to that buffer as an Array, so
/// the full Enumerable surface (`map`, `to_a`, `with_index`, `select`, …) keeps
/// working just as it did when these calls returned a bare array.
fn dispatch_enumerator(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    if let Some(gblock) = with_host(|h| h.generator_block(recv)) {
        return dispatch_generator(recv, &gblock, name, args, block);
    }
    let buf = with_host(|h| h.enum_buf(recv).unwrap_or_default());
    match name {
        "next" if args.is_empty() && block.is_none() => {
            match with_host(|h| h.enum_next(recv, true)) {
                Some(v) => Ok(v),
                None => Err(raise_exc("StopIteration", "iteration reached an end")),
            }
        }
        "peek" if args.is_empty() && block.is_none() => {
            match with_host(|h| h.enum_next(recv, false)) {
                Some(v) => Ok(v),
                None => Err(raise_exc("StopIteration", "iteration reached an end")),
            }
        }
        "rewind" if args.is_empty() && block.is_none() => {
            with_host(|h| h.enum_rewind(recv));
            Ok(recv.clone())
        }
        "size" | "length" if args.is_empty() && block.is_none() => Ok(Value::Int(buf.len() as i64)),
        // `with_index(offset=0)` re-attaches a block that also receives a running
        // index. What it returns depends on the method that built this
        // Enumerator: `map`/`collect`/`flat_map` collect the block's results,
        // `select`/`filter`/`reject` filter the elements, `each` (and anything
        // else) runs for side effects and returns the receiver's elements.
        "with_index" if block.is_some() => {
            let offset = match args.first() {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            let b = block.unwrap();
            let method = with_host(|h| h.enum_method(recv).unwrap_or_default());
            let mut collected: Vec<Value> = Vec::new();
            for (i, x) in buf.iter().enumerate() {
                let r = call_proc(&b, &[x.clone(), Value::Int(offset + i as i64)])?;
                match method.as_str() {
                    "map" | "collect" => collected.push(r),
                    "flat_map" | "collect_concat" => match with_host(|h| h.as_array(&r)) {
                        Some(xs) => collected.extend(xs),
                        None => collected.push(r),
                    },
                    "select" | "filter" => {
                        if with_host(|h| h.truthy(&r)) {
                            collected.push(x.clone());
                        }
                    }
                    "reject" => {
                        if !with_host(|h| h.truthy(&r)) {
                            collected.push(x.clone());
                        }
                    }
                    // `each` / `each_with_index` / unknown: side effects only,
                    // return the enumerated elements (MRI returns the receiver).
                    _ => collected.push(x.clone()),
                }
            }
            Ok(new_arr(collected))
        }
        // `with_object(memo)` threads a memo object through the block and
        // returns it, regardless of the source method.
        "with_object" | "each_with_object" if block.is_some() && args.len() == 1 => {
            let memo = args[0].clone();
            let b = block.unwrap();
            for x in &buf {
                call_proc(&b, &[x.clone(), memo.clone()])?;
            }
            Ok(memo)
        }
        // `with_index` without a block yields `[elem, offset+index]` pairs.
        "with_index" if block.is_none() => {
            let offset = match args.first() {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            let pairs: Vec<Value> = buf
                .iter()
                .enumerate()
                .map(|(i, x)| new_arr(vec![x.clone(), Value::Int(offset + i as i64)]))
                .collect();
            Ok(with_host(|h| h.new_enumerator(pairs, "each")))
        }
        // Every non-iteration message is delegated to the buffered values as an
        // Array, preserving the full Enumerable surface (`map`, `to_a`,
        // `select`, …) that block-less calls exposed before Enumerator existed.
        _ => remap_array_delegate(dispatch_array(&new_arr(buf), name, args, block), recv, name),
    }
}

/// The native `Enumerator::Yielder` handed to a generator block as `|y|`.
/// `<<`/`yield` push into the yielder's collector; hitting the drive's `limit`
/// raises a break signal that unwinds the generator body (bounding infinite
/// `loop {}` generators for `first(n)`/`take(n)`).
fn dispatch_yielder(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "<<" | "yield" => {
            let v = match args {
                [single] => single.clone(),
                many => new_arr(many.to_vec()),
            };
            if with_host(|h| h.yielder_push(recv, v)) {
                raise_signal_break(Value::Undef);
            }
            // `<<` returns the yielder (so `y << 1 << 2` chains); `yield` returns
            // nil in MRI, but generator blocks discard it, so `recv` is faithful.
            Ok(recv.clone())
        }
        _ => Err(no_method_error(recv, name)),
    }
}

/// Run a generator block, collecting yielded values, stopping once `limit`
/// values are produced. `usize::MAX` runs the block to completion (finite
/// generators / `to_a`). The yielder's limiter raises a break signal to unwind
/// infinite `loop {}`/`while` generators; that break is expected and cleared.
fn drive_generator(gblock: &Value, limit: usize) -> Result<Vec<Value>, String> {
    let yielder = with_host(|h| h.new_yielder(limit));
    let r = call_proc(gblock, std::slice::from_ref(&yielder));
    let buf = with_host(|h| h.take_enum_sink());
    // Clear the limiter's break (a generator block never breaks on its own).
    take_break();
    r?;
    Ok(buf)
}

/// Instance methods on a `Fiber`: `resume(*args)`, `alive?`.
/// `Thread` instance methods. `join`/`value` release the GVL and wait for the OS
/// thread; `value` returns the block's value (re-raising a thread exception),
/// `join` returns the thread.
fn dispatch_thread(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        // `thread[:key]` / `thread[:key] = value` — thread-local storage, backed
        // by the same process-global store as `Fiber[]` (single-thread-boot scope).
        "[]" => {
            let store = with_host(fiber_store);
            dispatch(&store, "[]", args, None)
        }
        "[]=" => {
            let store = with_host(fiber_store);
            dispatch(&store, "[]=", args, None)
        }
        "join" => {
            crate::host::thread_join(recv)?;
            Ok(recv.clone())
        }
        "value" => crate::host::thread_join(recv),
        "alive?" => Ok(Value::Bool(crate::host::thread_alive(recv))),
        "status" => Ok(if crate::host::thread_alive(recv) {
            new_str("run".to_string())
        } else {
            Value::Bool(false)
        }),
        "name" => Ok(Value::Undef),
        // A Thread compares/inspects by identity like any object.
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.inspect(recv)))),
        _ => Err(no_method_error(recv, name)),
    }
}

/// The process-global store backing `Fiber[]`/`Thread#[]` (a lazily-created
/// Hash). Not per-fiber/per-thread — sufficient for a single-fiber boot.
fn fiber_store(h: &mut crate::host::RubyHost) -> Value {
    match h.get_global("__fiber_storage__") {
        Value::Undef => {
            let hh = h.new_hash(IndexMap::new());
            h.set_global("__fiber_storage__", hh.clone());
            hh
        }
        v => v,
    }
}

fn dispatch_fiber(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "resume" => {
            let v = match args {
                [] => Value::Undef,
                [one] => one.clone(),
                many => new_arr(many.to_vec()),
            };
            crate::host::fiber_resume(recv, v)
        }
        "alive?" => Ok(Value::Bool(crate::host::fiber_alive(recv))),
        "inspect" | "to_s" => Ok(new_str(format!(
            "#<Fiber (created){}>",
            if crate::host::fiber_alive(recv) {
                ""
            } else {
                " (dead)"
            }
        ))),
        _ => Err(no_method_error(recv, name)),
    }
}

// ---- File / IO / Dir ------------------------------------------------------

/// Read a required string argument at index `i` (path/data). Coerces via
/// `String`-conversion; empty string if absent (callers validate arity above).
fn str_arg(args: &[Value], i: usize) -> String {
    args.get(i)
        .and_then(|a| with_host(|h| h.as_str(a)))
        .unwrap_or_default()
}

/// `IOError` carrying `msg` — the raise form used by closed-stream operations.
fn io_err(msg: &str) -> String {
    raise_exc("IOError", msg)
}

/// Map an `errno`-style host error string into a Ruby `Errno::ENOENT`-shaped
/// `SystemCallError` message. We don't model the full `Errno` hierarchy, so a
/// generic `SystemCallError` carries the OS text (faithful message, simpler
/// class).
fn sys_err(op: &str, path: &str, e: &std::io::Error) -> String {
    raise_exc("SystemCallError", &format!("{op} - {path}: {e}"))
}

/// Class methods shared by `File` and `IO`. Returns `None` for a name this
/// dispatcher doesn't own (so the caller falls through to the generic path).
fn dispatch_file_class(
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Option<Result<Value, String>> {
    Some(match name {
        "read" => {
            let path = str_arg(args, 0);
            match std::fs::read_to_string(&path) {
                Ok(s) => Ok(new_str(s)),
                Err(e) => Err(sys_err("No such file or directory @ rb_sysopen", &path, &e)),
            }
        }
        "write" => {
            let path = str_arg(args, 0);
            let data = str_arg(args, 1);
            match std::fs::write(&path, data.as_bytes()) {
                Ok(()) => Ok(Value::Int(data.len() as i64)),
                Err(e) => Err(sys_err("open", &path, &e)),
            }
        }
        "readlines" => {
            let path = str_arg(args, 0);
            match std::fs::read_to_string(&path) {
                Ok(s) => Ok(new_arr(
                    s.split_inclusive('\n')
                        .map(|l| new_str(l.to_string()))
                        .collect::<Vec<_>>(),
                )),
                Err(e) => Err(sys_err("No such file or directory @ rb_sysopen", &path, &e)),
            }
        }
        "foreach" => {
            let path = str_arg(args, 0);
            let s = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return Some(Err(sys_err(
                        "No such file or directory @ rb_sysopen",
                        &path,
                        &e,
                    )))
                }
            };
            match &block {
                Some(bl) => {
                    for line in s.split_inclusive('\n') {
                        let lv = new_str(line.to_string());
                        if let Err(e) = call_proc(bl, std::slice::from_ref(&lv)) {
                            return Some(Err(e));
                        }
                    }
                    Ok(Value::Undef)
                }
                // Block-less `foreach` yields an Enumerator over the lines.
                None => Ok(with_host(|h| {
                    let lines: Vec<Value> = s
                        .split_inclusive('\n')
                        .map(|l| h.new_string(l.to_string()))
                        .collect();
                    h.new_enumerator(lines, "each")
                })),
            }
        }
        "open" => return Some(file_open(args, block)),
        "exist?" | "exists?" => Ok(Value::Bool(
            std::path::Path::new(&str_arg(args, 0)).exists(),
        )),
        "file?" => Ok(Value::Bool(
            std::path::Path::new(&str_arg(args, 0)).is_file(),
        )),
        "directory?" => Ok(Value::Bool(
            std::path::Path::new(&str_arg(args, 0)).is_dir(),
        )),
        "size" => {
            let path = str_arg(args, 0);
            match std::fs::metadata(&path) {
                Ok(m) => Ok(Value::Int(m.len() as i64)),
                Err(e) => Err(sys_err(
                    "No such file or directory @ rb_file_s_stat",
                    &path,
                    &e,
                )),
            }
        }
        "delete" | "unlink" => {
            let mut count = 0i64;
            for a in args {
                let path = with_host(|h| h.as_str(a)).unwrap_or_default();
                if let Err(e) = std::fs::remove_file(&path) {
                    return Some(Err(sys_err("unlink", &path, &e)));
                }
                count += 1;
            }
            Ok(Value::Int(count))
        }
        "basename" => {
            let ext = args.get(1).and_then(|a| with_host(|h| h.as_str(a)));
            Ok(new_str(path_basename(&str_arg(args, 0), ext.as_deref())))
        }
        "dirname" => Ok(new_str(path_dirname(&str_arg(args, 0)))),
        "extname" => Ok(new_str(path_extname(&str_arg(args, 0)))),
        "join" => Ok(new_str(path_join(args))),
        "expand_path" => {
            let base = args.get(1).and_then(|a| with_host(|h| h.as_str(a)));
            Ok(new_str(path_expand(&str_arg(args, 0), base.as_deref())))
        }
        _ => return None,
    })
}

/// `File.open(path, mode="r")`. With a block: yields the IO, closes it on exit,
/// returns the block's value. Without: returns the open IO.
fn file_open(args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let path = str_arg(args, 0);
    let mode = args
        .get(1)
        .and_then(|a| with_host(|h| h.as_str(a)))
        .unwrap_or_else(|| "r".to_string());
    let mut opts = std::fs::OpenOptions::new();
    match mode.trim_end_matches('b') {
        "r" | "r+" => {
            opts.read(true).write(mode.contains('+'));
        }
        "w" | "w+" => {
            opts.write(true)
                .create(true)
                .truncate(true)
                .read(mode.contains('+'));
        }
        "a" | "a+" => {
            opts.append(true).create(true).read(mode.contains('+'));
        }
        _ => {
            opts.read(true);
        }
    }
    let file = opts
        .open(&path)
        .map_err(|e| sys_err("No such file or directory @ rb_sysopen", &path, &e))?;
    let io = crate::host::io_alloc_file(file, path);
    match block {
        Some(bl) => {
            let r = call_proc(&bl, std::slice::from_ref(&io));
            let _ = crate::host::io_close(&io);
            r
        }
        None => Ok(io),
    }
}

/// Instance methods on an `IO`/`File` handle (from `File.open`, or `$stdout`
/// et al.).
fn dispatch_io(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        "write" => {
            let mut total = 0usize;
            for a in args {
                let s = display(a);
                total += crate::host::io_write_str(recv, &s).map_err(|e| io_err(&e))?;
            }
            Ok(Value::Int(total as i64))
        }
        "<<" => {
            let s = display(&args[0]);
            crate::host::io_write_str(recv, &s).map_err(|e| io_err(&e))?;
            Ok(recv.clone())
        }
        "print" => {
            let mut out = String::new();
            for a in args {
                out.push_str(&display(a));
            }
            crate::host::io_write_str(recv, &out).map_err(|e| io_err(&e))?;
            Ok(Value::Undef)
        }
        "puts" => {
            if args.is_empty() {
                crate::host::io_write_str(recv, "\n").map_err(|e| io_err(&e))?;
            }
            for a in args {
                io_puts_arg(recv, a)?;
            }
            Ok(Value::Undef)
        }
        "read" => {
            let s = crate::host::io_read_all(recv).map_err(|e| io_err(&e))?;
            Ok(new_str(s))
        }
        "gets" => crate::host::io_gets(recv).map_err(|e| io_err(&e)),
        "readlines" => Ok(new_arr(
            crate::host::io_readlines(recv).map_err(|e| io_err(&e))?,
        )),
        "each_line" | "each" => {
            let lines = crate::host::io_readlines(recv).map_err(|e| io_err(&e))?;
            match block {
                Some(bl) => {
                    for l in &lines {
                        call_proc(&bl, std::slice::from_ref(l))?;
                    }
                    Ok(recv.clone())
                }
                None => Ok(with_host(|h| h.new_enumerator(lines, "each"))),
            }
        }
        "close" => {
            crate::host::io_close(recv).map_err(|e| io_err(&e))?;
            Ok(Value::Undef)
        }
        "closed?" => Ok(Value::Bool(crate::host::io_closed(recv))),
        "flush" | "sync" | "fsync" => {
            crate::host::io_flush(recv).map_err(|e| io_err(&e))?;
            Ok(recv.clone())
        }
        "sync=" => Ok(args.first().cloned().unwrap_or(Value::Undef)),
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        _ => Err(no_method_error(recv, name)),
    }
}

// ---- TCP sockets ----------------------------------------------------------

/// Raise a `SocketError` carrying the host-side OS error text.
fn socket_err(e: &str) -> String {
    raise_exc("SocketError", e)
}

/// Parse a TCP port from an Integer or a numeric String argument.
fn tcp_port_arg(v: &Value) -> Result<u16, String> {
    match v {
        Value::Int(n) => {
            u16::try_from(*n).map_err(|_| raise_exc("SocketError", "invalid port number"))
        }
        _ => {
            let s = with_host(|h| h.as_str(v)).unwrap_or_default();
            s.trim().parse::<u16>().map_err(|_| {
                raise_exc(
                    "SocketError",
                    &format!("getaddrinfo: Servname not supported for ai_socktype: {s}"),
                )
            })
        }
    }
}

/// `TCPServer.new`/`.open` and `TCPSocket.new`/`.open`. For `TCPServer` a block
/// (`.open`) yields the server and closes it on return; for `TCPSocket` a block
/// yields the connected socket and closes it on return.
fn tcp_class_new(cls: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let sock = if cls == "TCPServer" {
        let (host, port) = match args {
            [p] => ("0.0.0.0".to_string(), tcp_port_arg(p)?),
            [h, p] => {
                let host = if matches!(h, Value::Undef) {
                    "0.0.0.0".to_string()
                } else {
                    with_host(|hh| hh.as_str(h)).unwrap_or_else(|| "0.0.0.0".to_string())
                };
                (host, tcp_port_arg(p)?)
            }
            _ => return Err(raise_exc("ArgumentError", "wrong number of arguments")),
        };
        crate::host::tcp_listen(&host, port).map_err(|e| socket_err(&e))?
    } else {
        // TCPSocket.new(host, port)
        let (h, p) = match args {
            [h, p] => (h, p),
            _ => return Err(raise_exc("ArgumentError", "wrong number of arguments")),
        };
        let host = with_host(|hh| hh.as_str(h)).unwrap_or_default();
        crate::host::tcp_connect(&host, tcp_port_arg(p)?).map_err(|e| socket_err(&e))?
    };
    match block {
        Some(bl) => {
            let r = call_proc(&bl, std::slice::from_ref(&sock));
            let _ = crate::host::tcp_close(&sock);
            r
        }
        None => Ok(sock),
    }
}

/// Instance methods on a `TCPServer` handle: `accept`, `addr`, `close`, `closed?`.
fn dispatch_tcp_server(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        "accept" => crate::host::tcp_accept(recv).map_err(|e| socket_err(&e)),
        // `accept_nonblock` is best-effort: with buffered sockets we have no
        // pending queue to peek, so it behaves like a blocking `accept`.
        "accept_nonblock" => crate::host::tcp_accept(recv).map_err(|e| socket_err(&e)),
        "addr" | "local_address" => crate::host::tcp_addr(recv, false).map_err(|e| io_err(&e)),
        "listen" => Ok(Value::Int(0)),
        "close" => {
            crate::host::tcp_close(recv).map_err(|e| io_err(&e))?;
            Ok(Value::Undef)
        }
        "closed?" => Ok(Value::Bool(crate::host::tcp_closed(recv))),
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        _ => {
            let _ = (args, block);
            Err(no_method_error(recv, name))
        }
    }
}

/// Instance methods on a `TCPSocket` handle: the read/write surface needed to
/// serve or issue an HTTP request.
fn dispatch_tcp_socket(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        "write" => {
            let mut total = 0usize;
            for a in args {
                let s = display(a);
                total += crate::host::tcp_write(recv, &s).map_err(|e| io_err(&e))?;
            }
            Ok(Value::Int(total as i64))
        }
        "<<" => {
            crate::host::tcp_write(recv, &display(&args[0])).map_err(|e| io_err(&e))?;
            Ok(recv.clone())
        }
        "print" => {
            let mut out = String::new();
            for a in args {
                out.push_str(&display(a));
            }
            crate::host::tcp_write(recv, &out).map_err(|e| io_err(&e))?;
            Ok(Value::Undef)
        }
        "puts" => {
            if args.is_empty() {
                crate::host::tcp_write(recv, "\n").map_err(|e| io_err(&e))?;
            }
            for a in args {
                tcp_puts_arg(recv, a)?;
            }
            Ok(Value::Undef)
        }
        "gets" => crate::host::tcp_gets(recv).map_err(|e| io_err(&e)),
        "read" => {
            let n = args.first().and_then(int_arg).map(|n| n.max(0) as usize);
            crate::host::tcp_read(recv, n).map_err(|e| io_err(&e))
        }
        "readpartial" => {
            let n = args.first().and_then(int_arg).unwrap_or(0).max(0) as usize;
            match crate::host::tcp_readpartial(recv, n).map_err(|e| io_err(&e))? {
                Some(v) => Ok(v),
                None => Err(raise_exc("EOFError", "end of file reached")),
            }
        }
        "read_nonblock" => {
            let n = args.first().and_then(int_arg).unwrap_or(0).max(0) as usize;
            match crate::host::tcp_read_nonblock(recv, n) {
                Ok(Some(v)) => Ok(v),
                Ok(None) => Err(raise_exc("EOFError", "end of file reached")),
                Err(e) if e == "__EAGAIN__" => Err(raise_exc(
                    "IO::EAGAINWaitReadable",
                    "Resource temporarily unavailable - read would block",
                )),
                Err(e) => Err(io_err(&e)),
            }
        }
        "each_line" | "each" => {
            let mut results = Vec::new();
            loop {
                match crate::host::tcp_gets(recv).map_err(|e| io_err(&e))? {
                    Value::Undef => break,
                    line => match &block {
                        Some(bl) => {
                            call_proc(bl, std::slice::from_ref(&line))?;
                        }
                        None => results.push(line),
                    },
                }
            }
            match block {
                Some(_) => Ok(recv.clone()),
                None => Ok(with_host(|h| h.new_enumerator(results, "each"))),
            }
        }
        "addr" | "local_address" => crate::host::tcp_addr(recv, false).map_err(|e| io_err(&e)),
        "peeraddr" | "remote_address" => crate::host::tcp_addr(recv, true).map_err(|e| io_err(&e)),
        "flush" | "sync" | "fsync" => Ok(recv.clone()),
        "sync=" => Ok(args.first().cloned().unwrap_or(Value::Undef)),
        "close" | "close_write" | "close_read" => {
            crate::host::tcp_close(recv).map_err(|e| io_err(&e))?;
            Ok(Value::Undef)
        }
        "closed?" => Ok(Value::Bool(crate::host::tcp_closed(recv))),
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        _ => Err(no_method_error(recv, name)),
    }
}

/// `TCPSocket#puts` for one argument: arrays flatten; scalars get a trailing
/// `\n` only if their string form lacks one (mirrors `io_puts_arg`).
fn tcp_puts_arg(recv: &Value, v: &Value) -> Result<(), String> {
    if let Some(arr) = with_host(|h| h.as_array(v)) {
        if arr.is_empty() {
            crate::host::tcp_write(recv, "\n").map_err(|e| io_err(&e))?;
        }
        for e in &arr {
            tcp_puts_arg(recv, e)?;
        }
    } else {
        let mut s = display(v);
        if !s.ends_with('\n') {
            s.push('\n');
        }
        crate::host::tcp_write(recv, &s).map_err(|e| io_err(&e))?;
    }
    Ok(())
}

// ---- SQLite3::Database ------------------------------------------------------

use crate::host::SqlVal;

/// Raise a `SQLite3::SQLException` (a `StandardError` — a bare `rescue` catches
/// it, as does `rescue SQLite3::SQLException`). Carries the sqlite error text.
fn sqlite_err(msg: &str) -> String {
    raise_exc("SQLite3::SQLException", msg)
}

/// `SQLite3::Database.new(path[, opts])` / `.open`. `path` defaults to an
/// in-memory DB. The block form `Database.new(path) { |db| ... }` yields the
/// handle, closes it afterward (even on error), and returns the block's value.
fn sqlite_db_new(args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let path = args
        .first()
        .map(arg_str)
        .unwrap_or_else(|| ":memory:".to_string());
    let db = crate::host::db_open(&path).map_err(|e| sqlite_err(&e))?;
    // An options hash (`results_as_hash: true`) is honored; other keys ignored.
    if let Some(opts) = args.get(1).and_then(|a| with_host(|h| h.as_hash(a))) {
        if let Some(v) = opts.get(&RKey::Sym("results_as_hash".into())) {
            crate::host::db_set_results_as_hash(&db, with_host(|h| h.truthy(v)));
        }
    }
    if let Some(b) = block {
        let r = call_proc(&b, std::slice::from_ref(&db));
        crate::host::db_close(&db);
        return r;
    }
    Ok(db)
}

/// One sqlite column value → Ruby: INTEGER→Integer, REAL→Float, TEXT→String,
/// NULL→nil, BLOB→String (bytes, lossily decoded as UTF-8 since host Strings are
/// UTF-8).
fn sqlval_to_ruby(v: &SqlVal) -> Value {
    match v {
        SqlVal::Null => Value::Undef,
        SqlVal::Integer(n) => Value::Int(*n),
        SqlVal::Real(f) => Value::Float(*f),
        SqlVal::Text(s) => new_str(s.clone()),
        SqlVal::Blob(b) => new_str(String::from_utf8_lossy(b).into_owned()),
    }
}

/// One Ruby bind value → sqlite: nil→NULL, Integer→INTEGER, Float→REAL,
/// true/false→INTEGER 1/0, String→TEXT, anything else→its `to_s` as TEXT.
fn ruby_to_sqlval(v: &Value) -> SqlVal {
    match v {
        Value::Undef => SqlVal::Null,
        Value::Bool(b) => SqlVal::Integer(if *b { 1 } else { 0 }),
        Value::Int(n) => SqlVal::Integer(*n),
        Value::Float(f) => SqlVal::Real(*f),
        Value::Str(s) => SqlVal::Text(s.to_string()),
        _ => match with_host(|h| h.as_str(v)) {
            Some(s) => SqlVal::Text(s),
            None => SqlVal::Text(with_host(|h| h.to_s(v))),
        },
    }
}

/// The bind values for `execute`/`execute2`/`query`, whose gem signature is
/// `execute(sql, bind_vars = [])` — a single container, not varargs. An Array is
/// used as-is; a lone scalar is auto-wrapped to a single bind; absent is empty.
fn binds_from_one(arg: Option<&Value>) -> Vec<SqlVal> {
    let flat: Vec<Value> = match arg {
        Some(a) => with_host(|h| h.as_array(a)).unwrap_or_else(|| vec![a.clone()]),
        None => Vec::new(),
    };
    flat.iter().map(ruby_to_sqlval).collect()
}

/// The bind values for `get_first_row`/`get_first_value`, whose gem signature is
/// `(sql, *bind_vars)` — trailing varargs. A single Array argument is still
/// treated as the bind list (gem-compatible).
fn collect_binds(rest: &[Value]) -> Vec<SqlVal> {
    let flat: Vec<Value> = match rest {
        [only] => with_host(|h| h.as_array(only)).unwrap_or_else(|| vec![only.clone()]),
        many => many.to_vec(),
    };
    flat.iter().map(ruby_to_sqlval).collect()
}

/// Build one Ruby result row: an Array of column values, or (when
/// `results_as_hash`) a Hash keyed by column name.
fn build_row(cols: &[String], row: &[SqlVal], as_hash: bool) -> Value {
    if as_hash {
        let mut map = IndexMap::new();
        for (c, val) in cols.iter().zip(row.iter()) {
            map.insert(RKey::Str(c.clone()), sqlval_to_ruby(val));
        }
        with_host(|h| h.new_hash(map))
    } else {
        new_arr(row.iter().map(sqlval_to_ruby).collect())
    }
}

/// `db.execute` / `db.execute2`. Prepares, binds, runs, and returns an Array of
/// rows (empty for DDL/DML). With a block, yields each row and returns nil.
/// `execute2` prepends a header row of the column names.
fn sqlite_execute(
    recv: &Value,
    args: &[Value],
    block: Option<Value>,
    with_headers: bool,
) -> Result<Value, String> {
    let sql = args.first().map(arg_str).unwrap_or_default();
    let binds = binds_from_one(args.get(1));
    let (cols, rows) = crate::host::db_execute(recv, &sql, &binds).map_err(|e| sqlite_err(&e))?;
    let as_hash = crate::host::db_results_as_hash(recv);
    let mut ruby_rows: Vec<Value> = rows.iter().map(|r| build_row(&cols, r, as_hash)).collect();
    if with_headers {
        // `execute2`'s first row is the column names (always an Array of strings,
        // matching the gem even when results_as_hash is on).
        let header = new_arr(cols.iter().map(|c| new_str(c.clone())).collect());
        ruby_rows.insert(0, header);
    }
    if let Some(b) = block {
        for row in &ruby_rows {
            call_proc(&b, std::slice::from_ref(row))?;
        }
        return Ok(Value::Undef);
    }
    Ok(new_arr(ruby_rows))
}

/// Instance methods on a `SQLite3::Database` handle.
fn dispatch_sqlite_db(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        // `query` is the gem's low-level statement runner; here it returns rows
        // like `execute` (we do not model the streaming `ResultSet` object).
        "execute" | "query" => sqlite_execute(recv, args, block, false),
        "execute2" => sqlite_execute(recv, args, block, true),
        // `execute_batch` runs multiple `;`-separated statements, no result rows.
        "execute_batch" => {
            let sql = args.first().map(arg_str).unwrap_or_default();
            for stmt in sql.split(';') {
                if stmt.trim().is_empty() {
                    continue;
                }
                crate::host::db_execute(recv, stmt, &[]).map_err(|e| sqlite_err(&e))?;
            }
            Ok(Value::Undef)
        }
        "get_first_row" => {
            let sql = args.first().map(arg_str).unwrap_or_default();
            let binds = collect_binds(&args[1.min(args.len())..]);
            let (cols, rows) =
                crate::host::db_execute(recv, &sql, &binds).map_err(|e| sqlite_err(&e))?;
            let as_hash = crate::host::db_results_as_hash(recv);
            Ok(rows
                .first()
                .map(|r| build_row(&cols, r, as_hash))
                .unwrap_or(Value::Undef))
        }
        "get_first_value" => {
            let sql = args.first().map(arg_str).unwrap_or_default();
            let binds = collect_binds(&args[1.min(args.len())..]);
            let (_cols, rows) =
                crate::host::db_execute(recv, &sql, &binds).map_err(|e| sqlite_err(&e))?;
            Ok(rows
                .first()
                .and_then(|r| r.first())
                .map(sqlval_to_ruby)
                .unwrap_or(Value::Undef))
        }
        "last_insert_row_id" => Ok(Value::Int(crate::host::db_last_insert_rowid(recv))),
        "changes" => Ok(Value::Int(crate::host::db_changes(recv))),
        "results_as_hash=" => {
            let on = with_host(|h| h.truthy(&args[0]));
            crate::host::db_set_results_as_hash(recv, on);
            Ok(args.first().cloned().unwrap_or(Value::Undef))
        }
        "results_as_hash" => Ok(Value::Bool(crate::host::db_results_as_hash(recv))),
        "close" => {
            crate::host::db_close(recv);
            Ok(Value::Undef)
        }
        "closed?" => Ok(Value::Bool(crate::host::db_closed(recv))),
        "open?" => Ok(Value::Bool(!crate::host::db_closed(recv))),
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        _ => {
            let _ = block;
            Err(no_method_error(recv, name))
        }
    }
}

// ---- Fiddle (FFI) -----------------------------------------------------------
//
// Ruby's built-in FFI stdlib. `Fiddle.dlopen`/`Fiddle::Handle` wrap a `dlopen`ed
// shared library (host side table); `Fiddle::Function` binds a C function
// address to a runtime signature and calls it through libffi; `Fiddle::Pointer`
// wraps a raw address so a returned `char*` reads back as a Ruby String.

use libffi::middle::{arg as ffi_arg, Arg, Cif, CodePtr, Type};
use std::ffi::c_void;

/// Raise `Fiddle::DLError` — MRI's error class for dlopen/dlsym/type failures.
/// (`*Error` names are rescuable class refs; see `is_builtin_exception`.)
fn fiddle_err(msg: &str) -> String {
    raise_exc("Fiddle::DLError", msg)
}

/// The MRI integer value of a `Fiddle::TYPE_*`/`ALIGN_*` constant, reached as a
/// method send on the `Fiddle` module ref (exactly like `Math::PI`). A negative
/// type code is the unsigned variant of its magnitude (`TYPE_SIZE_T` = -5 =
/// unsigned `TYPE_LONG`), matching MRI's own values.
fn fiddle_type_const(name: &str) -> Option<i64> {
    Some(match name {
        "TYPE_VOID" => 0,
        "TYPE_VOIDP" => 1,
        "TYPE_CHAR" => 2,
        "TYPE_SHORT" => 3,
        "TYPE_INT" => 4,
        "TYPE_LONG" => 5,
        "TYPE_LONG_LONG" => 6,
        "TYPE_FLOAT" => 7,
        "TYPE_DOUBLE" => 8,
        "TYPE_SIZE_T" => -5,
        "TYPE_SSIZE_T" => 5,
        "TYPE_PTRDIFF_T" => 5,
        "TYPE_INTPTR_T" => 5,
        "TYPE_UINTPTR_T" => -5,
        "TYPE_UCHAR" => -2,
        "TYPE_USHORT" => -3,
        "TYPE_UINT" => -4,
        "TYPE_ULONG" => -5,
        "TYPE_ULONG_LONG" => -6,
        "ALIGN_VOIDP" | "SIZEOF_VOIDP" | "SIZEOF_LONG" | "SIZEOF_LONG_LONG" => 8,
        "SIZEOF_INT" => 4,
        "SIZEOF_SHORT" => 2,
        "SIZEOF_CHAR" => 1,
        "SIZEOF_DOUBLE" => 8,
        "SIZEOF_FLOAT" => 4,
        _ => return None,
    })
}

/// MRI Fiddle type code → libffi `Type`. `None` for an unknown code. `TYPE_VOID`
/// (0) is valid only as a return type.
fn fiddle_ffi_type(code: i32) -> Option<Type> {
    let unsigned = code < 0;
    let t = match (code.abs(), unsigned) {
        (0, _) => Type::void(),
        (1, _) => Type::pointer(), // TYPE_VOIDP
        (2, false) => Type::i8(),  // TYPE_CHAR
        (2, true) => Type::u8(),
        (3, false) => Type::c_short(), // TYPE_SHORT
        (3, true) => Type::u16(),
        (4, false) => Type::c_int(), // TYPE_INT
        (4, true) => Type::c_uint(),
        (5, false) => Type::c_long(),     // TYPE_LONG
        (5, true) => Type::usize(),       // TYPE_SIZE_T (unsigned long == usize on LP64)
        (6, false) => Type::c_longlong(), // TYPE_LONG_LONG
        (6, true) => Type::u64(),
        (7, _) => Type::f32(), // TYPE_FLOAT
        (8, _) => Type::f64(), // TYPE_DOUBLE
        _ => return None,
    };
    Some(t)
}

/// A single marshalled C argument, stored in correctly-sized native storage so
/// libffi reads the right number of bytes. `Ptr` carries a raw pointer (a
/// String's char*, another Pointer's address, or a raw integer address).
enum FiddleArg {
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Ptr(*const c_void),
}

/// `Fiddle::Function#call(*args)` — marshal Ruby args to C via libffi, invoke the
/// bound function, and marshal the C result back to Ruby.
fn fiddle_call(recv: &Value, args: &[Value]) -> Result<Value, String> {
    let (addr, argtypes, ret) =
        crate::host::fiddle_func_parts(recv).ok_or_else(|| fiddle_err("not a Fiddle::Function"))?;
    if args.len() != argtypes.len() {
        return Err(raise_exc(
            "ArgumentError",
            &format!(
                "wrong number of arguments (given {}, expected {})",
                args.len(),
                argtypes.len()
            ),
        ));
    }
    // Keep-alive buffers for `TYPE_VOIDP` String args: the char* handed to C must
    // stay valid for the whole call, so its NUL-terminated bytes live here.
    let mut keep: Vec<Vec<u8>> = Vec::new();
    let mut store: Vec<FiddleArg> = Vec::with_capacity(args.len());
    let mut ffi_types: Vec<Type> = Vec::with_capacity(args.len());
    for (v, &code) in args.iter().zip(argtypes.iter()) {
        ffi_types.push(
            fiddle_ffi_type(code)
                .ok_or_else(|| fiddle_err(&format!("unknown type code {code}")))?,
        );
        let s = match code.abs() {
            1 => {
                // TYPE_VOIDP: String → char*; Fiddle::Pointer → its address;
                // nil → NULL; Integer → a raw address.
                let p: *const c_void = if matches!(v, Value::Undef) {
                    std::ptr::null()
                } else if let Some(s) = with_host(|h| h.as_str(v)) {
                    let mut b = s.into_bytes();
                    b.push(0);
                    let ptr = b.as_ptr() as *const c_void;
                    keep.push(b);
                    ptr
                } else if let Some((paddr, _)) = crate::host::fiddle_ptr_parts(v) {
                    paddr as *const c_void
                } else {
                    (as_i(v) as u64) as *const c_void
                };
                FiddleArg::Ptr(p)
            }
            2 => FiddleArg::I8(as_i(v) as i8),
            3 => FiddleArg::I16(as_i(v) as i16),
            4 => FiddleArg::I32(as_i(v) as i32),
            5 | 6 => FiddleArg::I64(int_arg(v).unwrap_or_else(|| as_i(v))),
            7 => FiddleArg::F32(as_f(v) as f32),
            8 => FiddleArg::F64(as_f(v)),
            _ => {
                return Err(fiddle_err(&format!(
                    "unsupported argument type code {code}"
                )))
            }
        };
        store.push(s);
    }
    let ret_type = fiddle_ffi_type(ret)
        .ok_or_else(|| fiddle_err(&format!("unknown return type code {ret}")))?;
    let cif = Cif::new(ffi_types, ret_type);
    let code_ptr = CodePtr(addr as *mut c_void);
    // Build the untyped Arg list, each borrowing its stable `store` slot.
    let ffi_args: Vec<Arg> = store
        .iter()
        .map(|s| match s {
            FiddleArg::I8(v) => ffi_arg(v),
            FiddleArg::I16(v) => ffi_arg(v),
            FiddleArg::I32(v) => ffi_arg(v),
            FiddleArg::I64(v) => ffi_arg(v),
            FiddleArg::F32(v) => ffi_arg(v),
            FiddleArg::F64(v) => ffi_arg(v),
            FiddleArg::Ptr(v) => ffi_arg(v),
        })
        .collect();

    let ret_unsigned = ret < 0;
    // SAFETY: a raw C call whose signature is only checked at runtime. If the
    // declared argument/return types do not match the real C function the
    // process can crash — that is Fiddle's documented low-level contract, and it
    // matches MRI. libffi widens integer returns to at least register width, so
    // reading a signed return through an 8-byte `i64` buffer is safe; float
    // returns come back at their own width. Every `Arg`'s backing storage
    // (`store`, and the `keep` char* buffers) is still live for the whole call.
    let result = unsafe {
        match ret.abs() {
            0 => {
                let _: i64 = cif.call(code_ptr, &ffi_args);
                Value::Undef
            }
            // TYPE_VOIDP result → a Fiddle::Pointer (matching MRI), readable via
            // #to_s / #to_str.
            1 => {
                let a: i64 = cif.call(code_ptr, &ffi_args);
                crate::host::fiddle_ptr_raw(a as u64, 0)
            }
            2..=6 => {
                let raw: i64 = cif.call(code_ptr, &ffi_args);
                if ret_unsigned {
                    let u = raw as u64;
                    if u <= i64::MAX as u64 {
                        Value::Int(u as i64)
                    } else {
                        with_host(|h| h.new_bigint(num_bigint::BigInt::from(u)))
                    }
                } else {
                    Value::Int(raw)
                }
            }
            7 => {
                let f: f32 = cif.call(code_ptr, &ffi_args);
                Value::Float(f as f64)
            }
            8 => {
                let f: f64 = cif.call(code_ptr, &ffi_args);
                Value::Float(f)
            }
            _ => return Err(fiddle_err(&format!("unsupported return type code {ret}"))),
        }
    };
    drop(keep);
    Ok(result)
}

/// Open a library ref for `Fiddle.dlopen` / `Fiddle::Handle.new`.
fn fiddle_dlopen_ref(path: Option<String>) -> Result<Value, String> {
    crate::host::fiddle_dlopen(path.as_deref()).map_err(|e| fiddle_err(&e))
}

/// `Fiddle::Function.new(addr, [arg_type_codes], ret_type_code)`. `addr` is an
/// Integer (typically `handle[sym]`) or a Fiddle::Pointer.
fn fiddle_function_new(args: &[Value]) -> Result<Value, String> {
    let addr = args
        .first()
        .map(|v| match crate::host::fiddle_ptr_parts(v) {
            Some((a, _)) => a,
            None => int_arg(v).unwrap_or_else(|| as_i(v)) as u64,
        })
        .unwrap_or(0);
    let argtypes: Vec<i32> = args
        .get(1)
        .and_then(|a| with_host(|h| h.as_array(a)))
        .unwrap_or_default()
        .iter()
        .map(|v| as_i(v) as i32)
        .collect();
    let ret = args.get(2).map(|v| as_i(v) as i32).unwrap_or(0);
    Ok(crate::host::fiddle_func_new(addr, argtypes, ret))
}

/// `Fiddle::Pointer[str]` / `Fiddle::Pointer.to_ptr(str)` — wrap a String's bytes
/// (NUL-terminated so `#to_s` reads a valid C string) in owned memory.
fn fiddle_ptr_from(args: &[Value]) -> Value {
    let s = args.first().map(arg_str).unwrap_or_default();
    let size = s.len() as i64;
    let mut buf = s.into_bytes();
    buf.push(0);
    crate::host::fiddle_alloc(buf, size)
}

/// Fiddle class-ref surface (`Fiddle.*`, `Fiddle::Handle.new`,
/// `Fiddle::Function.new`, `Fiddle::Pointer.*`). Returns `Ok(None)` for a class
/// or name it does not own, so `dispatch_classref` continues.
fn dispatch_fiddle_classref(
    cls: &str,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, String> {
    match cls {
        "Fiddle" => {
            if let Some(code) = fiddle_type_const(name) {
                return Ok(Some(Value::Int(code)));
            }
            match name {
                "dlopen" => {
                    let path = args.first().and_then(fiddle_path_arg);
                    return fiddle_dlopen_ref(path).map(Some);
                }
                "Handle" | "Function" | "Pointer" | "DLError" | "Error" => {
                    return Ok(Some(with_host(|h| h.class_ref(&format!("Fiddle::{name}")))));
                }
                _ => {}
            }
        }
        "Fiddle::Handle" if name == "new" => {
            let path = args.first().and_then(fiddle_path_arg);
            return fiddle_dlopen_ref(path).map(Some);
        }
        "Fiddle::Function" if name == "new" => {
            return fiddle_function_new(args).map(Some);
        }
        "Fiddle::Pointer" => match name {
            "[]" | "to_ptr" => return Ok(Some(fiddle_ptr_from(args))),
            "malloc" => {
                let n = args.first().map(as_i).unwrap_or(0).max(0) as usize;
                let mut buf = vec![0u8; n];
                buf.push(0); // NUL guard so #to_s on a zeroed buffer is ""
                return Ok(Some(crate::host::fiddle_alloc(buf, n as i64)));
            }
            "new" => {
                let addr = args.first().map(|v| as_i(v) as u64).unwrap_or(0);
                let size = args.get(1).map(as_i).unwrap_or(0);
                return Ok(Some(crate::host::fiddle_ptr_raw(addr, size)));
            }
            _ => {}
        },
        _ => {}
    }
    Ok(None)
}

/// A dlopen path argument: `nil` → `None` (the current process), else the string.
fn fiddle_path_arg(v: &Value) -> Option<String> {
    match v {
        Value::Undef => None,
        _ => Some(arg_str(v)),
    }
}

/// `Fiddle::Handle` instance methods: symbol resolution and `close`.
fn dispatch_fiddle_handle(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "[]" | "sym" => {
            let sym = args.first().map(arg_str).unwrap_or_default();
            let addr = crate::host::fiddle_sym(recv, &sym).map_err(|e| fiddle_err(&e))?;
            Ok(Value::Int(addr as i64))
        }
        "close" => {
            crate::host::fiddle_handle_close(recv);
            Ok(Value::Int(0))
        }
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        _ => Err(no_method_error(recv, name)),
    }
}

/// `Fiddle::Function` instance methods: `call` and the address readers.
fn dispatch_fiddle_function(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "call" => fiddle_call(recv, args),
        "to_i" | "to_int" => {
            let addr = crate::host::fiddle_func_parts(recv)
                .map(|(a, _, _)| a)
                .unwrap_or(0);
            Ok(Value::Int(addr as i64))
        }
        "inspect" | "to_s" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        _ => Err(no_method_error(recv, name)),
    }
}

/// `Fiddle::Pointer` instance methods: read the pointed-to memory back to Ruby,
/// plus `size`/`null?`/`to_i`/`[]`/`free`.
fn dispatch_fiddle_pointer(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let (addr, size) =
        crate::host::fiddle_ptr_parts(recv).ok_or_else(|| no_method_error(recv, name))?;
    match name {
        // `to_s` with a length reads exactly that many bytes; without one it
        // reads the sized buffer (up to NUL) or a NUL-terminated C string.
        "to_s" => match args.first() {
            Some(a) => Ok(new_str(crate::host::fiddle_read_bytes(
                addr,
                as_i(a).max(0) as usize,
            ))),
            None => Ok(new_str(crate::host::fiddle_read_cstr_or_len(addr, size))),
        },
        "to_str" => {
            let len = args
                .first()
                .map(|a| as_i(a).max(0) as usize)
                .unwrap_or(size.max(0) as usize);
            Ok(new_str(crate::host::fiddle_read_bytes(addr, len)))
        }
        "to_i" | "to_int" => Ok(Value::Int(addr as i64)),
        "null?" => Ok(Value::Bool(addr == 0)),
        "size" => Ok(Value::Int(size)),
        "free" => {
            crate::host::fiddle_free(recv);
            Ok(Value::Undef)
        }
        // `ptr[i]` → the unsigned byte at offset i; `ptr[i, len]` → a String.
        "[]" => {
            let i = args.first().map(as_i).unwrap_or(0).max(0) as u64;
            match args.get(1) {
                Some(l) => Ok(new_str(crate::host::fiddle_read_bytes(
                    addr.wrapping_add(i),
                    as_i(l).max(0) as usize,
                ))),
                None => {
                    // MRI returns a signed C `char`, and the byte must be read raw
                    // (the String path lossily re-encodes non-ASCII bytes).
                    let b = crate::host::fiddle_read_byte(addr.wrapping_add(i));
                    Ok(Value::Int(b as i8 as i64))
                }
            }
        }
        // `ptr[i] = int` writes one byte; `ptr[i] = str` copies the string's
        // bytes; `ptr[i, len] = str` copies `len` bytes. Writes are clamped to the
        // pointer's own buffer size so an owned `malloc` is never overrun.
        "[]=" => {
            let i = args.first().map(as_i).unwrap_or(0).max(0) as u64;
            let base = addr.wrapping_add(i);
            // Bytes still available in the buffer from offset `i` (unbounded when
            // the size is unknown, e.g. a pointer returned from C).
            let avail = if size > 0 {
                (size as u64).saturating_sub(i) as usize
            } else {
                usize::MAX
            };
            let value = args.last().unwrap();
            let bytes: Vec<u8> = match value {
                Value::Int(n) if args.len() == 2 => vec![*n as u8],
                _ => with_host(|h| h.as_str(value))
                    .unwrap_or_default()
                    .into_bytes(),
            };
            // With an explicit length (`ptr[i, len] = …`), write exactly that many.
            let want = if args.len() >= 3 {
                as_i(&args[1]).max(0) as usize
            } else {
                bytes.len()
            };
            let n = want.min(bytes.len()).min(avail);
            crate::host::fiddle_write_bytes(base, &bytes[..n]);
            Ok(value.clone())
        }
        "inspect" => Ok(new_str(with_host(|h| h.inspect(recv)))),
        _ => Err(no_method_error(recv, name)),
    }
}

/// `IO#puts` for one argument: arrays flatten (each element on its own line),
/// scalars get a trailing `\n` only if their string form lacks one.
fn io_puts_arg(recv: &Value, v: &Value) -> Result<(), String> {
    if let Some(arr) = with_host(|h| h.as_array(v)) {
        if arr.is_empty() {
            crate::host::io_write_str(recv, "\n").map_err(|e| io_err(&e))?;
        }
        for e in &arr {
            io_puts_arg(recv, e)?;
        }
    } else {
        let mut s = display(v);
        if !s.ends_with('\n') {
            s.push('\n');
        }
        crate::host::io_write_str(recv, &s).map_err(|e| io_err(&e))?;
    }
    Ok(())
}

/// Class methods on `Dir`. Returns `None` for names it doesn't own.
/// `ENV` — the process environment surfaced as a hash-like object over
/// `std::env`. Keys and values are strings; a `nil` value deletes.
fn dispatch_env(name: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let key = |i: usize| args.get(i).map(|a| with_host(|h| h.to_s(a)));
    match name {
        "[]" => {
            let k = str_arg(args, 0);
            Ok(std::env::var(&k).map(new_str).unwrap_or(Value::Undef))
        }
        "[]=" | "store" => {
            let k = str_arg(args, 0);
            match args.get(1) {
                Some(Value::Undef) | None => std::env::remove_var(&k),
                Some(v) => std::env::set_var(&k, with_host(|h| h.to_s(v))),
            }
            Ok(args.get(1).cloned().unwrap_or(Value::Undef))
        }
        "fetch" => {
            let k = str_arg(args, 0);
            match std::env::var(&k) {
                Ok(v) => Ok(new_str(v)),
                Err(_) => {
                    if let Some(bl) = &block {
                        call_proc(bl, &[new_str(k)])
                    } else if args.len() >= 2 {
                        Ok(args[1].clone())
                    } else {
                        Err(raise_exc("KeyError", &format!("key not found: {k:?}")))
                    }
                }
            }
        }
        "key?" | "has_key?" | "include?" | "member?" => {
            Ok(Value::Bool(std::env::var(str_arg(args, 0)).is_ok()))
        }
        "value?" | "has_value?" => {
            let target = key(0).unwrap_or_default();
            Ok(Value::Bool(std::env::vars().any(|(_, v)| v == target)))
        }
        "key" => {
            let target = key(0).unwrap_or_default();
            Ok(std::env::vars()
                .find(|(_, v)| *v == target)
                .map(|(k, _)| new_str(k))
                .unwrap_or(Value::Undef))
        }
        "delete" => {
            let k = str_arg(args, 0);
            let prev = std::env::var(&k).ok();
            std::env::remove_var(&k);
            Ok(prev.map(new_str).unwrap_or(Value::Undef))
        }
        "keys" => Ok(new_arr(std::env::vars().map(|(k, _)| new_str(k)).collect())),
        "values" => Ok(new_arr(std::env::vars().map(|(_, v)| new_str(v)).collect())),
        "to_h" | "to_hash" => {
            let map: IndexMap<RKey, Value> = std::env::vars()
                .map(|(k, v)| (RKey::Str(k), new_str(v)))
                .collect();
            Ok(with_host(|h| h.new_hash(map)))
        }
        "to_a" => Ok(new_arr(
            std::env::vars()
                .map(|(k, v)| new_arr(vec![new_str(k), new_str(v)]))
                .collect(),
        )),
        "size" | "length" => Ok(Value::Int(std::env::vars().count() as i64)),
        "empty?" => Ok(Value::Bool(std::env::vars().next().is_none())),
        "each" | "each_pair" => {
            if let Some(bl) = &block {
                for (k, v) in std::env::vars().collect::<Vec<_>>() {
                    call_proc(bl, &[new_str(k), new_str(v)])?;
                }
            }
            Ok(with_host(|h| h.class_ref("ENV")))
        }
        "each_key" => {
            if let Some(bl) = &block {
                for (k, _) in std::env::vars().collect::<Vec<_>>() {
                    call_proc(bl, &[new_str(k)])?;
                }
            }
            Ok(with_host(|h| h.class_ref("ENV")))
        }
        "inspect" | "to_s" => {
            let map: IndexMap<RKey, Value> = std::env::vars()
                .map(|(k, v)| (RKey::Str(k), new_str(v)))
                .collect();
            let h = with_host(|h| h.new_hash(map));
            Ok(new_str(with_host(|host| host.inspect(&h))))
        }
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for ENV"),
        )),
    }
}

fn dispatch_dir_class(
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Option<Result<Value, String>> {
    Some(match name {
        "pwd" | "getwd" => match std::env::current_dir() {
            Ok(p) => Ok(new_str(p.to_string_lossy().into_owned())),
            Err(e) => Err(raise_exc("SystemCallError", &e.to_string())),
        },
        "glob" | "[]" => {
            let pat = str_arg(args, 0);
            Ok(new_arr(dir_glob(&pat).into_iter().map(new_str).collect()))
        }
        "entries" => {
            let path = str_arg(args, 0);
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    let mut names: Vec<String> = vec![".".to_string(), "..".to_string()];
                    for e in rd.flatten() {
                        names.push(e.file_name().to_string_lossy().into_owned());
                    }
                    Ok(new_arr(names.into_iter().map(new_str).collect()))
                }
                Err(e) => Err(sys_err(
                    "No such file or directory @ dir_initialize",
                    &path,
                    &e,
                )),
            }
        }
        "exist?" | "exists?" => Ok(Value::Bool(
            std::path::Path::new(&str_arg(args, 0)).is_dir(),
        )),
        "mkdir" => {
            let path = str_arg(args, 0);
            match std::fs::create_dir(&path) {
                Ok(()) => Ok(Value::Int(0)),
                Err(e) => Err(sys_err("mkdir", &path, &e)),
            }
        }
        "rmdir" | "delete" | "unlink" => {
            let path = str_arg(args, 0);
            match std::fs::remove_dir(&path) {
                Ok(()) => Ok(Value::Int(0)),
                Err(e) => Err(sys_err("rmdir", &path, &e)),
            }
        }
        "chdir" => {
            let path = if args.is_empty() {
                std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
            } else {
                str_arg(args, 0)
            };
            if let Err(e) = std::env::set_current_dir(&path) {
                return Some(Err(sys_err("chdir", &path, &e)));
            }
            match &block {
                // Block form: run the block in the new cwd, restore afterwards,
                // return the block's value.
                Some(bl) => {
                    let prev = std::env::current_dir().ok();
                    let r = call_proc(bl, &[]);
                    if let Some(p) = prev {
                        let _ = std::env::set_current_dir(p);
                    }
                    r
                }
                None => Ok(Value::Int(0)),
            }
        }
        "home" => Ok(new_str(
            std::env::var("HOME").unwrap_or_else(|_| "/".to_string()),
        )),
        // `Dir.tmpdir` (from `require "tmpdir"`) — the system temp directory.
        "tmpdir" => Ok(new_str(std::env::temp_dir().to_string_lossy().into_owned())),
        // `Dir.mktmpdir([prefix][, tmpdir])` — create a fresh temp directory. With
        // a block, yield its path and remove it (recursively) afterwards, returning
        // the block value; without a block, return the created path.
        "mktmpdir" => {
            let prefix = match args.first() {
                Some(a) if !matches!(a, Value::Undef) => with_host(|h| h.to_s(a)),
                _ => "d".to_string(),
            };
            let base = std::env::temp_dir();
            let mut path = std::path::PathBuf::new();
            // Try a handful of randomized names to avoid collisions.
            let mut created = false;
            for _ in 0..64 {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                let n = std::process::id() ^ nanos.rotate_left(13) ^ (path.capacity() as u32);
                path = base.join(format!("{prefix}{n:08x}{nanos:08x}"));
                if std::fs::create_dir(&path).is_ok() {
                    created = true;
                    break;
                }
            }
            if !created {
                return Some(Err(raise_exc(
                    "SystemCallError",
                    "could not create temp directory",
                )));
            }
            let p = path.to_string_lossy().into_owned();
            match &block {
                Some(bl) => {
                    let r = call_proc(bl, &[new_str(p.clone())]);
                    let _ = std::fs::remove_dir_all(&path);
                    r
                }
                None => Ok(new_str(p)),
            }
        }
        _ => return None,
    })
}

/// `Dir.glob` matching. Each `{a,b}` brace alternative is globbed separately,
/// its matches sorted lexicographically (MRI's default since 3.0), and the
/// alternatives concatenated in brace order (MRI does not re-sort across the
/// whole set) with duplicates dropped, keeping first occurrence. Leading-dot
/// files are excluded from `*`/`?`/`[..]` (MRI default). The `glob` crate has no
/// brace support, so `brace_expand` handles it. Paths stay relative when the
/// pattern is relative.
fn dir_glob(pattern: &str) -> Vec<String> {
    use glob::{glob_with, MatchOptions};
    let opts = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let mut out: Vec<String> = Vec::new();
    for pat in brace_expand(pattern) {
        let mut group: Vec<String> = Vec::new();
        if let Ok(paths) = glob_with(&pat, opts) {
            for entry in paths.flatten() {
                group.push(entry.to_string_lossy().into_owned());
            }
        }
        group.sort();
        for g in group {
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    out
}

/// Expand one level of `{a,b,c}` brace alternation into concrete patterns
/// (`x.{rb,txt}` → `["x.rb", "x.txt"]`). No braces → the pattern unchanged.
fn brace_expand(pattern: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (pattern.find('{'), pattern.find('}')) else {
        return vec![pattern.to_string()];
    };
    if close < open {
        return vec![pattern.to_string()];
    }
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let mut out = Vec::new();
    for alt in pattern[open + 1..close].split(',') {
        // Recurse so multiple brace groups all expand.
        for tail in brace_expand(&format!("{prefix}{alt}{suffix}")) {
            out.push(tail);
        }
    }
    out
}

// ---- pure path helpers (File.basename/dirname/extname/join/expand_path) ----

/// The last path component of `path`, trailing slashes stripped (`"/"` stays
/// `"/"`). Empty string for an empty path.
fn path_last_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        // All slashes (or empty): the root's basename is "/", empty stays empty.
        return if path.is_empty() { "" } else { "/" };
    }
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

/// `File.extname` — the extension including the leading dot, MRI semantics: a
/// leading-dot name (`.bashrc`) has no extension; a trailing dot yields `"."`;
/// an all-dots component yields `""`.
fn path_extname(path: &str) -> String {
    let base = path_last_component(path);
    // Index of the first non-dot byte; if none, the component is all dots.
    let start = match base.bytes().position(|b| b != b'.') {
        Some(i) => i,
        None => return String::new(),
    };
    // The last dot at or after `start` is the extension boundary.
    match base[start..].rfind('.') {
        Some(rel) => base[start + rel..].to_string(),
        None => String::new(),
    }
}

/// `File.basename(path[, suffix])`. `suffix == ".*"` strips whatever extension
/// `extname` finds; any other suffix is stripped only when it is a trailing
/// match that leaves a non-empty stem.
fn path_basename(path: &str, suffix: Option<&str>) -> String {
    let base = path_last_component(path);
    match suffix {
        None => base.to_string(),
        Some(".*") => {
            let ext = path_extname(base);
            if ext.is_empty() {
                base.to_string()
            } else {
                base[..base.len() - ext.len()].to_string()
            }
        }
        Some(sfx) => {
            if base != sfx && base.ends_with(sfx) {
                base[..base.len() - sfx.len()].to_string()
            } else {
                base.to_string()
            }
        }
    }
}

/// `File.dirname` — everything before the last component. No slash → `"."`;
/// the root → `"/"`.
fn path_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() {
            ".".to_string()
        } else {
            "/".to_string()
        };
    }
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
        None => ".".to_string(),
    }
}

/// `File.join(*parts)` — join with `/`, collapsing so exactly one slash sits
/// between adjacent parts. No parts → `""`.
fn path_join(parts: &[Value]) -> String {
    let strs: Vec<String> = parts
        .iter()
        .map(|p| with_host(|h| h.as_str(p)).unwrap_or_default())
        .collect();
    let mut it = strs.into_iter();
    let mut s = match it.next() {
        Some(first) => first,
        None => return String::new(),
    };
    for p in it {
        let left = s.ends_with('/');
        let right = p.starts_with('/');
        match (left, right) {
            (true, true) => s.push_str(&p[1..]),
            (false, false) => {
                s.push('/');
                s.push_str(&p);
            }
            _ => s.push_str(&p),
        }
    }
    s
}

/// `File.expand_path(path[, base])` — purely lexical (no filesystem access):
/// expands a leading `~`, resolves against `base` (default cwd), and collapses
/// `.`/`..`. Absolute result, no trailing slash except the root.
fn path_expand(path: &str, base: Option<&str>) -> String {
    let expanded = expand_tilde(path);
    let combined = if expanded.starts_with('/') {
        expanded
    } else {
        // Resolve `base` (itself possibly relative or `~`) against the cwd first.
        let base_raw = base.map(|b| b.to_string()).unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".to_string())
        });
        let base_abs = if base_raw.is_empty() {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".to_string())
        } else {
            path_expand(&base_raw, None)
        };
        if expanded.is_empty() {
            base_abs
        } else {
            format!("{base_abs}/{expanded}")
        }
    };
    normalize_abs(&combined)
}

/// Expand a leading `~` (`~` → `$HOME`, `~/x` → `$HOME/x`). Other forms (`~user`)
/// are left untouched.
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        std::env::var("HOME").unwrap_or_else(|_| "~".to_string())
    } else if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
        format!("{home}/{rest}")
    } else {
        path.to_string()
    }
}

/// Collapse `.`/`..`/empty segments of an absolute path into a canonical form.
fn normalize_abs(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", stack.join("/"))
    }
}

/// A block-based generator (`Enumerator.new { |y| ... }`). Terminal operations
/// re-drive the block; `next`/`peek` materialize it to completion once.
fn dispatch_generator(
    recv: &Value,
    gblock: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        "first" => {
            let n = args.first().map(|v| as_i(v).max(0) as usize);
            let out = drive_generator(gblock, n.unwrap_or(1))?;
            match n {
                Some(_) => Ok(new_arr(out)),
                None => Ok(out.into_iter().next().unwrap_or(Value::Undef)),
            }
        }
        "take" => {
            let n = args.first().map(|v| as_i(v).max(0) as usize).unwrap_or(0);
            Ok(new_arr(drive_generator(gblock, n)?))
        }
        "to_a" | "force" | "entries" => Ok(new_arr(drive_generator(gblock, usize::MAX)?)),
        "each" if block.is_some() => {
            let bl = block.unwrap();
            for v in drive_generator(gblock, usize::MAX)? {
                call_proc(&bl, std::slice::from_ref(&v))?;
            }
            Ok(recv.clone())
        }
        // `.lazy` keeps the generator as the pipeline source (see `lazy_pull`).
        "lazy" => Ok(with_host(|h| h.new_lazy(recv.clone(), vec![]))),
        // External iteration: materialize to completion on first use, then
        // cursor. An infinite generator here runs forever — there are no fibers.
        "next" | "peek" if args.is_empty() && block.is_none() => {
            let need = with_host(|h| h.generator_unmaterialized(recv));
            if need {
                // A `cycle` generator is infinite: materialize just one cycle
                // (its element count) and let `generator_next` wrap over it. Any
                // other generator drives to completion (an infinite one hangs
                // here, exactly as in MRI without fibers).
                let limit = with_host(|h| h.cycle_proc_len(gblock)).unwrap_or(usize::MAX);
                let buf = drive_generator(gblock, limit)?;
                with_host(|h| h.set_generator_materialized(recv, buf));
            }
            match with_host(|h| h.generator_next(recv, name == "next")) {
                Some(v) => Ok(v),
                None => Err(raise_exc("StopIteration", "iteration reached an end")),
            }
        }
        "rewind" if args.is_empty() => {
            with_host(|h| h.generator_rewind(recv));
            Ok(recv.clone())
        }
        // MRI returns nil for a generator's size (unknown without running it).
        "size" => Ok(Value::Undef),
        // Everything else (`map`, `select`, `reduce`, …): materialize fully and
        // delegate to Array. Faithful for finite generators; an infinite one
        // hangs here exactly as it does in MRI.
        _ => {
            let buf = drive_generator(gblock, usize::MAX)?;
            dispatch_array(&new_arr(buf), name, args, block)
        }
    }
}

const WDAY_FULL: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MON_FULL: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Left-pad an integer to `width` using `pad`, honoring an optional strftime
/// flag: `-` suppresses padding, `_` forces space, `0` forces zero.
fn strf_num(n: i64, width: usize, pad: char, flag: Option<char>) -> String {
    let (width, pad) = match flag {
        Some('-') => (0, ' '),
        Some('_') => (width, ' '),
        Some('0') => (width, '0'),
        _ => (width, pad),
    };
    let body = n.abs().to_string();
    let sign = if n < 0 { "-" } else { "" };
    let need = width.saturating_sub(body.len() + sign.len());
    format!("{sign}{}{body}", pad.to_string().repeat(need))
}

/// `Time#strftime` over the common directive set, including the `-`/`_`/`0`
/// padding flags (`%-d` = no pad, `%_d` = space, `%0e` = zero). Unrecognized
/// directives are emitted verbatim (leading `%` kept), matching MRI's lenient
/// behavior.
fn time_strftime(
    fmt: &str,
    f: (i64, i64, i64, i64, i64, i64, i64, i64, f64),
    epoch: f64,
) -> String {
    let (y, mo, d, hh, mi, ss, wday, yday, frac) = f;
    let hour12 = if hh % 12 == 0 { 12 } else { hh % 12 };
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // An optional padding flag between `%` and the directive letter.
        let flag = match chars.peek() {
            Some(&f @ ('-' | '_' | '0')) => {
                chars.next();
                Some(f)
            }
            _ => None,
        };
        match chars.next() {
            Some('Y') => out.push_str(&strf_num(y, 4, '0', flag)),
            Some('C') => out.push_str(&strf_num(y / 100, 2, '0', flag)),
            Some('y') => out.push_str(&strf_num(y.rem_euclid(100), 2, '0', flag)),
            Some('m') => out.push_str(&strf_num(mo, 2, '0', flag)),
            Some('d') => out.push_str(&strf_num(d, 2, '0', flag)),
            Some('e') => out.push_str(&strf_num(d, 2, ' ', flag)),
            Some('H') => out.push_str(&strf_num(hh, 2, '0', flag)),
            Some('k') => out.push_str(&strf_num(hh, 2, ' ', flag)),
            Some('I') => out.push_str(&strf_num(hour12, 2, '0', flag)),
            Some('l') => out.push_str(&strf_num(hour12, 2, ' ', flag)),
            Some('M') => out.push_str(&strf_num(mi, 2, '0', flag)),
            Some('S') => out.push_str(&strf_num(ss, 2, '0', flag)),
            Some('L') => out.push_str(&strf_num((frac * 1000.0).round() as i64, 3, '0', flag)),
            Some('j') => out.push_str(&strf_num(yday, 3, '0', flag)),
            Some('p') => out.push_str(if hh < 12 { "AM" } else { "PM" }),
            Some('P') => out.push_str(if hh < 12 { "am" } else { "pm" }),
            Some('A') => out.push_str(WDAY_FULL[wday as usize]),
            Some('a') => out.push_str(&WDAY_FULL[wday as usize][..3]),
            Some('B') => out.push_str(MON_FULL[(mo - 1) as usize]),
            Some('b') | Some('h') => out.push_str(&MON_FULL[(mo - 1) as usize][..3]),
            Some('w') => out.push_str(&wday.to_string()),
            Some('u') => out.push_str(&(if wday == 0 { 7 } else { wday }).to_string()),
            Some('s') => out.push_str(&(epoch.floor() as i64).to_string()),
            Some('z') => out.push_str("+0000"),
            Some('Z') => out.push_str("UTC"),
            Some('F') => out.push_str(&format!("{y:04}-{mo:02}-{d:02}")),
            Some('T') | Some('X') => out.push_str(&format!("{hh:02}:{mi:02}:{ss:02}")),
            Some('R') => out.push_str(&format!("{hh:02}:{mi:02}")),
            Some('D') => out.push_str(&format!("{:02}/{d:02}/{:02}", mo, y.rem_euclid(100))),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                if let Some(fl) = flag {
                    out.push(fl);
                }
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// `Time` instance methods (all UTC). Field readers come from `time_fields`;
/// `strftime` formats; comparison/arithmetic operators also route here (with
/// the native `+`/`-`/`<`/`>` forms handled in `Host::num_op`).
fn dispatch_time(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let secs = with_host(|h| h.time_secs(recv).unwrap_or(0.0));
    let f = with_host(|h| h.time_fields(secs));
    let (y, mo, d, hh, mi, ss, wday, yday, _frac) = f;
    match name {
        "year" => Ok(Value::Int(y)),
        "month" | "mon" => Ok(Value::Int(mo)),
        "day" | "mday" => Ok(Value::Int(d)),
        "hour" => Ok(Value::Int(hh)),
        "min" => Ok(Value::Int(mi)),
        "sec" => Ok(Value::Int(ss)),
        "wday" => Ok(Value::Int(wday)),
        "yday" => Ok(Value::Int(yday)),
        "to_i" | "tv_sec" => Ok(Value::Int(secs.floor() as i64)),
        "to_f" => Ok(Value::Float(secs)),
        "sunday?" => Ok(Value::Bool(wday == 0)),
        "monday?" => Ok(Value::Bool(wday == 1)),
        "tuesday?" => Ok(Value::Bool(wday == 2)),
        "wednesday?" => Ok(Value::Bool(wday == 3)),
        "thursday?" => Ok(Value::Bool(wday == 4)),
        "friday?" => Ok(Value::Bool(wday == 5)),
        "saturday?" => Ok(Value::Bool(wday == 6)),
        // UTC-only model: these conversions are all no-ops that return an
        // equivalent Time (or the flag). Local-timezone offset is not modeled.
        "utc" | "getutc" | "gmtime" | "localtime" | "getlocal" => {
            Ok(with_host(|h| h.new_time(secs)))
        }
        "utc?" | "gmt?" => Ok(Value::Bool(true)),
        "to_s" | "inspect" => Ok(with_host(|h| {
            let s = h.time_to_s(secs, name == "inspect");
            h.new_string(s)
        })),
        "strftime" => {
            let fmt = with_host(|h| h.as_str(&args[0]).unwrap_or_default());
            Ok(with_host(|h| {
                let s = time_strftime(&fmt, f, secs);
                h.new_string(s)
            }))
        }
        "<=>" => {
            if let Some(other) = with_host(|h| h.time_secs(&args[0])) {
                Ok(Value::Int(match secs.total_cmp(&other) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            } else {
                Ok(Value::Undef)
            }
        }
        "==" => Ok(Value::Bool(
            with_host(|h| h.time_secs(&args[0])) == Some(secs),
        )),
        "+" => Ok(with_host(|h| h.new_time(secs + as_f(&args[0])))),
        "-" => {
            if let Some(other) = with_host(|h| h.time_secs(&args[0])) {
                Ok(Value::Float(secs - other))
            } else {
                Ok(with_host(|h| h.new_time(secs - as_f(&args[0]))))
            }
        }
        "hash" => Ok(Value::Int(secs.to_bits() as i64)),
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for an instance of Time"),
        )),
    }
}

/// Parse an ISO-8601-style `YYYY-MM-DD` (or `YYYY/MM/DD`) date into a day count
/// since the Unix epoch. Ruby's `Date.parse` is far more lenient; this covers
/// the common machine-readable forms and rejects the rest.
fn parse_iso_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let parts: Vec<&str> = s.splitn(3, ['-', '/']).collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i64 = parts[0].parse().ok()?;
    let m: i64 = parts[1].parse().ok()?;
    let d: i64 = parts[2].get(..2).unwrap_or(parts[2]).parse().ok()?;
    if !(1..=12).contains(&m) || d < 1 || d > crate::host::days_in_month(y, m) {
        return None;
    }
    Some(crate::host::days_from_civil(y, m, d))
}

/// Shift a day count by `delta` calendar months, clamping the day to the last
/// valid day of the target month (`Jan 31 >> 1` → `Feb 28`/`29`), matching
/// `Date#>>` / `#next_month`.
fn date_add_months(days: i64, delta: i64) -> i64 {
    let (y, m, d) = crate::host::civil_from_days(days);
    let total = (y * 12 + (m - 1)) + delta;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    let nd = d.min(crate::host::days_in_month(ny, nm));
    crate::host::days_from_civil(ny, nm, nd)
}

/// `Date` instance methods (proleptic Gregorian, no time-of-day). Field readers
/// reuse `time_fields`; arithmetic operators also route here (with the native
/// `+`/`-`/`<`/`>` forms handled in `Host::num_op`).
fn dispatch_date(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let days = with_host(|h| h.date_days(recv).unwrap_or(0));
    let (y, mo, d) = crate::host::civil_from_days(days);
    // Reuse the UTC breakdown for wday/yday (time-of-day is always zero).
    let (_, _, _, _, _, _, wday, yday, _) = with_host(|h| h.time_fields(days as f64 * 86_400.0));
    let arg_days = |i: usize, dflt: i64| args.get(i).map(as_i).unwrap_or(dflt);
    match name {
        "year" => Ok(Value::Int(y)),
        "month" | "mon" => Ok(Value::Int(mo)),
        "day" | "mday" => Ok(Value::Int(d)),
        "wday" => Ok(Value::Int(wday)),
        "yday" => Ok(Value::Int(yday)),
        // ISO weekday: Monday=1..Sunday=7.
        "cwday" => Ok(Value::Int(if wday == 0 { 7 } else { wday })),
        "jd" => Ok(Value::Int(days + crate::host::UNIX_EPOCH_JDN)),
        "leap?" => Ok(Value::Bool(crate::host::is_leap_year(y))),
        "sunday?" => Ok(Value::Bool(wday == 0)),
        "monday?" => Ok(Value::Bool(wday == 1)),
        "tuesday?" => Ok(Value::Bool(wday == 2)),
        "wednesday?" => Ok(Value::Bool(wday == 3)),
        "thursday?" => Ok(Value::Bool(wday == 4)),
        "friday?" => Ok(Value::Bool(wday == 5)),
        "saturday?" => Ok(Value::Bool(wday == 6)),
        "to_s" | "iso8601" => Ok(with_host(|h| {
            let s = h.date_to_s(days);
            h.new_string(s)
        })),
        "inspect" => Ok(with_host(|h| {
            let s = h.date_inspect(days);
            h.new_string(s)
        })),
        "strftime" => {
            let fmt = with_host(|h| h.as_str(&args[0]).unwrap_or_default());
            let f = with_host(|h| h.time_fields(days as f64 * 86_400.0));
            Ok(with_host(|h| {
                let s = time_strftime(&fmt, f, days as f64 * 86_400.0);
                h.new_string(s)
            }))
        }
        "next_day" | "succ" => Ok(with_host(|h| h.new_date(days + arg_days(0, 1)))),
        "prev_day" => Ok(with_host(|h| h.new_date(days - arg_days(0, 1)))),
        "next_month" => Ok(with_host(|h| {
            h.new_date(date_add_months(days, arg_days(0, 1)))
        })),
        "prev_month" => Ok(with_host(|h| {
            h.new_date(date_add_months(days, -arg_days(0, 1)))
        })),
        ">>" => Ok(with_host(|h| {
            h.new_date(date_add_months(days, arg_days(0, 1)))
        })),
        "<<" => Ok(with_host(|h| {
            h.new_date(date_add_months(days, -arg_days(0, 1)))
        })),
        "next_year" => Ok(with_host(|h| {
            h.new_date(date_add_months(days, 12 * arg_days(0, 1)))
        })),
        "prev_year" => Ok(with_host(|h| {
            h.new_date(date_add_months(days, -12 * arg_days(0, 1)))
        })),
        "+" => Ok(with_host(|h| h.new_date(days + as_i(&args[0])))),
        "-" => {
            if let Some(other) = with_host(|h| h.date_days(&args[0])) {
                Ok(with_host(|h| {
                    let r = num_rational::BigRational::from(num_bigint::BigInt::from(days - other));
                    h.new_rational(r)
                }))
            } else {
                Ok(with_host(|h| h.new_date(days - as_i(&args[0]))))
            }
        }
        "<=>" => {
            if let Some(other) = with_host(|h| h.date_days(&args[0])) {
                Ok(Value::Int((days.cmp(&other) as i64).signum()))
            } else {
                Ok(Value::Undef)
            }
        }
        "==" => Ok(Value::Bool(
            with_host(|h| h.date_days(&args[0])) == Some(days),
        )),
        "hash" => Ok(Value::Int(days)),
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for an instance of Date"),
        )),
    }
}

/// Parse an ISO8601 `YYYY-MM-DDTHH:MM:SS` (date, optional `T`/space + time,
/// optional trailing `Z`/`+hh:mm` zone which is ignored — UTC-only) into epoch
/// seconds. Reuses `parse_iso_date` for the date half.
fn parse_iso_datetime(s: &str) -> Option<f64> {
    let s = s.trim();
    let (dpart, tpart) = match s.split_once(['T', ' ']) {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let days = parse_iso_date(dpart)?;
    let mut secs = days as f64 * 86_400.0;
    if let Some(t) = tpart {
        // Drop a trailing zone designator; UTC-only, so the offset is ignored.
        let t = t.split(['Z', '+']).next().unwrap_or(t);
        let comps: Vec<&str> = t.splitn(3, ':').collect();
        let hh: i64 = comps.first()?.trim().parse().ok()?;
        let mi: i64 = comps
            .get(1)
            .and_then(|x| x.trim().parse().ok())
            .unwrap_or(0);
        let ss: f64 = comps
            .get(2)
            .and_then(|x| x.trim().parse().ok())
            .unwrap_or(0.0);
        secs += (hh * 3600 + mi * 60) as f64 + ss;
    }
    Some(secs)
}

/// `DateTime` instance methods (UTC-only, proleptic Gregorian). Field readers
/// and `strftime` reuse `time_fields`/`time_strftime`; arithmetic is by day
/// (like `Date`), keeping the time of day.
fn dispatch_datetime(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let secs = with_host(|h| h.datetime_secs(recv).unwrap_or(0.0));
    let f = with_host(|h| h.time_fields(secs));
    let (y, mo, d, hh, mi, ss, wday, yday, _frac) = f;
    let day = (secs / 86_400.0).floor() as i64;
    let tod = secs - day as f64 * 86_400.0;
    let arg_i = |i: usize, dflt: i64| args.get(i).map(as_i).unwrap_or(dflt);
    match name {
        "year" => Ok(Value::Int(y)),
        "month" | "mon" => Ok(Value::Int(mo)),
        "day" | "mday" => Ok(Value::Int(d)),
        "hour" => Ok(Value::Int(hh)),
        "min" => Ok(Value::Int(mi)),
        "sec" => Ok(Value::Int(ss)),
        "wday" => Ok(Value::Int(wday)),
        "yday" => Ok(Value::Int(yday)),
        "cwday" => Ok(Value::Int(if wday == 0 { 7 } else { wday })),
        "jd" => Ok(Value::Int(day + crate::host::UNIX_EPOCH_JDN)),
        "leap?" => Ok(Value::Bool(crate::host::is_leap_year(y))),
        "sunday?" => Ok(Value::Bool(wday == 0)),
        "monday?" => Ok(Value::Bool(wday == 1)),
        "tuesday?" => Ok(Value::Bool(wday == 2)),
        "wednesday?" => Ok(Value::Bool(wday == 3)),
        "thursday?" => Ok(Value::Bool(wday == 4)),
        "friday?" => Ok(Value::Bool(wday == 5)),
        "saturday?" => Ok(Value::Bool(wday == 6)),
        "to_s" | "iso8601" => Ok(with_host(|h| {
            let s = h.datetime_to_s(secs);
            h.new_string(s)
        })),
        "inspect" => Ok(with_host(|h| {
            let s = h.datetime_inspect(secs);
            h.new_string(s)
        })),
        "strftime" => {
            let fmt = with_host(|h| h.as_str(&args[0]).unwrap_or_default());
            Ok(with_host(|h| {
                let s = time_strftime(&fmt, f, secs);
                h.new_string(s)
            }))
        }
        "to_date" => Ok(with_host(|h| h.new_date(day))),
        "to_time" => Ok(with_host(|h| h.new_time(secs))),
        "next_day" | "succ" => Ok(with_host(|h| {
            h.new_datetime(secs + arg_i(0, 1) as f64 * 86_400.0)
        })),
        "prev_day" => Ok(with_host(|h| {
            h.new_datetime(secs - arg_i(0, 1) as f64 * 86_400.0)
        })),
        "next_month" | ">>" => Ok(with_host(|h| {
            h.new_datetime(date_add_months(day, arg_i(0, 1)) as f64 * 86_400.0 + tod)
        })),
        "prev_month" | "<<" => Ok(with_host(|h| {
            h.new_datetime(date_add_months(day, -arg_i(0, 1)) as f64 * 86_400.0 + tod)
        })),
        "next_year" => Ok(with_host(|h| {
            h.new_datetime(date_add_months(day, 12 * arg_i(0, 1)) as f64 * 86_400.0 + tod)
        })),
        "prev_year" => Ok(with_host(|h| {
            h.new_datetime(date_add_months(day, -12 * arg_i(0, 1)) as f64 * 86_400.0 + tod)
        })),
        "+" => Ok(with_host(|h| {
            h.new_datetime(secs + as_f(&args[0]) * 86_400.0)
        })),
        "-" => {
            if let Some(other) = with_host(|h| h.datetime_secs(&args[0])) {
                Ok(with_host(|h| {
                    let numer = num_bigint::BigInt::from((secs - other).round() as i64);
                    let r = num_rational::BigRational::new(numer, num_bigint::BigInt::from(86_400));
                    h.new_rational(r)
                }))
            } else {
                Ok(with_host(|h| {
                    h.new_datetime(secs - as_f(&args[0]) * 86_400.0)
                }))
            }
        }
        "<=>" => {
            if let Some(other) = with_host(|h| h.datetime_secs(&args[0])) {
                Ok(Value::Int(match secs.total_cmp(&other) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            } else {
                Ok(Value::Undef)
            }
        }
        "==" => Ok(Value::Bool(
            with_host(|h| h.datetime_secs(&args[0])) == Some(secs),
        )),
        "hash" => Ok(Value::Int(secs.to_bits() as i64)),
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for an instance of DateTime"),
        )),
    }
}

fn dispatch_set(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let items = with_host(|h| h.as_set(recv).unwrap_or_default());
    // The other operand's elements, for set-algebra methods.
    let other = || -> Vec<Value> {
        args.first()
            .and_then(|v| with_host(|h| h.as_set(v)).or_else(|| with_host(|h| h.as_array(v))))
            .unwrap_or_default()
    };
    match name {
        "add" | "<<" => {
            with_host(|h| h.set_add(recv, args[0].clone()));
            Ok(recv.clone())
        }
        "add?" => {
            let added = with_host(|h| h.set_add(recv, args[0].clone()));
            Ok(if added { recv.clone() } else { Value::Undef })
        }
        "delete" => {
            with_host(|h| h.set_remove(recv, &args[0]));
            Ok(recv.clone())
        }
        "delete?" => {
            let removed = with_host(|h| h.set_remove(recv, &args[0]));
            Ok(if removed { recv.clone() } else { Value::Undef })
        }
        "include?" | "member?" | "===" | "contain?" => {
            Ok(Value::Bool(with_host(|h| h.set_contains(recv, &args[0]))))
        }
        "size" | "length" | "count" if args.is_empty() && block.is_none() => {
            Ok(Value::Int(items.len() as i64))
        }
        "empty?" => Ok(Value::Bool(items.is_empty())),
        "clear" => {
            for v in &items {
                with_host(|h| h.set_remove(recv, v));
            }
            Ok(recv.clone())
        }
        "to_a" | "to_ary" => Ok(new_arr(items)),
        "to_set" | "dup" | "clone" => Ok(with_host(|h| h.new_set(items))),
        "merge" => {
            for v in other() {
                with_host(|h| h.set_add(recv, v));
            }
            Ok(recv.clone())
        }
        // Set algebra — build a fresh Set.
        "union" | "|" | "+" | "merge_new" => {
            let mut all = items.clone();
            all.extend(other());
            Ok(with_host(|h| h.new_set(all)))
        }
        "intersection" | "&" => {
            let o = other();
            let keep: Vec<Value> = items
                .iter()
                .filter(|v| o.iter().any(|w| with_host(|h| h.eq_values(v, w))))
                .cloned()
                .collect();
            Ok(with_host(|h| h.new_set(keep)))
        }
        "difference" | "-" => {
            let o = other();
            let keep: Vec<Value> = items
                .iter()
                .filter(|v| !o.iter().any(|w| with_host(|h| h.eq_values(v, w))))
                .cloned()
                .collect();
            Ok(with_host(|h| h.new_set(keep)))
        }
        "^" => {
            // Symmetric difference: elements in exactly one of the two sets.
            let o = other();
            let mut out: Vec<Value> = items
                .iter()
                .filter(|v| !o.iter().any(|w| with_host(|h| h.eq_values(v, w))))
                .cloned()
                .collect();
            out.extend(
                o.iter()
                    .filter(|w| !items.iter().any(|v| with_host(|h| h.eq_values(v, w))))
                    .cloned(),
            );
            Ok(with_host(|h| h.new_set(out)))
        }
        "subset?" | "<=" => {
            let o = other();
            Ok(Value::Bool(items.iter().all(|v| {
                o.iter().any(|w| with_host(|h| h.eq_values(v, w)))
            })))
        }
        "superset?" | ">=" => {
            let o = other();
            Ok(Value::Bool(o.iter().all(|w| {
                items.iter().any(|v| with_host(|h| h.eq_values(v, w)))
            })))
        }
        "proper_subset?" | "<" => {
            let o = other();
            let subset = items
                .iter()
                .all(|v| o.iter().any(|w| with_host(|h| h.eq_values(v, w))));
            Ok(Value::Bool(subset && items.len() < o.len()))
        }
        "proper_superset?" | ">" => {
            let o = other();
            let superset = o
                .iter()
                .all(|w| items.iter().any(|v| with_host(|h| h.eq_values(v, w))));
            Ok(Value::Bool(superset && items.len() > o.len()))
        }
        "disjoint?" => {
            let o = other();
            Ok(Value::Bool(!items.iter().any(|v| {
                o.iter().any(|w| with_host(|h| h.eq_values(v, w)))
            })))
        }
        "intersect?" => {
            let o = other();
            Ok(Value::Bool(items.iter().any(|v| {
                o.iter().any(|w| with_host(|h| h.eq_values(v, w)))
            })))
        }
        "each" if block.is_some() => {
            let bl = block.unwrap();
            for v in &items {
                call_proc(&bl, std::slice::from_ref(v))?;
                if has_pending_signal() {
                    if let Some(bv) = take_break() {
                        return Ok(bv);
                    }
                    break;
                }
            }
            Ok(recv.clone())
        }
        // Enumerable methods (map/select/reduce/sort/…) run over the element
        // Array. `map`/`select` etc. return an Array, matching Ruby's Set.
        _ => remap_array_delegate(
            dispatch_array(&new_arr(items), name, args, block),
            recv,
            name,
        ),
    }
}

fn dispatch_hash(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    frozen_guard(recv, name, HASH_MUTATORS)?;
    let map = with_host(|h| h.as_hash(recv).unwrap_or_default());
    match name {
        "size" | "length" => Ok(Value::Int(map.len() as i64)),
        "count" => {
            if let Some(b) = &block {
                let mut n = 0i64;
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    if with_host(|h| h.truthy(&r)) {
                        n += 1;
                    }
                }
                Ok(Value::Int(n))
            } else {
                Ok(Value::Int(map.len() as i64))
            }
        }
        "empty?" => Ok(Value::Bool(map.is_empty())),
        // Pattern-match protocol: return self (the requested-keys arg is only a
        // hint; the pattern re-checks each key). Matches MRI's `Hash#deconstruct_keys`.
        "deconstruct_keys" => Ok(recv.clone()),
        "keys" => Ok(with_host(|h| {
            let ks: Vec<Value> = map.keys().map(|k| h.key_value(k)).collect();
            h.new_array(ks)
        })),
        "values" => Ok(new_arr(map.values().cloned().collect())),
        "key?" | "has_key?" | "include?" | "member?" => {
            let k = with_host(|h| h.value_to_key(&args[0]));
            Ok(Value::Bool(map.contains_key(&k)))
        }
        "value?" | "has_value?" => Ok(Value::Bool(
            map.values()
                .any(|v| with_host(|h| h.eq_values(v, &args[0]))),
        )),
        "[]" => {
            let k = with_host(|h| h.value_to_key(&args[0]));
            // A missing key calls the default block (`Hash.new { |h,k| ... }`,
            // which may itself mutate the hash), else yields the plain default
            // (nil unless `Hash.new(d)`).
            match map.get(&k).cloned() {
                Some(v) => Ok(v),
                None => match with_host(|h| h.hash_default_proc(recv)) {
                    Some(bl) => call_proc(&bl, &[recv.clone(), args[0].clone()]),
                    None => Ok(with_host(|h| h.hash_default(recv))),
                },
            }
        }
        "fetch" => {
            let k = with_host(|h| h.value_to_key(&args[0]));
            if let Some(v) = map.get(&k) {
                Ok(v.clone())
            } else if let Some(b) = &block {
                // fetch(key) { |key| ... } — block is called with the missing key.
                call_proc(b, std::slice::from_ref(&args[0]))
            } else if let Some(d) = args.get(1) {
                Ok(d.clone())
            } else {
                let ins = with_host(|h| h.inspect(&args[0]));
                Err(raise_exc("KeyError", &format!("key not found: {ins}")))
            }
        }
        "[]=" | "store" => {
            let mut m = map;
            let k = with_host(|h| h.value_to_key(&args[0]));
            m.insert(k, args[1].clone());
            with_host(|h| h.set_hash(recv, m));
            Ok(args[1].clone())
        }
        "delete" => {
            let mut m = map;
            let k = with_host(|h| h.value_to_key(&args[0]));
            let v = m.shift_remove(&k).unwrap_or(Value::Undef);
            with_host(|h| h.set_hash(recv, m));
            Ok(v)
        }
        "merge" => {
            // `merge` with no argument returns a copy; each hash argument is
            // merged in left-to-right. With a block, key collisions are resolved
            // by `block.call(key, old_value, new_value)`.
            let mut m = map;
            for a in args {
                if let Some(other) = with_host(|h| h.as_hash(a)) {
                    if let Some(b) = &block {
                        for (k, v) in other {
                            if let Some(old) = m.get(&k) {
                                let kv = with_host(|h| h.key_value(&k));
                                let merged = call_proc(b, &[kv, old.clone(), v.clone()])?;
                                m.insert(k, merged);
                            } else {
                                m.insert(k, v);
                            }
                        }
                    } else {
                        m.extend(other);
                    }
                }
            }
            Ok(with_host(|h| h.new_hash(m)))
        }
        // `merge!` / `update` — the in-place form of `merge`: each hash argument
        // is merged into the receiver left-to-right, a block resolves collisions
        // by `block.call(key, old, new)`, and the receiver itself is returned.
        "merge!" | "update" => {
            let mut m = map;
            for a in args {
                if let Some(other) = with_host(|h| h.as_hash(a)) {
                    if let Some(b) = &block {
                        for (k, v) in other {
                            if let Some(old) = m.get(&k) {
                                let kv = with_host(|h| h.key_value(&k));
                                let merged = call_proc(b, &[kv, old.clone(), v.clone()])?;
                                m.insert(k, merged);
                            } else {
                                m.insert(k, v);
                            }
                        }
                    } else {
                        m.extend(other);
                    }
                }
            }
            with_host(|h| h.set_hash(recv, m));
            Ok(recv.clone())
        }
        "to_a" => Ok(with_host(|h| {
            let rows: Vec<Value> = map
                .iter()
                .map(|(k, v)| {
                    let kv = h.key_value(k);
                    h.new_array(vec![kv, v.clone()])
                })
                .collect();
            h.new_array(rows)
        })),
        "each" | "each_pair" => {
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    call_proc(b, &[kv, v.clone()])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
            }
            Ok(recv.clone())
        }
        "map" => {
            let mut out = Vec::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    // Yield the `[k, v]` pair as a single argument: a 2-param
                    // block auto-splats it, and a 1-param destructuring block
                    // (`|(k, v)|`) receives the whole pair to unpack.
                    let pair = new_arr(vec![kv, v.clone()]);
                    out.push(call_proc(b, &[pair])?);
                }
            }
            Ok(new_arr(out))
        }
        "select" | "filter" | "reject" => {
            let keep = name != "reject";
            let mut out = IndexMap::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    if with_host(|h| h.truthy(&r)) == keep {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        "transform_values" => {
            let mut out = IndexMap::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    out.insert(k.clone(), call_proc(b, std::slice::from_ref(v))?);
                }
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        "transform_keys" => {
            // `transform_keys(hash = {}) { |key| ... }` — a key present in the
            // mapping hash is rewritten to its mapped value; otherwise the block
            // (if any) computes the new key; otherwise the key is unchanged.
            let mapping = args.first().and_then(|a| with_host(|h| h.as_hash(a)));
            let mut out = IndexMap::new();
            for (k, v) in &map {
                let kv = with_host(|h| h.key_value(k));
                let nkey = if let Some(mapped) = mapping.as_ref().and_then(|m| m.get(k)) {
                    with_host(|h| h.value_to_key(mapped))
                } else if let Some(b) = &block {
                    let nk = call_proc(b, std::slice::from_ref(&kv))?;
                    with_host(|h| h.value_to_key(&nk))
                } else {
                    k.clone()
                };
                out.insert(nkey, v.clone());
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        "filter_map" => {
            let mut out = Vec::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    if with_host(|h| h.truthy(&r)) {
                        out.push(r);
                    }
                }
            }
            Ok(new_arr(out))
        }
        "each_with_object" => {
            let memo = args[0].clone();
            if let Some(b) = &block {
                for (k, v) in &map {
                    // Hash#each_with_object yields the [key, value] pair and the memo.
                    let pair = with_host(|h| {
                        let kv = h.key_value(k);
                        h.new_array(vec![kv, v.clone()])
                    });
                    call_proc(b, &[pair, memo.clone()])?;
                }
            }
            Ok(memo)
        }
        "sum" => {
            let mut acc = args.first().cloned().unwrap_or(Value::Int(0));
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    acc = add_values(&acc, &r);
                }
            }
            Ok(acc)
        }
        "any?" | "all?" | "none?" => {
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    let t = with_host(|h| h.truthy(&r));
                    match name {
                        "any?" if t => return Ok(Value::Bool(true)),
                        "all?" if !t => return Ok(Value::Bool(false)),
                        "none?" if t => return Ok(Value::Bool(false)),
                        _ => {}
                    }
                }
            }
            Ok(Value::Bool(name != "any?"))
        }
        // Enumerable methods that iterate the hash as `[k, v]` pairs: delegate
        // to the array of pairs (blocks receive the pair, auto-splat to `|k, v|`
        // or destructure to `|(k, v)|`).
        "min_by" | "max_by" | "sort_by" | "reduce" | "inject" | "group_by" | "partition"
        | "find_all" | "chunk_while" | "each_slice" | "each_cons" | "minmax_by" => {
            let rows: Vec<Value> = with_host(|h| {
                map.iter()
                    .map(|(k, v)| {
                        let kv = h.key_value(k);
                        h.new_array(vec![kv, v.clone()])
                    })
                    .collect()
            });
            let tmp = with_host(|h| h.new_array(rows));
            remap_array_delegate(dispatch_array(&tmp, name, args, block), recv, name)
        }
        "invert" => {
            let mut out = IndexMap::new();
            for (k, v) in &map {
                let nk = with_host(|h| h.value_to_key(v));
                let kv = with_host(|h| h.key_value(k));
                out.insert(nk, kv);
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        // The value returned for a missing key (`Hash#default` / `#default=`).
        "default" => Ok(with_host(|h| h.hash_default(recv))),
        "default=" => {
            with_host(|h| h.set_hash_default(recv, args[0].clone()));
            Ok(args[0].clone())
        }
        "to_h" => Ok(recv.clone()),
        "except" => {
            // Return a copy without the given keys (original order preserved).
            let drop: Vec<_> = args
                .iter()
                .map(|a| with_host(|h| h.value_to_key(a)))
                .collect();
            let mut out = IndexMap::new();
            for (k, v) in &map {
                if !drop.contains(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        "slice" => {
            // Return a copy with only the given keys, in argument order (MRI).
            let mut out = IndexMap::new();
            for a in args {
                let k = with_host(|h| h.value_to_key(a));
                if let Some(v) = map.get(&k) {
                    out.insert(k, v.clone());
                }
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        "compact" => {
            // Drop pairs whose value is nil.
            let mut out = IndexMap::new();
            for (k, v) in &map {
                if !matches!(v, Value::Undef) {
                    out.insert(k.clone(), v.clone());
                }
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        "flat_map" | "collect_concat" => {
            let mut out = Vec::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    if let Some(xs) = with_host(|h| h.as_array(&r)) {
                        out.extend(xs);
                    } else {
                        out.push(r);
                    }
                }
            }
            Ok(new_arr(out))
        }
        "each_with_index" => {
            let pairs: Vec<Value> = with_host(|h| {
                map.iter()
                    .map(|(k, v)| {
                        let kv = h.key_value(k);
                        h.new_array(vec![kv, v.clone()])
                    })
                    .collect()
            });
            if let Some(b) = &block {
                for (i, pair) in pairs.iter().enumerate() {
                    call_proc(b, &[pair.clone(), Value::Int(i as i64)])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                // No Enumerator type yet: return `[[k, v], index]` pairs so the
                // chain `each_with_index.to_a` / `.map { |kv, i| … }` still works.
                Ok(new_arr(
                    pairs
                        .into_iter()
                        .enumerate()
                        .map(|(i, pair)| new_arr(vec![pair, Value::Int(i as i64)]))
                        .collect(),
                ))
            }
        }
        "find" | "detect" => {
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv.clone(), v.clone()])?;
                    if with_host(|h| h.truthy(&r)) {
                        return Ok(with_host(|h| h.new_array(vec![kv, v.clone()])));
                    }
                }
            }
            Ok(Value::Undef)
        }
        "dig" => Ok(dig(recv, args)),
        _ => Err(no_method_error(recv, name)),
    }
}

// ---- Range ----------------------------------------------------------------

fn dispatch_range(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    // String ranges (`'a'..'e'`) iterate with `String#succ` succession.
    if let Some((lo, hi, excl)) = with_host(|h| h.as_str_range(recv)) {
        return dispatch_str_range(recv, name, args, block, lo, hi, excl);
    }
    // Float ranges (`1.0..2.0`) cannot be iterated directly (Ruby raises), but
    // support `step`, the endpoints, and the containment predicates.
    if let Some((lo, hi, excl)) = with_host(|h| h.as_float_range(recv)) {
        return dispatch_float_range(recv, name, args, block, lo, hi, excl);
    }
    let (lo, hi, excl) = with_host(|h| h.as_range(recv).unwrap());
    let endless = hi == crate::host::RANGE_ENDLESS;
    let beginless = lo == crate::host::RANGE_BEGINLESS;

    // An endless range (`1..`) supports the methods that don't need an upper
    // bound; anything that would materialize it raises like Ruby.
    if endless {
        match name {
            "first" | "take" if !args.is_empty() => {
                let n = as_i(&args[0]).max(0) as usize;
                return Ok(new_arr((lo..).take(n).map(Value::Int).collect()));
            }
            "first" | "begin" | "min" => return Ok(Value::Int(lo)),
            "include?" | "cover?" | "member?" | "===" => {
                return Ok(Value::Bool(as_i(&args[0]) >= lo));
            }
            "end" => return Ok(Value::Undef),
            "each" | "step" if block.is_some() => {
                let b = block.as_ref().unwrap();
                let by = if name == "step" {
                    as_i(&args[0]).max(1)
                } else {
                    1
                };
                let mut i = lo;
                loop {
                    call_proc(b, &[Value::Int(i)])?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                    i += by;
                }
                return Ok(recv.clone());
            }
            _ => {
                return Err(raise_exc(
                    "RangeError",
                    "cannot convert endless range to an array",
                ))
            }
        }
    }
    // A beginless range (`..5`) only supports the upper-bound queries.
    if beginless {
        match name {
            "include?" | "cover?" | "member?" | "===" => {
                let n = as_i(&args[0]);
                return Ok(Value::Bool(if excl { n < hi } else { n <= hi }));
            }
            "end" | "last" | "max" => return Ok(Value::Int(if excl { hi - 1 } else { hi })),
            _ => return Err(raise_exc("TypeError", "can't iterate from NilClass")),
        }
    }

    let end = if excl { hi } else { hi + 1 };
    match name {
        "to_a" | "to_ary" | "entries" => Ok(new_arr((lo..end).map(Value::Int).collect())),
        // `first(n)` / `last(n)` (with a count) return an Array; the no-arg
        // forms return the single boundary element.
        "first" if !args.is_empty() => {
            let n = as_i(&args[0]).max(0) as usize;
            Ok(new_arr((lo..end).take(n).map(Value::Int).collect()))
        }
        "last" if !args.is_empty() => {
            let n = as_i(&args[0]).max(0) as usize;
            let start = end.saturating_sub(n as i64).max(lo);
            Ok(new_arr((start..end).map(Value::Int).collect()))
        }
        "min" | "first" | "begin" => Ok(Value::Int(lo)),
        "max" | "last" | "end" if name != "end" => Ok(Value::Int(if excl { hi - 1 } else { hi })),
        "end" => Ok(Value::Int(hi)),
        "size" | "count" | "length" => Ok(Value::Int((end - lo).max(0))),
        "sum" => Ok(Value::Int((lo..end).sum())),
        // `cover?` (only) accepts a Range argument (Ruby 2.6+): true when the
        // other range's span lies entirely within self. `include?`/`member?`/
        // `===` treat the argument as a single element, never a sub-range.
        "cover?" if with_host(|h| h.as_range(&args[0]).is_some()) => {
            let (olo, ohi, oexcl) = with_host(|h| h.as_range(&args[0]).unwrap());
            if olo == crate::host::RANGE_BEGINLESS || ohi == crate::host::RANGE_ENDLESS {
                // A beginless/endless other range can't be bounded by a finite one.
                return Ok(Value::Bool(false));
            }
            // Effective inclusive maxima (self is bounded here; `end` = hi+ (!excl)).
            let smax = if excl { hi - 1 } else { hi };
            let omax = if oexcl { ohi - 1 } else { ohi };
            Ok(Value::Bool(olo >= lo && omax <= smax))
        }
        "include?" | "cover?" | "member?" | "===" => {
            let n = as_i(&args[0]);
            Ok(Value::Bool(n >= lo && n < end))
        }
        "step" => {
            // A Float step over an Integer range steps in Float space, matching
            // Ruby (`(1..2).step(0.5)` → `[1.0, 1.5, 2.0]`).
            if matches!(args.first(), Some(Value::Float(_))) {
                let by = as_f(&args[0]);
                let vals = float_range_step(lo as f64, hi as f64, excl, by);
                return match block {
                    Some(b) => {
                        for v in vals {
                            call_proc(&b, &[v])?;
                            if has_pending_signal() {
                                if let Some(bv) = take_break() {
                                    return Ok(bv);
                                }
                                break;
                            }
                        }
                        Ok(recv.clone())
                    }
                    None => Ok(with_host(|h| h.new_enumerator(vals, "each"))),
                };
            }
            let n = as_i(&args[0]);
            if n <= 0 {
                return Err(raise_exc("ArgumentError", "step can't be negative"));
            }
            let vals: Vec<Value> = (lo..end).step_by(n as usize).map(Value::Int).collect();
            if let Some(b) = &block {
                for v in vals {
                    call_proc(b, &[v])?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
                Ok(recv.clone())
            } else {
                Ok(new_arr(vals))
            }
        }
        "each" => {
            if let Some(b) = &block {
                for i in lo..end {
                    call_proc(b, &[Value::Int(i)])?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
            }
            Ok(recv.clone())
        }
        // Everything else falls back to the eager Array implementation over the
        // materialized elements (Range is Enumerable).
        _ => {
            let arr: Vec<Value> = (lo..end).map(Value::Int).collect();
            let tmp = with_host(|h| h.new_array(arr));
            remap_array_delegate(dispatch_array(&tmp, name, args, block), recv, name)
        }
    }
}

/// Materialize a String range into its elements via `String#succ` succession,
/// with Ruby's length guard (stop once the successor grows past the endpoint).
fn str_range_vec(lo: &str, hi: &str, excl: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = lo.to_string();
    loop {
        if cur.len() > hi.len() {
            break;
        }
        match cur.as_str().cmp(hi) {
            std::cmp::Ordering::Greater => break,
            std::cmp::Ordering::Equal => {
                if !excl {
                    out.push(cur);
                }
                break;
            }
            std::cmp::Ordering::Less => {
                let next = str_succ(&cur);
                out.push(cur);
                cur = next;
            }
        }
    }
    out
}

/// The stepped values of a Float range `(lo..hi)` / `(lo...hi)` by `by`, using
/// Ruby's float-step count (so accumulation error doesn't lose the last value)
/// and clamping an inclusive final value exactly to `hi`. An exclusive range
/// drops any value that reaches `hi`.
fn float_range_step(lo: f64, hi: f64, excl: bool, by: f64) -> Vec<Value> {
    if by <= 0.0 || !by.is_finite() {
        return Vec::new();
    }
    let mut err = (lo.abs() + hi.abs() + (hi - lo).abs()) / by.abs() * f64::EPSILON;
    if err > 0.5 {
        err = 0.5;
    }
    let n = ((hi - lo) / by + err).floor() + 1.0;
    let mut out = Vec::new();
    let mut i = 0.0f64;
    while i < n {
        let mut v = lo + i * by;
        if v > hi {
            v = hi;
        }
        if excl && v >= hi {
            break;
        }
        out.push(Value::Float(v));
        i += 1.0;
    }
    out
}

/// `Float` range methods. Ruby forbids iterating a Float range directly, so
/// `each`/`to_a`/`map`/… raise `TypeError`; the endpoints, containment
/// predicates, and `step` are supported.
fn dispatch_float_range(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
    lo: f64,
    hi: f64,
    excl: bool,
) -> Result<Value, String> {
    let contains = |x: f64| {
        if excl {
            x >= lo && x < hi
        } else {
            x >= lo && x <= hi
        }
    };
    match name {
        "begin" | "first" | "min" if args.is_empty() => Ok(Value::Float(lo)),
        "end" | "last" if args.is_empty() => Ok(Value::Float(hi)),
        "max" if args.is_empty() => {
            if excl {
                // Ruby raises for `(1.0...2.0).max` — no well-defined float max.
                Err(raise_exc(
                    "TypeError",
                    "cannot exclude non Integer end value",
                ))
            } else {
                Ok(Value::Float(hi))
            }
        }
        "exclude_end?" => Ok(Value::Bool(excl)),
        "include?" | "member?" | "cover?" | "===" => Ok(Value::Bool(contains(as_f(&args[0])))),
        "step" | "%" => {
            let by = args.first().map(as_f).unwrap_or(1.0);
            let vals = float_range_step(lo, hi, excl, by);
            match block {
                Some(b) => {
                    for v in vals {
                        call_proc(&b, &[v])?;
                        if has_pending_signal() {
                            if let Some(bv) = take_break() {
                                return Ok(bv);
                            }
                            break;
                        }
                    }
                    Ok(recv.clone())
                }
                None => Ok(with_host(|h| h.new_enumerator(vals, "each"))),
            }
        }
        "to_s" | "inspect" => Ok(new_str(with_host(|h| h.to_s(recv)))),
        // Iterating a Float range directly is a TypeError in Ruby.
        "each" | "to_a" | "to_ary" | "map" | "collect" | "select" | "filter" | "reject" | "sum"
        | "reduce" | "inject" | "each_with_index" | "each_with_object" | "count" | "size" => {
            Err(raise_exc("TypeError", "can't iterate from Float"))
        }
        _ => Err(no_method_error(recv, name)),
    }
}

fn dispatch_str_range(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
    lo: String,
    hi: String,
    excl: bool,
) -> Result<Value, String> {
    let elems = || -> Vec<Value> {
        str_range_vec(&lo, &hi, excl)
            .into_iter()
            .map(new_str)
            .collect()
    };
    match name {
        "to_a" | "to_ary" | "entries" => Ok(new_arr(elems())),
        "begin" | "first" if args.is_empty() => Ok(new_str(lo)),
        "end" => Ok(new_str(hi)),
        "first" => {
            let n = as_i(&args[0]).max(0) as usize;
            Ok(new_arr(elems().into_iter().take(n).collect()))
        }
        "last" if args.is_empty() => Ok(elems().pop().unwrap_or(Value::Undef)),
        "last" => {
            let n = as_i(&args[0]).max(0) as usize;
            let all = elems();
            let start = all.len().saturating_sub(n);
            Ok(new_arr(all[start..].to_vec()))
        }
        "min" => Ok(new_str(lo)),
        "max" if !excl => Ok(new_str(hi)),
        // A String range has no computable `size` (its elements aren't numeric),
        // so Ruby returns nil. `count` still counts via the Enumerable fallback.
        "size" => Ok(Value::Undef),
        "include?" | "member?" => {
            let s = arg_str(&args[0]);
            Ok(Value::Bool(str_range_vec(&lo, &hi, excl).contains(&s)))
        }
        "cover?" | "===" => {
            let s = arg_str(&args[0]);
            let upper = if excl {
                s.as_str() < hi.as_str()
            } else {
                s.as_str() <= hi.as_str()
            };
            Ok(Value::Bool(s.as_str() >= lo.as_str() && upper))
        }
        "each" => {
            if let Some(b) = &block {
                for e in elems() {
                    call_proc(b, &[e])?;
                    if has_pending_signal() {
                        if let Some(bv) = take_break() {
                            return Ok(bv);
                        }
                        break;
                    }
                }
            }
            Ok(recv.clone())
        }
        // Enumerable fallback over the materialized elements.
        _ => {
            let arr = elems();
            let tmp = with_host(|h| h.new_array(arr));
            remap_array_delegate(dispatch_array(&tmp, name, args, block), recv, name)
        }
    }
}

// ---- Symbol / Proc --------------------------------------------------------

fn dispatch_symbol(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let s = with_host(|h| h.as_symbol(recv).unwrap_or_default());
    match name {
        "to_s" | "id2name" | "name" => Ok(new_str(s)),
        // `to_sym` returns self; `itself`/`intern` are Ruby aliases in this context.
        "to_sym" | "intern" => Ok(recv.clone()),
        "length" | "size" => Ok(Value::Int(s.chars().count() as i64)),
        "empty?" => Ok(Value::Bool(s.is_empty())),
        // Case/`succ`/`capitalize` all return a Symbol (unlike String's String).
        "upcase" => Ok(with_host(|h| h.new_symbol(&s.to_uppercase()))),
        "downcase" => Ok(with_host(|h| h.new_symbol(&s.to_lowercase()))),
        "succ" | "next" => Ok(with_host(|h| h.new_symbol(&str_succ(&s)))),
        // `swapcase` returns a Symbol (unlike String's String), inverting case.
        "swapcase" => {
            let out: String = s
                .chars()
                .map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().collect::<String>()
                    } else if c.is_lowercase() {
                        c.to_uppercase().collect::<String>()
                    } else {
                        c.to_string()
                    }
                })
                .collect();
            Ok(with_host(|h| h.new_symbol(&out)))
        }
        "capitalize" => {
            let mut c = s.chars();
            let cap = match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            };
            Ok(with_host(|h| h.new_symbol(&cap)))
        }
        // `Symbol#[]` indexes the name and returns a String, like `String#[]`.
        "[]" | "slice" => Ok(str_index(&s, args)),
        "start_with?" => Ok(Value::Bool(args.iter().any(|a| s.starts_with(&arg_str(a))))),
        "end_with?" => Ok(Value::Bool(args.iter().any(|a| s.ends_with(&arg_str(a))))),
        // `match?` tests the name against a Regexp (or string pattern) without $~.
        "match?" => {
            let m = str_regex(&args[0])
                .map(|re| re.is_match(&s).unwrap_or(false))
                .unwrap_or(false);
            Ok(Value::Bool(m))
        }
        // `<=>` compares names; nil when the other operand is not a Symbol.
        "<=>" => Ok(match with_host(|h| h.as_symbol(&args[0])) {
            Some(other) => Value::Int(match s.cmp(&other) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }),
            None => Value::Undef,
        }),
        // `&:upcase` — a proc that sends the named method to its first argument.
        "to_proc" => Ok(with_host(|h| h.new_sym_proc(&s))),
        _ => Err(no_method_error(recv, name)),
    }
}

fn dispatch_proc(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    // A `Symbol#to_proc` proc: calling it sends the symbol's method to arg[0].
    if let Some(sym) = with_host(|h| h.as_sym_proc(recv)) {
        return match name {
            "call" | "()" | "[]" | "yield" => {
                if args.is_empty() {
                    Err(raise_exc("ArgumentError", "no receiver is available"))
                } else {
                    dispatch(&args[0], &sym, &args[1..], None)
                }
            }
            "to_proc" => Ok(recv.clone()),
            _ => Err(no_method_error(recv, name)),
        };
    }
    match name {
        "call" | "()" | "[]" | "yield" => call_proc(recv, args),
        "arity" => Ok(Value::Int(with_host(|h| h.proc_arity(recv)).unwrap_or(0))),
        "lambda?" => Ok(Value::Bool(with_host(|h| h.proc_is_lambda(recv)))),
        // `to_proc` on a Proc is the identity.
        "to_proc" => Ok(recv.clone()),
        "curry" => with_host(|h| h.proc_curry(recv))
            .ok_or_else(|| "undefined method 'curry' for Proc".to_string()),
        // Composition: `(f >> g).call(x) == g.call(f.call(x))`.
        ">>" => {
            let g = args[0].clone();
            let is_lambda = with_host(|h| h.proc_is_lambda(recv));
            Ok(with_host(|h| h.new_composed(recv.clone(), g, is_lambda)))
        }
        // `(f << g).call(x) == f.call(g.call(x))`.
        "<<" => {
            let g = args[0].clone();
            let is_lambda = with_host(|h| h.proc_is_lambda(recv));
            Ok(with_host(|h| h.new_composed(g, recv.clone(), is_lambda)))
        }
        _ => Err(no_method_error(recv, name)),
    }
}

/// Methods on a bound `Method` object (`obj.method(:m)`): `call`/`[]`/`()` route
/// back through dispatch on the captured receiver; `arity`, `name`, `to_proc`,
/// `receiver` expose its parts.
/// Invoke a bound `Method`'s target on its captured receiver, falling back to
/// the Kernel private methods (`puts`, `print`, `p`, `require`, `raise`, …) when
/// the receiver's class has no such instance method. MRI reaches those through
/// Kernel in every object's ancestry, so `method(:puts).call(...)` works on the
/// top-level `main` object even though `puts` is not a user-defined method.
pub(crate) fn call_bound(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match dispatch(recv, name, args, block.clone()) {
        Err(e) if e.starts_with("undefined method") => kernel(name, args, block).map_err(|_| e),
        other => other,
    }
}

fn dispatch_method(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let (mrecv, mname) = match with_host(|h| h.as_method(recv)) {
        Some(m) => m,
        None => return Err(no_method_error(recv, name)),
    };
    match name {
        "call" | "()" | "[]" | "yield" | "===" => call_bound(&mrecv, &mname, args, block),
        // UnboundMethod (or Method) rebinding: `bind(obj)` yields a Method bound
        // to `obj`; `bind_call(obj, *args)` binds and invokes in one step.
        "bind" => Ok(with_host(|h| h.new_method(args[0].clone(), &mname))),
        "bind_call" => call_bound(&args[0], &mname, &args[1..], block),
        // `Method#unbind` — drop the receiver, yielding an UnboundMethod.
        "unbind" => Ok(with_host(|h| h.new_method(Value::Undef, &mname))),
        "arity" => Ok(Value::Int(with_host(|h| h.method_arity(&mrecv, &mname)))),
        "parameters" => Ok(with_host(|h| {
            let pairs = h.method_parameters(&mrecv, &mname);
            let arr: Vec<Value> = pairs
                .iter()
                .map(|(kind, pname)| {
                    let k = h.new_symbol(kind);
                    let n = h.new_symbol(pname);
                    h.new_array(vec![k, n])
                })
                .collect();
            h.new_array(arr)
        })),
        "name" => Ok(with_host(|h| h.new_symbol(&mname))),
        "receiver" => Ok(mrecv),
        // A `Method` is itself callable via `call_proc`, so `to_proc` is identity.
        "to_proc" => Ok(recv.clone()),
        _ => Err(no_method_error(recv, name)),
    }
}

/// An `Encoding` object named `name` (`String#encoding`, `Encoding::UTF_8`, …).
/// Canonicalize a `force_encoding` argument (a name string or an `Encoding`
/// object) to `"ASCII-8BIT"` for the binary aliases, otherwise the raw name.
/// Only the binary tag is tracked; every other name maps to non-binary.
fn normalize_encoding_name(raw: &str) -> Option<String> {
    let name = raw.trim().to_ascii_uppercase();
    match name.as_str() {
        "ASCII-8BIT" | "ASCII_8BIT" | "BINARY" => Some("ASCII-8BIT".to_string()),
        other => Some(other.to_string()),
    }
}

/// A best-effort `Thread::Backtrace::Location` for `caller_locations`. Carries
/// path/lineno/label as ivars with attr-readers registered once, so
/// `.path`/`.lineno`/`.label`/`.absolute_path`/`.base_label` resolve. Precise
/// live stack frames aren't tracked outside `--dap`; callers use it for
/// `module_eval` backtrace attribution only.
fn backtrace_location(path: &str, lineno: i64, label: &str) -> Value {
    with_host(|h| {
        let cls = "Thread::Backtrace::Location";
        if h.find_method(cls, "path").is_none() {
            for f in ["path", "lineno", "label", "absolute_path", "base_label"] {
                h.add_attr(cls, f, true, false);
            }
        }
        let obj = h.new_object(cls);
        let p = h.new_string(path.to_string());
        h.set_ivar_of(&obj, "path", p.clone());
        h.set_ivar_of(&obj, "absolute_path", p);
        h.set_ivar_of(&obj, "lineno", Value::Int(lineno));
        let l = h.new_string(label.to_string());
        h.set_ivar_of(&obj, "label", l.clone());
        h.set_ivar_of(&obj, "base_label", l);
        obj
    })
}

/// `CGI.escape` — percent-encode for `application/x-www-form-urlencoded`: keep
/// `[A-Za-z0-9_.-]`, space becomes `+`, everything else `%XX`.
fn cgi_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `CGI.unescape` — inverse of `cgi_escape`: `+` → space, `%XX` → byte.
fn cgi_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `CGI.escapeHTML` — escape the five HTML metacharacters.
fn cgi_escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// `CGI.unescapeHTML` — inverse of `cgi_escape_html` for the common entities.
fn cgi_unescape_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn encoding_object(name: &str) -> Value {
    let enc = with_host(|h| h.new_object("Encoding"));
    let nm = new_str(name.to_string());
    with_host(|h| h.set_ivar_of(&enc, "name", nm));
    enc
}

/// Escape regex metacharacters in `s` (`Regexp.escape`/`quote`), matching MRI's
/// set: metacharacters get a backslash, control chars become escape sequences.
fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '.' | '\\' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
            | '#' | '-' | ' ' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            _ => out.push(c),
        }
    }
    out
}

fn dispatch_regexp(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let (re, source) = with_host(|h| h.as_regex(recv)).unwrap();
    match name {
        "source" => Ok(new_str(source)),
        "match?" => {
            let s = arg_str(&args[0]);
            Ok(Value::Bool(re.is_match(&s).unwrap_or(false)))
        }
        "=~" => {
            let s = arg_str(&args[0]);
            match_data(&re, &s); // sets `$~`/`$1`..
            Ok(re
                .find(&s)
                .ok()
                .flatten()
                .map(|m| Value::Int(s[..m.start()].chars().count() as i64))
                .unwrap_or(Value::Undef))
        }
        "match" => {
            let s = arg_str(&args[0]);
            Ok(match_data(&re, &s))
        }
        "scan" => {
            let s = arg_str(&args[0]);
            Ok(scan_regex(&re, &s))
        }
        "to_s" => Ok(new_str(format!("(?-mix:{source})"))),
        "inspect" => Ok(new_str(format!("/{source}/"))),
        _ => Err(no_method_error(recv, name)),
    }
}

// ===========================================================================
// Kernel functions (self / top-level calls with no user method).
// ===========================================================================

/// The name Ruby reports for a value in a `can't convert X into ...` TypeError:
/// the literals `nil`/`true`/`false`, else the class name.
fn type_name_for(v: &Value) -> String {
    match v {
        Value::Undef => "nil".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        _ => with_host(|h| h.class_of(v)),
    }
}

/// Value of an ASCII digit char in the given base (2..=36), or `None`.
fn base_digit(b: u8, base: i64) -> Option<u8> {
    let v = match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'z' => b - b'a' + 10,
        b'A'..=b'Z' => b - b'A' + 10,
        _ => return None,
    };
    if (v as i64) < base {
        Some(v)
    } else {
        None
    }
}

/// Strip Ruby digit-grouping underscores, rejecting any that are not sandwiched
/// between two valid base digits (leading/trailing/doubled underscores are
/// invalid). Also rejects any non-digit char.
fn strip_underscores(s: &str, base: i64) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            let prev_ok = i > 0 && base_digit(bytes[i - 1], base).is_some();
            let next_ok = i + 1 < bytes.len() && base_digit(bytes[i + 1], base).is_some();
            if !prev_ok || !next_ok {
                return None;
            }
            continue;
        }
        base_digit(b, base)?;
        out.push(b as char);
    }
    Some(out)
}

/// Faithful `Kernel#Integer` string parse. `base == 0` auto-detects a radix
/// prefix (0x/0b/0o/0d) or a bare leading `0` (octal); an explicit base in
/// 2..=36 must agree with any prefix present. Returns `None` on invalid input.
fn ruby_integer_str(input: &str, base: i64) -> Option<i64> {
    let s = input.trim();
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut idx = 0;
    let mut neg = false;
    if bytes[0] == b'+' || bytes[0] == b'-' {
        neg = bytes[0] == b'-';
        idx = 1;
    }
    let rest = &s[idx..];
    let rb = rest.as_bytes();
    let mut base = base;
    let mut digits = rest;
    let mut had_prefix = false;
    if rb.len() >= 2 && rb[0] == b'0' {
        let (pbase, ok) = match rb[1] | 0x20 {
            b'x' => (16, base == 0 || base == 16),
            b'b' => (2, base == 0 || base == 2),
            b'o' => (8, base == 0 || base == 8),
            b'd' => (10, base == 0 || base == 10),
            _ => (0, false),
        };
        if pbase != 0 && ok {
            base = pbase;
            digits = &rest[2..];
            had_prefix = true;
        }
    }
    if !had_prefix && base == 0 {
        // A bare leading `0` (e.g. "077") is octal; everything else is decimal.
        base = if rb.len() > 1 && rb[0] == b'0' { 8 } else { 10 };
    }
    if !(2..=36).contains(&base) {
        return None;
    }
    let cleaned = strip_underscores(digits, base)?;
    if cleaned.is_empty() {
        return None;
    }
    let n = i64::from_str_radix(&cleaned, base as u32).ok()?;
    Some(if neg { -n } else { n })
}

/// Faithful `Kernel#Float` string parse: decimal (with digit-grouping
/// underscores and `.5`/`5.` forms) or a C99 hex float (`0x1.8p3`). Unlike
/// Rust's `f64::from_str`, this rejects `inf`/`nan`/`Infinity`. `None` if invalid.
fn ruby_float_str(input: &str) -> Option<f64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let (sign, body) = match s.as_bytes()[0] {
        b'+' => (1.0, &s[1..]),
        b'-' => (-1.0, &s[1..]),
        _ => (1.0, s),
    };
    let bb = body.as_bytes();
    if bb.len() >= 2 && bb[0] == b'0' && (bb[1] | 0x20) == b'x' {
        return parse_hex_float(&body[2..]).map(|v| sign * v);
    }
    // Decimal: allow only the float grammar's chars (this rejects inf/nan), and
    // require any underscore to sit between two digits.
    let bytes = s.as_bytes();
    let mut cleaned = String::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'_' => {
                let prev_ok = i > 0 && bytes[i - 1].is_ascii_digit();
                let next_ok = i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
                if !prev_ok || !next_ok {
                    return None;
                }
            }
            b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => cleaned.push(b as char),
            _ => return None,
        }
    }
    cleaned.parse::<f64>().ok()
}

/// Parse the body (after `0x`) of a C99 hex float: `[hex].[hex]p[+-]dec`.
fn parse_hex_float(s: &str) -> Option<f64> {
    let (mantissa, exp) = match s.split_once(['p', 'P']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (s, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((a, b)) => (a, b),
        None => (mantissa, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let mut val = 0.0f64;
    for c in int_part.chars() {
        val = val * 16.0 + c.to_digit(16)? as f64;
    }
    let mut scale = 1.0 / 16.0;
    for c in frac_part.chars() {
        val += c.to_digit(16)? as f64 * scale;
        scale /= 16.0;
    }
    Some(val * 2f64.powi(exp))
}

fn kernel(name: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    match name {
        // `eval("code")` — compile and run the string on the current host in the
        // current self/scope; definitions it makes persist. (Only the top-level /
        // current-binding form is supported; an explicit `Binding` argument is
        // not modeled.)
        "eval" => {
            let src = arg_str(&args[0]);
            crate::host::eval_in_place(&src)
        }
        // `caller` / `caller_locations` — the call stack. A precise live
        // backtrace isn't tracked outside `--dap`, so return a best-effort single
        // frame from the current file. activesupport uses this only for
        // `module_eval` backtrace attribution (reads `.path`/`.lineno`).
        "caller" => Ok(new_arr(vec![])),
        "caller_locations" => {
            let path = crate::host::current_file_path().unwrap_or_else(|| "(eval)".to_string());
            Ok(new_arr(vec![backtrace_location(&path, 0, "<main>")]))
        }
        "puts" => {
            if args.is_empty() {
                println!();
            }
            for a in args {
                puts_one(a);
            }
            Ok(Value::Undef)
        }
        "print" => {
            // `$,` (output field separator) is inserted between arguments and
            // `$\` (output record separator) is appended after the last one;
            // both default to nil (no separator/terminator).
            let sep = with_host(|h| match h.get_global(",") {
                Value::Undef => None,
                v => Some(h.to_s(&v)),
            });
            let term = with_host(|h| match h.get_global("\\") {
                Value::Undef => None,
                v => Some(h.to_s(&v)),
            });
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    if let Some(s) = &sep {
                        print!("{s}");
                    }
                }
                print!("{}", display(a));
            }
            if let Some(t) = &term {
                print!("{t}");
            }
            Ok(Value::Undef)
        }
        "p" => {
            for a in args {
                let s = with_host(|h| h.inspect(a));
                println!("{s}");
            }
            Ok(match args.len() {
                0 => Value::Undef,
                1 => args[0].clone(),
                _ => with_host(|h| h.new_array(args.to_vec())),
            })
        }
        "pp" => kernel("p", args, block),
        // `Kernel#open(path, mode="r")` delegates to `File.open` for a plain
        // path (no pipe/`|command` support).
        "open" => file_open(args, block),
        "require" => do_require(args, ReqMode::Require),
        "require_relative" => do_require(args, ReqMode::Relative),
        "load" => do_require(args, ReqMode::Load),
        // AOP: `intercept(pattern, kind, handler)` registers `handler` (a method
        // name) to run as :before/:after/:around advice on any called method whose
        // name matches the glob `pattern`.
        "intercept" => {
            let pattern = name_of(&args[0]);
            let kind = name_of(args.get(1).unwrap_or(&Value::Undef));
            let handler = name_of(args.get(2).unwrap_or(&Value::Undef));
            let advice = match kind.as_str() {
                "before" => crate::intercepts::Advice::Before,
                "after" => crate::intercepts::Advice::After,
                "around" => crate::intercepts::Advice::Around,
                other => {
                    return Err(raise_exc(
                        "ArgumentError",
                        &format!("unknown advice kind '{other}'"),
                    ))
                }
            };
            crate::intercepts::register(&pattern, advice, &handler)
                .map_err(|e| raise_exc("ArgumentError", &e))?;
            Ok(with_host(|h| h.new_symbol(&handler)))
        }
        "raise" | "fail" => {
            // Forms: `raise` / `raise "msg"` / `raise SomeError` /
            // `raise SomeError, "msg"` / `raise instance`. When the named class
            // defines its own `initialize`, build the exception through `new` so
            // a user `initialize` (and its `super("msg")`) runs — otherwise fall
            // back to the default `new_exception(class, message)`.
            let build = |cls: &str, ctor_args: &[Value]| -> Result<Value, String> {
                if with_host(|h| h.find_method(cls, "initialize")).is_some() {
                    dispatch_classref(cls, "new", ctor_args, None)
                } else {
                    let message = match ctor_args.first() {
                        Some(a) => with_host(|h| h.to_s(a)),
                        None => cls.to_string(),
                    };
                    Ok(with_host(|h| h.new_exception(cls, &message)))
                }
            };
            let exc = match args {
                [] => with_host(|h| h.new_exception("RuntimeError", "RuntimeError")),
                [a] => {
                    if let Some(cls) = with_host(|h| h.classref_name(a)) {
                        build(&cls, &[])?
                    } else if with_host(|h| h.object_class(a)).is_some() {
                        // Re-raising an existing exception instance.
                        a.clone()
                    } else {
                        let m = with_host(|h| h.to_s(a));
                        with_host(|h| h.new_exception("RuntimeError", &m))
                    }
                }
                [cls, rest @ ..] => {
                    if let Some(clsname) = with_host(|h| h.classref_name(cls)) {
                        build(&clsname, rest)?
                    } else {
                        let m = with_host(|h| h.to_s(cls));
                        with_host(|h| h.new_exception("RuntimeError", &m))
                    }
                }
            };
            // The propagated Err string is the exception's message (its `message`
            // ivar if set, else its class name) — used only if unrescued.
            let message = with_host(|h| match h.ivar_of(&exc, "message") {
                Value::Undef => h.class_of(&exc).to_string(),
                m => h.to_s(&m),
            });
            with_host(|h| h.set_pending_exc(exc));
            Err(message)
        }
        "rand" => Ok(kernel_rand(args)),
        "sleep" => Ok(Value::Int(0)),
        "srand" => {
            let seed = match args.first() {
                Some(Value::Int(n)) => *n,
                _ => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0),
            };
            Ok(Value::Int(rng_srand(seed)))
        }
        "Integer" => {
            // `Integer(str, base=0)` / `Integer(numeric)`. A base is only valid
            // for a string receiver; auto-detect (base 0) honours radix prefixes
            // (0x/0b/0o/0d) and a bare leading `0` (octal), plus digit-grouping
            // underscores and an optional sign.
            let has_base = args.len() >= 2;
            match &args[0] {
                Value::Int(n) => {
                    if has_base {
                        return Err(raise_exc(
                            "ArgumentError",
                            "base specified for non string value",
                        ));
                    }
                    Ok(Value::Int(*n))
                }
                Value::Float(f) => {
                    if has_base {
                        return Err(raise_exc(
                            "ArgumentError",
                            "base specified for non string value",
                        ));
                    }
                    Ok(Value::Int(*f as i64))
                }
                _ => match with_host(|h| h.as_str(&args[0])) {
                    Some(s) => {
                        let base = if has_base { as_i(&args[1]) } else { 0 };
                        if base != 0 && !(2..=36).contains(&base) {
                            return Err(raise_exc(
                                "ArgumentError",
                                &format!("invalid radix {base}"),
                            ));
                        }
                        match ruby_integer_str(&s, base) {
                            Some(n) => Ok(Value::Int(n)),
                            None => Err(raise_exc(
                                "ArgumentError",
                                &format!("invalid value for Integer(): {s:?}"),
                            )),
                        }
                    }
                    None => Err(raise_exc(
                        "TypeError",
                        &format!("can't convert {} into Integer", type_name_for(&args[0])),
                    )),
                },
            }
        }
        "Float" => match &args[0] {
            Value::Int(n) => Ok(Value::Float(*n as f64)),
            Value::Float(f) => Ok(Value::Float(*f)),
            _ => match with_host(|h| h.as_str(&args[0])) {
                Some(s) => match ruby_float_str(&s) {
                    Some(f) => Ok(Value::Float(f)),
                    None => Err(raise_exc(
                        "ArgumentError",
                        &format!("invalid value for Float(): {s:?}"),
                    )),
                },
                None => Err(raise_exc(
                    "TypeError",
                    &format!("can't convert {} into Float", type_name_for(&args[0])),
                )),
            },
        },
        "Rational" => {
            let num = with_host(|h| h.as_bigint(&args[0]))
                .ok_or_else(|| raise_exc("TypeError", "can't convert to Rational"))?;
            let den = match args.get(1) {
                Some(d) => with_host(|h| h.as_bigint(d))
                    .ok_or_else(|| raise_exc("TypeError", "can't convert to Rational"))?,
                None => num_bigint::BigInt::from(1),
            };
            use num_traits::Zero as _;
            if den.is_zero() {
                return Err(raise_exc("ZeroDivisionError", "divided by 0"));
            }
            let r = num_rational::BigRational::new(num, den);
            Ok(with_host(|h| h.new_rational(r)))
        }
        "Complex" => {
            let re = args.first().cloned().unwrap_or(Value::Int(0));
            let im = args.get(1).cloned().unwrap_or(Value::Int(0));
            Ok(with_host(|h| h.new_complex(re, im)))
        }
        "String" => Ok(with_host(|h| {
            let s = h.to_s(&args[0]);
            h.new_string(s)
        })),
        "Array" => Ok(match with_host(|h| h.as_array(&args[0])) {
            Some(a) => with_host(|h| h.new_array(a)),
            // A hash converts to its `to_a` form: an array of `[key, value]`
            // pairs (`Array({a: 1}) # => [[:a, 1]]`).
            None => match with_host(|h| h.as_hash(&args[0])) {
                Some(map) => with_host(|h| {
                    let rows: Vec<Value> = map
                        .iter()
                        .map(|(k, v)| {
                            let kv = h.key_value(k);
                            h.new_array(vec![kv, v.clone()])
                        })
                        .collect();
                    h.new_array(rows)
                }),
                None if matches!(args[0], Value::Undef) => with_host(|h| h.new_array(vec![])),
                // An object with a `to_a` (MatchData, Struct, Set, Range, …)
                // converts through it; anything else becomes a one-element array.
                None => match dispatch(&args[0], "to_a", &[], None)
                    .ok()
                    .and_then(|v| with_host(|h| h.as_array(&v)))
                {
                    Some(a) => with_host(|h| h.new_array(a)),
                    None => with_host(|h| h.new_array(vec![args[0].clone()])),
                },
            },
        }),
        // `Kernel#Hash(x)` — nil and `[]` convert to an empty Hash; a Hash passes
        // through; anything else raises TypeError (matching MRI, which only
        // accepts an empty array via the implicit `to_hash`-less path).
        "Hash" => {
            if matches!(args[0], Value::Undef) {
                return Ok(with_host(|h| h.new_hash(IndexMap::new())));
            }
            if with_host(|h| h.as_hash(&args[0]).is_some()) {
                return Ok(args[0].clone());
            }
            match with_host(|h| h.as_array(&args[0])) {
                Some(a) if a.is_empty() => Ok(with_host(|h| h.new_hash(IndexMap::new()))),
                _ => {
                    let cls = with_host(|h| h.class_of(&args[0]));
                    Err(raise_exc(
                        "TypeError",
                        &format!("can't convert {cls} into Hash"),
                    ))
                }
            }
        }
        "format" | "sprintf" => {
            let fmt = arg_str(&args[0]);
            // A lone trailing Hash supplies named references (`%<name>s`).
            if args.len() == 2 {
                if let Some(map) = with_host(|h| h.as_hash(&args[1])) {
                    return Ok(new_str(sprintf(&fmt, &[], Some(&map))));
                }
            }
            Ok(new_str(sprintf(&fmt, &args[1..], None)))
        }
        "printf" => {
            // `printf(fmt, *args)` writes the formatted string to stdout and
            // returns nil. A lone trailing Hash supplies `%<name>s` references.
            let fmt = arg_str(&args[0]);
            let out = if args.len() == 2 {
                match with_host(|h| h.as_hash(&args[1])) {
                    Some(map) => sprintf(&fmt, &[], Some(&map)),
                    None => sprintf(&fmt, &args[1..], None),
                }
            } else {
                sprintf(&fmt, &args[1..], None)
            };
            print!("{out}");
            Ok(Value::Undef)
        }
        "gets" => {
            // MRI's `Kernel#gets` sets `$_` to the line just read (`nil` at EOF).
            let line = read_line();
            with_host(|h| h.set_global("_", line.clone()));
            Ok(line)
        }
        "proc" => block.ok_or_else(|| "tried to create Proc without a block".into()),
        "lambda" => {
            let b = block.ok_or_else(|| String::from("tried to create Proc without a block"))?;
            with_host(|h| h.set_proc_lambda(&b));
            Ok(b)
        }
        "loop" => {
            if let Some(b) = &block {
                loop {
                    // `Kernel#loop` silently rescues `StopIteration` and stops,
                    // returning the iterator's result (nil for the common case).
                    if let Err(e) = call_proc(b, &[]) {
                        let exc = with_host(|h| h.take_pending_exc());
                        let cls = exc.as_ref().and_then(|v| with_host(|h| h.object_class(v)));
                        if cls.as_deref() == Some("StopIteration") {
                            return Ok(Value::Undef);
                        }
                        if let Some(v) = exc {
                            with_host(|h| h.set_pending_exc(v));
                        }
                        return Err(e);
                    }
                    if has_pending_signal() {
                        // `break value` inside the loop is the loop's value.
                        return Ok(take_break().unwrap_or(Value::Undef));
                    }
                }
            }
            Ok(Value::Undef)
        }
        "catch" => {
            // `catch(tag) { |tag| … }` runs the block, yielding the tag to it, and
            // returns the block's value — unless a `throw` for this exact tag
            // unwound into it, in which case the thrown value is returned. With no
            // argument a fresh unique object serves as the tag.
            let tag = match args.first() {
                Some(t) => t.clone(),
                None => with_host(|h| h.new_object("Object")),
            };
            let b = block.ok_or_else(|| raise_exc("LocalJumpError", "no block given (yield)"))?;
            let r = call_proc(&b, std::slice::from_ref(&tag))?;
            // A `throw` for our tag stops here; a non-matching throw (or any other
            // pending signal) is left in place to keep unwinding.
            match take_throw(&tag) {
                Some(v) => Ok(v),
                None => Ok(r),
            }
        }
        "throw" => {
            // `throw(tag)` / `throw(tag, value)` — unwind to the matching
            // `catch(tag)`; a bare `throw tag` carries `nil`.
            let tag = args.first().cloned().unwrap_or(Value::Undef);
            let value = args.get(1).cloned().unwrap_or(Value::Undef);
            raise_signal_throw(tag, value);
            Ok(Value::Undef)
        }
        "block_given?" => Ok(Value::Bool(current_block().is_some())),
        "exit" | "exit!" => {
            // `exit`/`exit(true)` → 0, `exit(false)` → 1, `exit(n)` → n.
            let code = match args.first() {
                None | Some(Value::Bool(true)) => 0,
                Some(Value::Bool(false)) => 1,
                Some(v) => as_i(v) as i32,
            };
            std::process::exit(code);
        }
        "warn" => {
            // `warn(*msgs)` writes each message (with a trailing newline) to
            // $stderr and returns nil. A message that already ends in a newline
            // is not doubled. Array args are flattened like `puts`.
            for v in args {
                if let Some(arr) = with_host(|h| h.as_array(v)) {
                    for e in &arr {
                        let s = with_host(|h| h.to_s(e));
                        if s.ends_with('\n') {
                            eprint!("{s}");
                        } else {
                            eprintln!("{s}");
                        }
                    }
                } else {
                    let s = with_host(|h| h.to_s(v));
                    if s.ends_with('\n') {
                        eprint!("{s}");
                    } else {
                        eprintln!("{s}");
                    }
                }
            }
            Ok(Value::Undef)
        }
        "abort" => {
            // `abort(msg)` writes `msg` to stderr; either form exits with 1.
            if let Some(v) = args.first() {
                let msg = with_host(|h| h.to_s(v));
                eprintln!("{msg}");
            }
            std::process::exit(1);
        }
        _ => Err(format!("undefined method '{name}'")),
    }
}

fn puts_one(v: &Value) {
    if let Some(arr) = with_host(|h| h.as_array(v)) {
        if arr.is_empty() {
            println!();
        }
        for e in &arr {
            puts_one(e);
        }
    } else {
        // Ruby's `puts` appends a newline only when the value's string form does
        // not already end in one.
        let s = display(v);
        if s.ends_with('\n') {
            print!("{s}");
        } else {
            println!("{s}");
        }
    }
}

thread_local! {
    // SplitMix64 state driving `rand`, plus the last seed `srand` returns.
    static RNG_STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0x2545F4914F6CDD1D) };
    static RNG_SEED: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Advance the thread-local SplitMix64 state and return the next 64-bit word.
fn rng_next() -> u64 {
    let z = RNG_STATE.with(|c| {
        let z = c.get().wrapping_add(0x9E3779B97F4A7C15);
        c.set(z);
        z
    });
    let mut z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Reseed the PRNG from `seed`; return the PREVIOUS seed (MRI `srand` semantics).
fn rng_srand(seed: i64) -> i64 {
    RNG_STATE.with(|c| c.set(seed as u64));
    RNG_SEED.with(|c| c.replace(seed))
}

/// Build a random value from one 64-bit word and `rand`'s argument: `rand(n)` →
/// `0..n-1`, `rand(a..b)` → an integer in the (inclusive/exclusive) range, and
/// no/zero argument → a uniform double in `[0, 1)` (top 53 bits, like MRI).
fn rand_value(z: u64, args: &[Value]) -> Value {
    let unit = || (z >> 11) as f64 / (1u64 << 53) as f64;
    match args.first() {
        Some(Value::Int(n)) if *n > 0 => Value::Int((z % (*n as u64)) as i64),
        Some(Value::Float(f)) if *f > 0.0 => Value::Float(unit() * f),
        Some(v) => {
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(v)) {
                let span = (hi - lo + if excl { 0 } else { 1 }).max(1) as u64;
                return Value::Int(lo + (z % span) as i64);
            }
            Value::Float(unit())
        }
        None => Value::Float(unit()),
    }
}

fn kernel_rand(args: &[Value]) -> Value {
    rand_value(rng_next(), args)
}

/// Advance a `Random` instance's own SplitMix64 state (stored in its `state`
/// ivar) and return the next 64-bit word — so each `Random.new(seed)` is an
/// independent, reproducible stream distinct from the global `rand`.
fn random_advance(recv: &Value) -> u64 {
    let cur = match with_host(|h| h.ivar_of(recv, "state")) {
        Value::Int(n) => n as u64,
        _ => 0x2545F4914F6CDD1D,
    };
    let z = cur.wrapping_add(0x9E3779B97F4A7C15);
    with_host(|h| h.set_ivar_of(recv, "state", Value::Int(z as i64)));
    let mut m = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    m = (m ^ (m >> 27)).wrapping_mul(0x94D049BB133111EB);
    m ^ (m >> 31)
}

/// `Mutex`/`Monitor` instance methods. The lock state is a `__locked` flag on the
/// object. Under the GVL a critical section with no blocking call runs without
/// interruption, so `synchronize` simply runs the block with the flag set and
/// clears it afterward (even if the block raises). Returns `Ok(None)` for a name
/// it does not handle.
fn mutex_method(recv: &Value, name: &str, block: Option<Value>) -> Result<Option<Value>, String> {
    let locked = || with_host(|h| matches!(h.ivar_of(recv, "__locked"), Value::Bool(true)));
    let set = |v: bool| with_host(|h| h.set_ivar_of(recv, "__locked", Value::Bool(v)));
    match name {
        "lock" => {
            set(true);
            Ok(Some(recv.clone()))
        }
        // `Monitor#new_cond` / `MonitorMixin#new_cond` — a condition variable bound
        // to this monitor (concurrent-ruby's ConditionSignalling uses it).
        "new_cond" => {
            let cvid = crate::host::new_condvar();
            Ok(Some(with_host(|h| {
                let o = h.new_object("ConditionVariable");
                h.set_ivar_of(&o, "__cvid", Value::Int(cvid as i64));
                o
            })))
        }
        "unlock" => {
            set(false);
            Ok(Some(recv.clone()))
        }
        "locked?" => Ok(Some(Value::Bool(locked()))),
        "owned?" => Ok(Some(Value::Bool(locked()))),
        "try_lock" => {
            if locked() {
                Ok(Some(Value::Bool(false)))
            } else {
                set(true);
                Ok(Some(Value::Bool(true)))
            }
        }
        "synchronize" | "mon_synchronize" | "enter" | "mon_enter" => {
            let b = match block {
                Some(b) => b,
                // `lock`-style entry with no block (Monitor#mon_enter).
                None => {
                    set(true);
                    return Ok(Some(recv.clone()));
                }
            };
            set(true);
            let r = call_proc(&b, &[]);
            set(false);
            r.map(Some)
        }
        "exit" | "mon_exit" => {
            set(false);
            Ok(Some(recv.clone()))
        }
        _ => Ok(None),
    }
}

/// `Queue`/`SizedQueue` instance methods, keyed by the `__qid` ivar. `pop`/`push`
/// block on the queue's own condvar (with the GVL released) — see `queue_pop`.
fn queue_method(recv: &Value, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
    let qid = match with_host(|h| h.ivar_of(recv, "__qid")) {
        Value::Int(n) => n as u32,
        _ => return Ok(None),
    };
    // `pop(true)`/`push(v, true)` request non-blocking (raise if would block).
    let non_block = |a: Option<&Value>| a.is_some_and(|v| with_host(|h| h.truthy(v)));
    match name {
        "push" | "enq" | "<<" | "append" => {
            let v = args.first().cloned().unwrap_or(Value::Undef);
            crate::host::queue_push(qid, v, non_block(args.get(1)))?;
            Ok(Some(recv.clone()))
        }
        "pop" | "deq" | "shift" => Ok(Some(crate::host::queue_pop(qid, non_block(args.first()))?)),
        "size" | "length" => Ok(Some(Value::Int(crate::host::queue_len(qid) as i64))),
        "empty?" => Ok(Some(Value::Bool(crate::host::queue_len(qid) == 0))),
        "num_waiting" => Ok(Some(Value::Int(0))),
        "clear" => {
            crate::host::queue_clear(qid);
            Ok(Some(recv.clone()))
        }
        "close" => {
            crate::host::queue_close(qid);
            Ok(Some(recv.clone()))
        }
        "closed?" => Ok(Some(Value::Bool(crate::host::queue_closed(qid)))),
        _ => Ok(None),
    }
}

/// `ConditionVariable` instance methods, keyed by the `__cvid` ivar. `wait`
/// unlocks the given `Mutex`, releases the GVL, parks until `signal`/`broadcast`,
/// then relocks the mutex (MRI semantics).
fn condvar_method(recv: &Value, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
    let cvid = match with_host(|h| h.ivar_of(recv, "__cvid")) {
        Value::Int(n) => n as u32,
        _ => return Ok(None),
    };
    match name {
        "wait" => {
            // Unlock the caller's mutex (arg 0) for the duration of the wait.
            let mutex = args.first().cloned();
            if let Some(m) = &mutex {
                with_host(|h| h.set_ivar_of(m, "__locked", Value::Bool(false)));
            }
            crate::host::condvar_wait(cvid);
            if let Some(m) = &mutex {
                with_host(|h| h.set_ivar_of(m, "__locked", Value::Bool(true)));
            }
            Ok(Some(recv.clone()))
        }
        "signal" => {
            crate::host::condvar_notify(cvid, false);
            Ok(Some(recv.clone()))
        }
        "broadcast" => {
            crate::host::condvar_notify(cvid, true);
            Ok(Some(recv.clone()))
        }
        _ => Ok(None),
    }
}

/// Instance methods of a `Random` object (`rand`, `seed`). Returns `Ok(None)`
/// for a name it does not handle.
fn random_method(recv: &Value, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
    match name {
        "rand" => Ok(Some(rand_value(random_advance(recv), args))),
        "seed" => Ok(Some(with_host(|h| h.ivar_of(recv, "seed")))),
        _ => Ok(None),
    }
}

// ---- Stdlib modules: SecureRandom / Base64 / Digest / OpenStruct -----------
//
// All dependency-free and MRI-faithful. Digest and Base64 are deterministic and
// verified byte-for-byte against the reference `ruby`; SecureRandom draws from
// the same thread-local SplitMix64 that backs `rand`, so its outputs are the
// right shape/length/format but not cryptographically strong (documented).

/// `n` pseudo-random bytes from the SplitMix64 PRNG (shared with `rand`).
fn secure_random_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        for b in rng_next().to_le_bytes() {
            if out.len() < n {
                out.push(b);
            }
        }
    }
    out
}

const B64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64-encode `data` with the chosen alphabet. `pad` appends `=` padding.
fn base64_encode_bytes(data: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(alphabet[((n >> 18) & 63) as usize] as char);
        out.push(alphabet[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(alphabet[((n >> 6) & 63) as usize] as char);
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alphabet[(n & 63) as usize] as char);
        } else if pad {
            out.push('=');
        }
    }
    out
}

/// Base64-decode, accepting both the standard and URL-safe alphabets and
/// skipping whitespace (lenient, covering `decode64`/`strict_decode64`/urlsafe).
fn base64_decode_bytes(s: &str) -> Vec<u8> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    };
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// MRI `Base64.encode64`: standard alphabet, padded, wrapped every 60 output
/// chars with `\n`, and a trailing `\n` (empty input yields `""`).
fn base64_encode64(data: &[u8]) -> String {
    let raw = base64_encode_bytes(data, B64_STD, true);
    if raw.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(raw.len() + raw.len() / 60 + 1);
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && i % 60 == 0 {
            out.push('\n');
        }
        out.push(c);
    }
    out.push('\n');
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 15) as u32, 16).unwrap());
    }
    s
}

/// MD5 (RFC 1321), pure Rust, returns the 16-byte digest.
fn md5_digest(msg: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let (mut a0, mut b0, mut c0, mut d0): (u32, u32, u32, u32) =
        (0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476);
    let mut data = msg.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_le_bytes());
    for block in data.chunks(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// SHA-1 (FIPS 180-4), pure Rust, returns the 20-byte digest.
fn sha1_digest(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut data = msg.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for block in data.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// SHA-256 (FIPS 180-4), pure Rust, returns the 32-byte digest.
fn sha256_digest(msg: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut data = msg.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for block in data.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (hi, vi) in h.iter_mut().zip(v.iter()) {
            *hi = hi.wrapping_add(*vi);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The raw digest for a `Digest::<ALGO>` module reference.
fn digest_of(algo: &str, data: &[u8]) -> Vec<u8> {
    match algo {
        "Digest::MD5" => md5_digest(data).to_vec(),
        "Digest::SHA1" => sha1_digest(data).to_vec(),
        _ => sha256_digest(data).to_vec(),
    }
}

/// Class-method dispatch for the dependency-free stdlib modules. Returns `None`
/// when `cls` is not one of them (so normal class-ref dispatch continues).
/// A binary String from raw bytes. rubylang strings are UTF-8 (no ASCII-8BIT
/// representation), so map each byte to the same Unicode codepoint (Latin-1):
/// this preserves every byte losslessly and round-trips through [`latin1_bytes`],
/// so `Zlib.inflate(Zlib.deflate(x)) == x` holds within the process. (`bytesize`
/// of the returned string over-counts bytes ≥ 128, a known cost of the UTF-8-only
/// string model — real ASCII-8BIT support is a separate, larger change.)
fn bin_str(bytes: Vec<u8>) -> Value {
    let s: String = bytes.iter().map(|&b| b as char).collect();
    with_host(|h| h.new_string(s))
}

/// Recover the raw bytes of a [`bin_str`]-encoded string: each char's codepoint
/// (0..=255) is one byte. Non-Latin-1 chars are truncated to their low byte.
fn latin1_bytes(v: &Value) -> Vec<u8> {
    with_host(|h| h.as_str(v))
        .unwrap_or_default()
        .chars()
        .map(|c| c as u32 as u8)
        .collect()
}

/// zlib-wrapped DEFLATE (`Zlib.deflate`). `level` is -1 (default) or 0..=9.
fn zlib_deflate(data: &[u8], level: i64) -> Result<Vec<u8>, String> {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    let comp = if (0..=9).contains(&level) {
        Compression::new(level as u32)
    } else {
        Compression::default()
    };
    let mut enc = ZlibEncoder::new(Vec::new(), comp);
    enc.write_all(data).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

/// Inverse of [`zlib_deflate`] (`Zlib.inflate`).
fn zlib_inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut dec = ZlibDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|_| raise_exc("Zlib::DataError", "invalid zlib stream"))?;
    Ok(out)
}

/// gzip compress (`Zlib.gzip`).
fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

/// gzip decompress (`Zlib.gunzip`).
fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|_| raise_exc("Zlib::DataError", "invalid gzip stream"))?;
    Ok(out)
}

/// CRC-32 (`Zlib.crc32`), continuable via `init`.
fn zlib_crc32(data: &[u8], init: u32) -> u32 {
    let mut crc = init ^ 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// Adler-32 (`Zlib.adler32`), continuable via `init` (1 for a fresh checksum).
fn zlib_adler32(data: &[u8], init: u32) -> u32 {
    let mut a = init & 0xFFFF;
    let mut b = (init >> 16) & 0xFFFF;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn dispatch_stdlib_module(cls: &str, name: &str, args: &[Value]) -> Option<Result<Value, String>> {
    let str_arg = |i: usize| -> Vec<u8> {
        args.get(i)
            .and_then(|a| with_host(|h| h.as_str(a)))
            .unwrap_or_default()
            .into_bytes()
    };
    match cls {
        // `Digest` namespace: `Digest::MD5` etc. resolve to the sub-module ref.
        "Digest" => match name {
            "MD5" | "SHA1" | "SHA256" => {
                Some(Ok(with_host(|h| h.class_ref(&format!("Digest::{name}")))))
            }
            "hexencode" => Some(Ok(new_str(hex_encode(&str_arg(0))))),
            _ => None,
        },
        "Digest::MD5" | "Digest::SHA1" | "Digest::SHA256" => match name {
            "hexdigest" => Some(Ok(new_str(hex_encode(&digest_of(cls, &str_arg(0)))))),
            "digest" => Some(Ok(new_str(
                String::from_utf8_lossy(&digest_of(cls, &str_arg(0))).into_owned(),
            ))),
            "base64digest" => Some(Ok(new_str(base64_encode_bytes(
                &digest_of(cls, &str_arg(0)),
                B64_STD,
                true,
            )))),
            _ => None,
        },
        "SecureRandom" => Some(dispatch_secure_random(name, args)),
        // `Zlib` — DEFLATE/zlib/gzip via `flate2`. `Zlib.deflate`/`Zlib::Deflate
        // .deflate` produce zlib-wrapped streams; `Zlib.gzip`/`gunzip` gzip ones.
        "Zlib" | "Zlib::Deflate" | "Zlib::Inflate" => {
            match (cls, name) {
                ("Zlib" | "Zlib::Deflate", "deflate") => {
                    let level = args.get(1).map(as_i).unwrap_or(-1);
                    // Input is ordinary text — its UTF-8 bytes are the payload.
                    Some(zlib_deflate(&str_arg(0), level).map(bin_str))
                }
                ("Zlib" | "Zlib::Inflate", "inflate") => {
                    // Input is compressed binary (a `bin_str` from deflate).
                    let out = zlib_inflate(&latin1_bytes(args.first().unwrap_or(&Value::Undef)));
                    Some(out.map(|b| with_host(|h| h.new_string(String::from_utf8_lossy(&b).into_owned()))))
                }
                ("Zlib", "gzip") => Some(gzip_compress(&str_arg(0)).map(bin_str)),
                ("Zlib", "gunzip") => {
                    let out = gzip_decompress(&latin1_bytes(args.first().unwrap_or(&Value::Undef)));
                    Some(out.map(|b| with_host(|h| h.new_string(String::from_utf8_lossy(&b).into_owned()))))
                }
                ("Zlib", "crc32") => {
                    let init = args.get(1).map(as_i).unwrap_or(0) as u32;
                    Some(Ok(Value::Int(zlib_crc32(&str_arg(0), init) as i64)))
                }
                ("Zlib", "adler32") => {
                    let init = args.get(1).map(as_i).unwrap_or(1) as u32;
                    Some(Ok(Value::Int(zlib_adler32(&str_arg(0), init) as i64)))
                }
                // `Zlib::Deflate` / `Zlib::Inflate` sub-refs from the `Zlib` module.
                ("Zlib", "Deflate" | "Inflate" | "GzipWriter" | "GzipReader") => {
                    Some(Ok(with_host(|h| h.class_ref(&format!("Zlib::{name}")))))
                }
                _ => None,
            }
        }
        "Base64" => match name {
            "encode64" => Some(Ok(new_str(base64_encode64(&str_arg(0))))),
            "strict_encode64" => Some(Ok(new_str(base64_encode_bytes(&str_arg(0), B64_STD, true)))),
            "urlsafe_encode64" => {
                // MRI defaults padding to true; `padding: false` drops the `=`.
                let pad = args
                    .get(1)
                    .and_then(|a| with_host(|h| h.as_hash(a)))
                    .and_then(|m| m.get(&RKey::Sym("padding".to_string())).cloned())
                    .map(|v| with_host(|h| h.truthy(&v)))
                    .unwrap_or(true);
                Some(Ok(new_str(base64_encode_bytes(&str_arg(0), B64_URL, pad))))
            }
            "decode64" | "strict_decode64" | "urlsafe_decode64" => {
                let s = args
                    .first()
                    .and_then(|a| with_host(|h| h.as_str(a)))
                    .unwrap_or_default();
                Some(Ok(new_str(
                    String::from_utf8_lossy(&base64_decode_bytes(&s)).into_owned(),
                )))
            }
            _ => None,
        },
        _ => None,
    }
}

/// `SecureRandom.*` — shape-faithful, drawn from the shared SplitMix64 PRNG.
fn dispatch_secure_random(name: &str, args: &[Value]) -> Result<Value, String> {
    let n = |dflt: usize| {
        args.first()
            .map(|a| as_i(a).max(0) as usize)
            .unwrap_or(dflt)
    };
    match name {
        "hex" => Ok(new_str(hex_encode(&secure_random_bytes(n(16))))),
        "bytes" => Ok(new_str(
            String::from_utf8_lossy(&secure_random_bytes(n(16))).into_owned(),
        )),
        "base64" => Ok(new_str(base64_encode_bytes(
            &secure_random_bytes(n(16)),
            B64_STD,
            true,
        ))),
        "urlsafe_base64" => Ok(new_str(base64_encode_bytes(
            &secure_random_bytes(n(16)),
            B64_URL,
            false,
        ))),
        "uuid" => {
            let mut b = secure_random_bytes(16);
            b[6] = (b[6] & 0x0f) | 0x40; // version 4
            b[8] = (b[8] & 0x3f) | 0x80; // variant 10
            let h = hex_encode(&b);
            Ok(new_str(format!(
                "{}-{}-{}-{}-{}",
                &h[0..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..32],
            )))
        }
        "alphanumeric" => {
            const CH: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
            let len = n(16);
            let s: String = (0..len)
                .map(|_| CH[(rng_next() % 62) as usize] as char)
                .collect();
            Ok(new_str(s))
        }
        "random_number" => {
            let z = rng_next();
            let unit = (z >> 11) as f64 / (1u64 << 53) as f64;
            match args.first() {
                Some(Value::Int(m)) if *m > 0 => Ok(Value::Int((z % (*m as u64)) as i64)),
                Some(Value::Float(f)) if *f > 0.0 => Ok(Value::Float(unit * f)),
                _ => Ok(Value::Float(unit)),
            }
        }
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for SecureRandom"),
        )),
    }
}

/// OpenStruct instance methods: dynamic reader/writer plus the container API.
/// State lives in the object's ivars (bare attribute names). Returns `Ok(None)`
/// when `name` is not one OpenStruct handles here.
fn openstruct_method(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Option<Value>, String> {
    // Ordered (name, value) pairs, restoring insertion order from the ivars.
    let pairs = || -> Vec<(String, Value)> {
        with_host(|h| {
            h.ivar_names(recv)
                .iter()
                .map(|n| {
                    let bare = n.trim_start_matches('@').to_string();
                    let v = h.ivar_of(recv, &bare);
                    (bare, v)
                })
                .collect()
        })
    };
    match name {
        // `os.foo = v` — a writer (the parser routes `foo=` here). Restricted to
        // identifier-like setters so operator names (`[]=`, `==`, `>=`) fall
        // through to their own arms below.
        _ if name.ends_with('=')
            && !args.is_empty()
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_') =>
        {
            let field = &name[..name.len() - 1];
            with_host(|h| h.set_ivar_of(recv, field, args[0].clone()));
            Ok(Some(args[0].clone()))
        }
        "[]" => {
            let field = name_of(&args[0]);
            Ok(Some(with_host(|h| h.ivar_of(recv, &field))))
        }
        "[]=" => {
            let field = name_of(&args[0]);
            with_host(|h| h.set_ivar_of(recv, &field, args[1].clone()));
            Ok(Some(args[1].clone()))
        }
        "to_h" => {
            let mut map = IndexMap::new();
            for (k, v) in pairs() {
                map.insert(RKey::Sym(k), v);
            }
            Ok(Some(with_host(|h| h.new_hash(map))))
        }
        "each_pair" | "each" => {
            if let Some(bl) = block {
                for (k, v) in pairs() {
                    let key = with_host(|h| h.new_symbol(&k));
                    call_proc(&bl, &[key, v])?;
                }
            }
            Ok(Some(recv.clone()))
        }
        "members" => Ok(Some(new_arr(
            pairs()
                .iter()
                .map(|(k, _)| with_host(|h| h.new_symbol(k)))
                .collect(),
        ))),
        "dig" => {
            let field = name_of(&args[0]);
            let v = with_host(|h| h.ivar_of(recv, &field));
            if args.len() > 1 && !matches!(v, Value::Undef) {
                Ok(Some(dispatch(&v, "dig", &args[1..], None)?))
            } else {
                Ok(Some(v))
            }
        }
        // Any other bare name is a dynamic reader: return the field or nil.
        _ if !name.ends_with('?') && !name.ends_with('!') && args.is_empty() && block.is_none() => {
            Ok(Some(with_host(|h| h.ivar_of(recv, name))))
        }
        _ => Ok(None),
    }
}

fn read_line() -> Value {
    let mut s = String::new();
    match std::io::stdin().read_line(&mut s) {
        Ok(0) => Value::Undef,
        Ok(_) => new_str(s),
        Err(_) => Value::Undef,
    }
}

/// One base-b digit (`d < base`), lower- or upper-case for a..f.
fn digit_char(d: u8, upper: bool) -> char {
    if d < 10 {
        (b'0' + d) as char
    } else if upper {
        (b'A' + (d - 10)) as char
    } else {
        (b'a' + (d - 10)) as char
    }
}

/// Ruby's `..`-prefixed two's-complement notation for a negative integer in
/// base `b` (b ∈ {2,8,16}): e.g. `-255` in hex → `"..f01"`. Returns the digit
/// text *including* the leading `".."` and the repeating fill digit (for
/// zero-padding). Computed as the b-1's complement of the magnitude plus one.
fn complement_body(n: i128, base: u8, upper: bool) -> (String, char) {
    let mut v = n.unsigned_abs();
    let mut digits: Vec<u8> = Vec::new();
    while v > 0 {
        digits.push((v % base as u128) as u8);
        v /= base as u128;
    }
    if digits.is_empty() {
        digits.push(0);
    }
    let b1 = base - 1;
    let mut comp: Vec<u8> = digits.iter().map(|d| b1 - d).collect();
    let mut carry = 1u8;
    for d in comp.iter_mut() {
        let s = *d + carry;
        *d = s % base;
        carry = s / base;
    }
    // The top magnitude digit is non-zero, so the carry cannot escape and the
    // infinite high digits stay `b1`.
    comp.reverse();
    let rc = digit_char(b1, upper);
    let s: String = comp.iter().map(|&d| digit_char(d, upper)).collect();
    // The infinite high digits are all `rc`; show one of them, then the digits
    // that differ. e.g. hex `-255` → comp `"01"` → `"..f01"`.
    let stripped = s.trim_start_matches(rc);
    (format!("..{rc}{stripped}"), rc)
}

/// Strip trailing zeros (and a trailing `.`) from a fixed-point string — the
/// C `%g` cleanup applied unless the `#` flag is set.
fn strip_trailing_zeros(s: &mut String) {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
}

/// Render a non-negative finite float in `%e`/`%E` form with Ruby's exponent
/// style (sign + at-least-two digits): `1.23e+04`.
fn fmt_e(f: f64, prec: usize, upper: bool, alt: bool) -> String {
    let raw = format!("{:.*e}", prec, f);
    let (mant, exp) = raw.split_once('e').unwrap();
    let mut mant = mant.to_string();
    if alt && !mant.contains('.') {
        mant.push('.');
    }
    let e: i32 = exp.parse().unwrap_or(0);
    let ec = if upper { 'E' } else { 'e' };
    let es = if e < 0 { '-' } else { '+' };
    format!("{mant}{ec}{es}{:02}", e.abs())
}

/// Render a non-negative finite float in `%g`/`%G` form (C general format).
fn fmt_g(f: f64, prec: usize, upper: bool, alt: bool) -> String {
    let p = if prec == 0 { 1 } else { prec };
    // Exponent after rounding to `p` significant digits.
    let raw = format!("{:.*e}", p - 1, f);
    let e: i32 = raw
        .split_once('e')
        .and_then(|(_, x)| x.parse().ok())
        .unwrap_or(0);
    if e >= -4 && e < p as i32 {
        let frac = (p as i32 - 1 - e).max(0) as usize;
        let mut s = format!("{:.*}", frac, f);
        if !alt {
            strip_trailing_zeros(&mut s);
        } else if !s.contains('.') {
            s.push('.');
        }
        s
    } else {
        let (mant, exp) = raw.split_once('e').unwrap();
        let mut mant = mant.to_string();
        if !alt {
            strip_trailing_zeros(&mut mant);
        } else if !mant.contains('.') {
            mant.push('.');
        }
        let ee: i32 = exp.parse().unwrap_or(0);
        let ec = if upper { 'E' } else { 'e' };
        let es = if ee < 0 { '-' } else { '+' };
        format!("{mant}{ec}{es}{:02}", ee.abs())
    }
}

/// `format`/`sprintf`/`String#%` — handles `%[flags][width][.precision]conv`
/// with flags `-`, `0`, `+`, ` `, `#`, `*` dynamic width/precision, and
/// conversions d/i/u/f/e/E/g/G/s/p/x/X/o/b/B/c/%.
fn sprintf(fmt: &str, args: &[Value], named: Option<&IndexMap<RKey, Value>>) -> String {
    let bytes: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut ai = 0;
    let next_arg = |ai: &mut usize| {
        let v = args.get(*ai).cloned().unwrap_or(Value::Undef);
        *ai += 1;
        v
    };
    // Resolve a named reference (`%<name>` / `%{name}`) from the Hash operand.
    let named_val = |name: &str| -> Value {
        named
            .and_then(|m| m.get(&RKey::Sym(name.to_string())).cloned())
            .unwrap_or(Value::Undef)
    };
    while i < bytes.len() {
        if bytes[i] != '%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i < bytes.len() && bytes[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // `%{name}` shorthand: substitute the named value's `to_s`, no spec.
        if i < bytes.len() && bytes[i] == '{' {
            i += 1;
            let mut nm = String::new();
            while i < bytes.len() && bytes[i] != '}' {
                nm.push(bytes[i]);
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // consume '}'
            }
            out.push_str(&arg_str(&named_val(&nm)));
            continue;
        }
        // `%N$` positional argument selector (1-based): `%2$s` uses args[1] and
        // does not advance the sequential counter. Only consumes the digits when
        // they are actually followed by `$`.
        let mut pos_arg: Option<usize> = None;
        {
            let save = i;
            let mut n = 0usize;
            let mut saw = false;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                n = n * 10 + (bytes[i] as usize - '0' as usize);
                i += 1;
                saw = true;
            }
            if saw && i < bytes.len() && bytes[i] == '$' {
                i += 1;
                pos_arg = n.checked_sub(1);
            } else {
                i = save;
            }
        }
        // `%<name>s` reference: Ruby accepts it anywhere in the spec (before
        // flags, between flags and width, or after width), so probe at each
        // stage and let the value flow into the conversion below.
        let mut named_arg: Option<Value> = None;
        macro_rules! probe_named {
            () => {
                if named_arg.is_none() && i < bytes.len() && bytes[i] == '<' {
                    i += 1;
                    let mut nm = String::new();
                    while i < bytes.len() && bytes[i] != '>' {
                        nm.push(bytes[i]);
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1; // consume '>'
                    }
                    named_arg = Some(named_val(&nm));
                }
            };
        }
        probe_named!();
        // flags
        let (mut left, mut zero, mut plus, mut space, mut alt) =
            (false, false, false, false, false);
        while i < bytes.len() {
            match bytes[i] {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                ' ' => space = true,
                '#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        probe_named!();
        // width (`*` = dynamic; a negative dynamic width means left-align)
        let mut width = 0usize;
        if i < bytes.len() && bytes[i] == '*' {
            i += 1;
            let w = as_i(&next_arg(&mut ai));
            if w < 0 {
                left = true;
                width = w.unsigned_abs() as usize;
            } else {
                width = w as usize;
            }
        } else {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                width = width * 10 + (bytes[i] as usize - '0' as usize);
                i += 1;
            }
        }
        probe_named!();
        // precision (`.` then digits or `*`; a negative dynamic precision drops it)
        let mut prec: Option<usize> = None;
        if i < bytes.len() && bytes[i] == '.' {
            i += 1;
            if i < bytes.len() && bytes[i] == '*' {
                i += 1;
                let p = as_i(&next_arg(&mut ai));
                if p >= 0 {
                    prec = Some(p as usize);
                }
            } else {
                let mut p = 0usize;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    p = p * 10 + (bytes[i] as usize - '0' as usize);
                    i += 1;
                }
                prec = Some(p);
            }
        }
        let conv = if i < bytes.len() { bytes[i] } else { '%' };
        i += 1;
        if conv == '%' {
            out.push('%');
            continue;
        }
        let arg = match (pos_arg, named_arg) {
            (Some(k), _) => args.get(k).cloned().unwrap_or(Value::Undef),
            (None, Some(v)) => v,
            (None, None) => next_arg(&mut ai),
        };

        // Per-conversion: (sign, prefix, body, numeric, int_conv, complement fill).
        let mut sign = "";
        let mut prefix = String::new();
        let mut numeric = false;
        let mut int_conv = false;
        let mut zero_ok = true;
        let mut complement: Option<char> = None;

        // Integer base conversions share sign / precision / `#` handling.
        let base_conv = |base: u8, up: bool| -> (String, Option<char>, String, &'static str) {
            let n = as_i(&arg) as i128;
            let neg = n < 0;
            if neg && !(plus || space) {
                // Ruby two's-complement `..` notation (no leading sign).
                let (b, rc) = complement_body(n, base, up);
                let pfx = if alt {
                    match base {
                        16 if up => "0X".to_string(),
                        16 => "0x".to_string(),
                        8 => String::new(), // handled by digit form below
                        2 if up => "0B".to_string(),
                        2 => "0b".to_string(),
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                (b, Some(rc), pfx, "")
            } else {
                let mag = n.unsigned_abs();
                let mut b = String::new();
                let mut v = mag;
                if v == 0 {
                    b.push('0');
                }
                while v > 0 {
                    b.push(digit_char((v % base as u128) as u8, up));
                    v /= base as u128;
                }
                let mut b: String = b.chars().rev().collect();
                // precision = minimum number of digits
                if let Some(p) = prec {
                    if mag == 0 && p == 0 {
                        b.clear();
                    } else if b.len() < p {
                        b = format!("{}{}", "0".repeat(p - b.len()), b);
                    }
                }
                let sgn = if neg {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                let pfx = if alt && mag != 0 {
                    match base {
                        16 if up => "0X".to_string(),
                        16 => "0x".to_string(),
                        2 if up => "0B".to_string(),
                        2 => "0b".to_string(),
                        8 => {
                            if b.starts_with('0') {
                                String::new()
                            } else {
                                b.insert(0, '0');
                                String::new()
                            }
                        }
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                (b, None, pfx, sgn)
            }
        };

        let body: String = match conv {
            'd' | 'i' | 'u' => {
                numeric = true;
                int_conv = true;
                let n = as_i(&arg);
                let neg = n < 0;
                sign = if neg {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                let mut b = n.unsigned_abs().to_string();
                if let Some(p) = prec {
                    if n == 0 && p == 0 {
                        b.clear();
                    } else if b.len() < p {
                        b = format!("{}{}", "0".repeat(p - b.len()), b);
                    }
                }
                b
            }
            'x' | 'X' | 'o' | 'b' | 'B' => {
                numeric = true;
                int_conv = true;
                let (base, up) = match conv {
                    'x' => (16, false),
                    'X' => (16, true),
                    'o' => (8, false),
                    'B' => (2, true),
                    _ => (2, false),
                };
                let (b, cpl, pfx, sgn) = base_conv(base, up);
                prefix = pfx;
                complement = cpl;
                sign = sgn;
                b
            }
            'f' => {
                numeric = true;
                let f = as_f(&arg);
                if !f.is_finite() {
                    zero_ok = false;
                    if !f.is_nan() && f.is_sign_negative() {
                        sign = "-";
                    } else if !f.is_nan() && plus {
                        sign = "+";
                    } else if !f.is_nan() && space {
                        sign = " ";
                    }
                    if f.is_nan() {
                        "NaN".into()
                    } else {
                        "Inf".into()
                    }
                } else {
                    if f.is_sign_negative() {
                        sign = "-";
                    } else if plus {
                        sign = "+";
                    } else if space {
                        sign = " ";
                    }
                    let p = prec.unwrap_or(6);
                    let mut s = format!("{:.*}", p, f.abs());
                    if alt && !s.contains('.') {
                        s.push('.');
                    }
                    s
                }
            }
            'e' | 'E' | 'g' | 'G' => {
                numeric = true;
                let f = as_f(&arg);
                let up = conv == 'E' || conv == 'G';
                if !f.is_finite() {
                    zero_ok = false;
                    if !f.is_nan() && f.is_sign_negative() {
                        sign = "-";
                    } else if !f.is_nan() && plus {
                        sign = "+";
                    } else if !f.is_nan() && space {
                        sign = " ";
                    }
                    if f.is_nan() {
                        "NaN".into()
                    } else {
                        "Inf".into()
                    }
                } else {
                    if f.is_sign_negative() {
                        sign = "-";
                    } else if plus {
                        sign = "+";
                    } else if space {
                        sign = " ";
                    }
                    if conv == 'e' || conv == 'E' {
                        fmt_e(f.abs(), prec.unwrap_or(6), up, alt)
                    } else {
                        fmt_g(f.abs(), prec.unwrap_or(6), up, alt)
                    }
                }
            }
            'c' => {
                zero_ok = false;
                match &arg {
                    Value::Int(n) => char::from_u32(*n as u32)
                        .map(String::from)
                        .unwrap_or_default(),
                    _ => {
                        let s = arg_str(&arg);
                        s.chars().next().map(String::from).unwrap_or_default()
                    }
                }
            }
            'p' => {
                zero_ok = false;
                let mut s = with_host(|h| h.inspect(&arg));
                if let Some(p) = prec {
                    s = s.chars().take(p).collect();
                }
                s
            }
            's' => {
                zero_ok = false;
                let mut s = arg_str(&arg);
                if let Some(p) = prec {
                    s = s.chars().take(p).collect();
                }
                s
            }
            other => {
                out.push('%');
                out.push(other);
                ai -= 1;
                continue;
            }
        };

        // Width padding (counted in characters).
        let numeric_zeropad = numeric && zero_ok && !(int_conv && prec.is_some());
        let vis = sign.chars().count() + prefix.chars().count() + body.chars().count();
        if width > vis {
            let deficit = width - vis;
            if left {
                out.push_str(sign);
                out.push_str(&prefix);
                out.push_str(&body);
                out.extend(std::iter::repeat(' ').take(deficit));
            } else if zero && numeric_zeropad {
                if let Some(rc) = complement {
                    out.push_str(&prefix);
                    out.push_str("..");
                    out.extend(std::iter::repeat(rc).take(deficit));
                    out.push_str(&body[2..]);
                } else {
                    out.push_str(sign);
                    out.push_str(&prefix);
                    out.extend(std::iter::repeat('0').take(deficit));
                    out.push_str(&body);
                }
            } else {
                out.extend(std::iter::repeat(' ').take(deficit));
                out.push_str(sign);
                out.push_str(&prefix);
                out.push_str(&body);
            }
        } else {
            out.push_str(sign);
            out.push_str(&prefix);
            out.push_str(&body);
        }
    }
    out
}

// ---- small value helpers --------------------------------------------------

fn new_str(s: String) -> Value {
    with_host(|h| h.new_string(s))
}

// ---- ERB (dependency-free template engine over the host value model) --------

/// Instance methods on an `ERB` object (its compiled Ruby source lives in the
/// `@src` ivar). Returns `Ok(None)` for a name ERB does not handle, so the caller
/// can fall through to the generic object dispatch.
///
/// - `result` / `result(binding)` — evaluate the compiled template in the
///   caller's current scope (its top-level locals, instance variables, and
///   methods are visible). An explicit `Binding` argument is accepted but not
///   modeled; evaluation always uses the current scope.
/// - `result_with_hash(hash)` — evaluate in a fresh, isolated scope with the
///   hash keys bound as template locals.
/// - `src` — the generated buffer-building Ruby source (matches MRI's `#src`).
fn erb_method(recv: &Value, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
    let src = || {
        let v = with_host(|h| h.ivar_of(recv, "src"));
        with_host(|h| h.as_str(&v)).unwrap_or_default()
    };
    match name {
        "result" => Ok(Some(crate::host::eval_in_place(&src())?)),
        "result_with_hash" => {
            let hash = args
                .first()
                .and_then(|a| with_host(|h| h.as_hash(a)))
                .unwrap_or_default();
            let mut locals = Vec::with_capacity(hash.len());
            for (k, v) in hash {
                let kv = with_host(|h| h.key_value(&k));
                locals.push((name_of(&kv), v));
            }
            Ok(Some(crate::host::eval_erb_with_locals(&src(), locals)?))
        }
        "src" => Ok(Some(new_str(src()))),
        // `run`/`run_with_hash` evaluate and print the result (MRI writes to $stdout).
        "run" => {
            let r = crate::host::eval_in_place(&src())?;
            print!("{}", with_host(|h| h.to_s(&r)));
            Ok(Some(Value::Undef))
        }
        _ => Ok(None),
    }
}

/// Compile an ERB template into Ruby source that builds a `_erbout` buffer and
/// evaluates to it. Each literal run becomes `_erbout << "..."`, each `<%= e %>`
/// becomes `_erbout << (e).to_s`, and each `<% c %>` emits `c` verbatim (so
/// loops/conditionals wrap the appends). `<%# ... %>` is dropped and `<%%`
/// yields a literal `<%`.
///
/// `dash_trim` enables the `"-"` trim mode: `-%>` chomps the following newline
/// and `<%-` strips leading blanks on its line. Template text is embedded in a
/// double-quoted Ruby string, so `#{...}` in text interpolates — matching MRI.
fn erb_compile(template: &str, dash_trim: bool) -> String {
    enum Tag {
        Expr,
        Comment,
        Code,
        CodeTrimLead,
    }

    /// Flush pending literal `text` as an `_erbout << "..."` append, escaping the
    /// characters that are special inside a Ruby double-quoted string. `#` is
    /// left unescaped so `#{...}` in template text interpolates (MRI behavior).
    fn flush(out: &mut String, text: &mut String) {
        if text.is_empty() {
            return;
        }
        out.push_str("_erbout << \"");
        for ch in text.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out.push_str("\"\n");
        text.clear();
    }

    let mut out = String::from("_erbout = +\"\"\n");
    let mut text = String::new();
    let bytes = template.as_bytes();
    let n = bytes.len();
    let mut i = 0;

    while i < n {
        let rest = &template[i..];
        // `<%%` is an escaped literal `<%`.
        if rest.starts_with("<%%") {
            text.push_str("<%");
            i += 3;
            continue;
        }
        if rest.starts_with("<%") {
            let mut p = i + 2;
            let tag = match bytes.get(p) {
                Some(b'=') => {
                    p += 1;
                    Tag::Expr
                }
                Some(b'#') => {
                    p += 1;
                    Tag::Comment
                }
                Some(b'-') if dash_trim => {
                    p += 1;
                    Tag::CodeTrimLead
                }
                _ => Tag::Code,
            };
            // `<%-` trims trailing blanks accumulated on the current line.
            if matches!(tag, Tag::CodeTrimLead) {
                while text.ends_with(' ') || text.ends_with('\t') {
                    text.pop();
                }
            }
            // Find the closing `%>`; if absent, treat the remainder as literal text.
            let Some(rel) = template[p..].find("%>") else {
                flush(&mut out, &mut text);
                text.push_str(&template[i..]);
                break;
            };
            let close = p + rel;
            let (content_end, trailing_trim) = if dash_trim && close > p && bytes[close - 1] == b'-'
            {
                (close - 1, true)
            } else {
                (close, false)
            };
            let content = template[p..content_end].trim();
            flush(&mut out, &mut text);
            match tag {
                Tag::Expr => {
                    out.push_str("_erbout << (");
                    out.push_str(content);
                    out.push_str(").to_s\n");
                }
                Tag::Comment => {}
                Tag::Code | Tag::CodeTrimLead => {
                    out.push_str(content);
                    out.push('\n');
                }
            }
            i = close + 2;
            // `-%>` chomps the immediately following newline.
            if trailing_trim {
                if template[i..].starts_with("\r\n") {
                    i += 2;
                } else if template[i..].starts_with('\n') {
                    i += 1;
                }
            }
            continue;
        }
        let ch = rest.chars().next().unwrap();
        text.push(ch);
        i += ch.len_utf8();
    }
    flush(&mut out, &mut text);
    out.push_str("_erbout\n");
    out
}

// ---- JSON (dependency-free, hand-written over the host value model) ---------

/// Encode a value as compact JSON, matching MRI `JSON.generate` byte-for-byte on
/// the common cases. Takes `&mut RubyHost` directly (never re-enters `with_host`).
fn json_encode(h: &mut RubyHost, v: &Value) -> Result<String, String> {
    match v {
        Value::Undef => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        // `to_s` on a Float is Ruby's `Float#to_s` (`1.5`, `2.0`, sci-notation),
        // which is what MRI's JSON emits for finite floats.
        Value::Float(_) => Ok(h.to_s(v)),
        _ => {
            if let Some(s) = h.as_str(v) {
                return Ok(json_escape(&s));
            }
            if let Some(s) = h.as_symbol(v) {
                return Ok(json_escape(&s));
            }
            if let Some(items) = h.as_array(v) {
                let mut parts = Vec::with_capacity(items.len());
                for e in &items {
                    parts.push(json_encode(h, e)?);
                }
                return Ok(format!("[{}]", parts.join(",")));
            }
            if let Some(map) = h.as_hash(v) {
                let mut parts = Vec::with_capacity(map.len());
                for (k, val) in &map {
                    let ks = json_key_string(h, k);
                    parts.push(format!("{}:{}", json_escape(&ks), json_encode(h, val)?));
                }
                return Ok(format!("{{{}}}", parts.join(",")));
            }
            // A promoted Integer (Bignum) encodes unquoted, like MRI.
            if let Some(b) = h.as_bigint(v) {
                return Ok(b.to_string());
            }
            // Everything else (Rational, Time, user objects, …) uses the default
            // `Object#to_json`: its `to_s` as a quoted JSON string.
            let s = h.to_s(v);
            Ok(json_escape(&s))
        }
    }
}

/// Pretty-printed JSON (`JSON.pretty_generate`): 2-space indent, `": "` after
/// keys, one element per line; empty containers stay `{}` / `[]`.
fn json_pretty(h: &mut RubyHost, v: &Value, depth: usize) -> Result<String, String> {
    if let Some(items) = h.as_array(v) {
        if items.is_empty() {
            return Ok("[]".to_string());
        }
        let ind = "  ".repeat(depth + 1);
        let close = "  ".repeat(depth);
        let mut parts = Vec::with_capacity(items.len());
        for e in &items {
            parts.push(format!("{ind}{}", json_pretty(h, e, depth + 1)?));
        }
        return Ok(format!("[\n{}\n{close}]", parts.join(",\n")));
    }
    if let Some(map) = h.as_hash(v) {
        if map.is_empty() {
            return Ok("{}".to_string());
        }
        let ind = "  ".repeat(depth + 1);
        let close = "  ".repeat(depth);
        let mut parts = Vec::with_capacity(map.len());
        for (k, val) in &map {
            let ks = json_key_string(h, k);
            parts.push(format!(
                "{ind}{}: {}",
                json_escape(&ks),
                json_pretty(h, val, depth + 1)?
            ));
        }
        return Ok(format!("{{\n{}\n{close}}}", parts.join(",\n")));
    }
    json_encode(h, v)
}

/// A hash key's JSON string form: symbol/string keys keep their text; every
/// other key type stringifies via `to_s` (`{1=>2}` → `"1"`, `{nil=>3}` → `""`).
fn json_key_string(h: &mut RubyHost, k: &RKey) -> String {
    match k {
        RKey::Str(s) | RKey::Sym(s) => s.clone(),
        RKey::Int(n) => n.to_string(),
        _ => {
            let kv = h.key_value(k);
            h.to_s(&kv)
        }
    }
}

/// A JSON string literal (quoted, MRI-exact escaping): only `" \ \b \t \n \f \r`
/// are named; other C0 controls become lowercase `\u00xx`; DEL and all non-ASCII
/// (UTF-8) pass through raw; `/` is not escaped.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Recursive-descent JSON decoder. Builds host Values: objects → Hash with
/// `RKey::Str` keys (or `RKey::Sym` when `symbolize`), integers → `Int` (Bignum
/// on overflow), reals → `Float`. Errors return a plain message (the caller
/// wraps it in `JSON::ParserError`).
fn json_parse(h: &mut RubyHost, s: &str, symbolize: bool) -> Result<Value, String> {
    let mut p = JsonParser {
        chars: s.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.parse_value(h, symbolize)?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err("unexpected token after JSON value".to_string());
    }
    Ok(v)
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            Some(' ') | Some('\t') | Some('\n') | Some('\r')
        ) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self, h: &mut RubyHost, symbolize: bool) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(h, symbolize),
            Some('[') => self.parse_array(h, symbolize),
            Some('"') => {
                let s = self.parse_string()?;
                Ok(h.new_string(s))
            }
            Some('t') => self.parse_lit("true", Value::Bool(true)),
            Some('f') => self.parse_lit("false", Value::Bool(false)),
            Some('n') => self.parse_lit("null", Value::Undef),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(h),
            _ => Err("unexpected token in JSON".to_string()),
        }
    }

    fn parse_lit(&mut self, lit: &str, v: Value) -> Result<Value, String> {
        for want in lit.chars() {
            if self.next() != Some(want) {
                return Err(format!("expected `{lit}`"));
            }
        }
        Ok(v)
    }

    fn parse_object(&mut self, h: &mut RubyHost, symbolize: bool) -> Result<Value, String> {
        self.pos += 1; // consume '{'
        let mut map: IndexMap<RKey, Value> = IndexMap::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(h.new_hash(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err("expected string key in JSON object".to_string());
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.next() != Some(':') {
                return Err("expected `:` in JSON object".to_string());
            }
            let val = self.parse_value(h, symbolize)?;
            let k = if symbolize {
                RKey::Sym(key)
            } else {
                RKey::Str(key)
            };
            map.insert(k, val);
            self.skip_ws();
            match self.next() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err("expected `,` or `}` in JSON object".to_string()),
            }
        }
        Ok(h.new_hash(map))
    }

    fn parse_array(&mut self, h: &mut RubyHost, symbolize: bool) -> Result<Value, String> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(h.new_array(items));
        }
        loop {
            let val = self.parse_value(h, symbolize)?;
            items.push(val);
            self.skip_ws();
            match self.next() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err("expected `,` or `]` in JSON array".to_string()),
            }
        }
        Ok(h.new_array(items))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.pos += 1; // consume opening '"'
        let mut out = String::new();
        loop {
            match self.next() {
                None => return Err("unterminated JSON string".to_string()),
                Some('"') => break,
                Some('\\') => match self.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{08}'),
                    Some('f') => out.push('\u{0c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let mut code: u32 = 0;
                        for _ in 0..4 {
                            let d = self
                                .next()
                                .and_then(|c| c.to_digit(16))
                                .ok_or_else(|| "invalid \\u escape in JSON".to_string())?;
                            code = code * 16 + d;
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    _ => return Err("invalid escape in JSON string".to_string()),
                },
                Some(c) => out.push(c),
            }
        }
        Ok(out)
    }

    fn parse_number(&mut self, h: &mut RubyHost) -> Result<Value, String> {
        let start = self.pos;
        let mut is_float = false;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            match c {
                '0'..='9' => self.pos += 1,
                '.' | 'e' | 'E' | '+' | '-' => {
                    is_float = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| "invalid number in JSON".to_string())
        } else {
            match text.parse::<i64>() {
                Ok(n) => Ok(Value::Int(n)),
                Err(_) => text
                    .parse::<num_bigint::BigInt>()
                    .map(|b| h.new_bigint(b))
                    .map_err(|_| "invalid number in JSON".to_string()),
            }
        }
    }
}

/// The strict numeric hook: fusevm calls this when a native arithmetic or
/// comparison op has a non-`Int`/`Float` operand. For a user object it dispatches
/// to the operator method (`def +`, `def <=>`, …) or derives a comparison from a
/// Comparable `<=>`; otherwise it defers to the host's String/Array semantics.
/// Uses fine-grained borrows so a user operator method can re-enter the VM.
pub fn numeric_hook(op: fusevm::NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    use fusevm::NumOp::*;
    if let Some(cls) = with_host(|h| h.object_class(a)) {
        let name = num_op_method(op);
        if !name.is_empty() && with_host(|h| h.find_method_owner(&cls, name)).is_some() {
            return call_instance_method(a.clone(), &cls, name, std::slice::from_ref(b), None);
        }
        // Comparable: derive `< > <= >= == !=` from a `<=>` method.
        if matches!(op, Lt | Gt | Le | Ge | Eq | Ne)
            && with_host(|h| h.find_method_owner(&cls, "<=>")).is_some()
        {
            let cmp = call_instance_method(a.clone(), &cls, "<=>", std::slice::from_ref(b), None)?;
            let c = as_i(&cmp);
            return Ok(Value::Bool(match op {
                Lt => c < 0,
                Gt => c > 0,
                Le => c <= 0,
                Ge => c >= 0,
                Eq => c == 0,
                Ne => c != 0,
                _ => false,
            }));
        }
    }
    with_host(|h| h.num_op(op, a, b))
}

/// The method name for an operator `NumOp`, or `""` if it has none.
fn num_op_method(op: fusevm::NumOp) -> &'static str {
    use fusevm::NumOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Mod => "%",
        Pow => "**",
        Lt => "<",
        Gt => ">",
        Le => "<=",
        Ge => ">=",
        Eq => "==",
        Ne => "!=",
        _ => "",
    }
}

/// Raise a typed exception: register the exception object (so `rescue Class` can
/// match it) and return the message as the `Err` payload.
/// Which of `require` / `require_relative` / `load` a call is.
#[derive(Clone, Copy, PartialEq)]
enum ReqMode {
    Require,
    Relative,
    Load,
}

/// Standard-library names the runtime provides natively (or intentionally
/// ignores), so `require '<name>'` is a no-op returning true rather than a file
/// search. These never map to a `.rb` on disk.
/// Pure-Ruby stdlib libraries bundled into the binary, keyed by `require` name.
/// Each is compiled and run on the host the first time it is required, so the
/// installed `ruby` needs no external stdlib directory. Kept out of
/// `is_builtin_lib` so `require` actually loads them instead of no-opping.
pub(crate) fn embedded_stdlib(name: &str) -> Option<&'static str> {
    let n = name.strip_suffix(".rb").unwrap_or(name);
    match n {
        "uri" => Some(include_str!("../stdlib/uri.rb")),
        "forwardable" => Some(include_str!("../stdlib/forwardable.rb")),
        "delegate" => Some(include_str!("../stdlib/delegate.rb")),
        "rubygems/version" => Some(include_str!("../stdlib/rubygems_version.rb")),
        "csv" => Some(include_str!("../stdlib/csv.rb")),
        "optparse" => Some(include_str!("../stdlib/optparse.rb")),
        "yaml" | "psych" => Some(include_str!("../stdlib/yaml.rb")),
        _ => None,
    }
}

pub(crate) fn is_builtin_lib(name: &str) -> bool {
    let n = name.strip_suffix(".rb").unwrap_or(name);
    matches!(
        n,
        "set"
            | "json"
            | "date"
            | "time"
            | "securerandom"
            | "digest"
            | "openssl"
            | "ipaddr"
            | "base64"
            | "bigdecimal"
            | "bigdecimal/util"
            | "pp"
            | "prettyprint"
            | "ostruct"
            | "singleton"
            | "comparable"
            | "enumerable"
            | "benchmark"
            | "stringio"
            | "strscan"
            | "pathname"
            | "fileutils"
            | "tmpdir"
            | "tempfile"
            // `uri`, `csv`, `yaml`, `optparse` are real pure-Ruby libs bundled via
            // `embedded_stdlib`, not no-op names — kept out of this list on purpose.
            | "cgi"
            | "cgi/escape"
            | "cgi/util"
            | "timeout"
            | "erb"
            | "logger"
            | "open3"
            | "etc"
            | "socket"
            | "io/console"
            | "abbrev"
            | "sqlite3"
            | "fiddle"
            // Core libs that are no-ops in modern Ruby (their classes are built
            // in) or that gems require defensively.
            | "thread"
            | "fiber"
            | "zlib"
            | "monitor"
            | "mutex_m"
            | "weakref"
            | "English"
            | "did_you_mean"
            | "rbconfig"
            | "ripper"
            | "objspace"
            | "date/format"
    )
}

/// A path exists as a regular file: return its canonical (absolute) form.
fn try_file(p: &std::path::Path) -> Option<std::path::PathBuf> {
    if p.is_file() {
        std::fs::canonicalize(p).ok()
    } else {
        None
    }
}

/// The current `$LOAD_PATH` entries as strings (reading the `$LOAD_PATH` alias,
/// falling back to `$:`).
fn load_path_dirs() -> Vec<String> {
    with_host(|h| {
        let lp = match h.get_global("LOAD_PATH") {
            Value::Undef => h.get_global(":"),
            v => v,
        };
        h.as_array(&lp)
            .map(|xs| xs.iter().filter_map(|v| h.as_str(v)).collect())
            .unwrap_or_default()
    })
}

/// Try `base/raw` and, unless it already ends in `.rb`, `base/raw.rb`. Shared by
/// the runtime resolvers and the build-time bundler (`bundle.rs`), so a required
/// path resolves identically whether it is loaded at run time or bundled ahead of
/// time.
pub(crate) fn resolve_in(base: &std::path::Path, raw: &str) -> Option<std::path::PathBuf> {
    if let Some(f) = try_file(&base.join(raw)) {
        return Some(f);
    }
    if !raw.ends_with(".rb") {
        if let Some(f) = try_file(&base.join(format!("{raw}.rb"))) {
            return Some(f);
        }
    }
    None
}

/// Core of `require` resolution over an explicit, ordered search-dir list
/// (host-independent, so the build-time bundler can reuse it with the entrypoint
/// dir + cwd in place of a live `$LOAD_PATH`): an absolute path (with/without
/// `.rb`), else each dir in order, else the current directory.
pub(crate) fn resolve_require_in(
    dirs: &[std::path::PathBuf],
    raw: &str,
) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(raw);
    if p.is_absolute() {
        if let Some(f) = try_file(p) {
            return Some(f);
        }
        if !raw.ends_with(".rb") {
            return try_file(std::path::Path::new(&format!("{raw}.rb")));
        }
        return None;
    }
    for dir in dirs {
        if let Some(f) = resolve_in(dir, raw) {
            return Some(f);
        }
    }
    resolve_in(std::path::Path::new("."), raw)
}

/// `require` resolution against the live `$LOAD_PATH`, else the current directory.
fn resolve_require(raw: &str) -> Option<std::path::PathBuf> {
    let dirs: Vec<std::path::PathBuf> = load_path_dirs()
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect();
    resolve_require_in(&dirs, raw)
}

/// `require_relative` resolution: relative to the requiring file's directory
/// (the top of the file-dir stack), else the current directory.
fn resolve_relative(raw: &str) -> Option<std::path::PathBuf> {
    let base = crate::host::current_file_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    resolve_in(&base, raw)
}

/// `load` resolution: an explicit path (absolute or `./`/`../`) relative to cwd
/// or the current file's dir, else searched in `$LOAD_PATH`, then cwd. `load`
/// never appends `.rb` — the name must include its extension.
fn resolve_load(raw: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(raw);
    if p.is_absolute() || raw.starts_with("./") || raw.starts_with("../") {
        return try_file(p)
            .or_else(|| crate::host::current_file_dir().and_then(|d| try_file(&d.join(raw))));
    }
    for dir in load_path_dirs() {
        if let Some(f) = try_file(&std::path::Path::new(&dir).join(raw)) {
            return Some(f);
        }
    }
    try_file(p)
}

/// Whether `$LOADED_FEATURES` already contains `abs`.
fn feature_loaded(abs: &str) -> bool {
    with_host(|h| {
        h.as_array(&h.get_global("LOADED_FEATURES"))
            .map(|xs| xs.iter().any(|v| h.as_str(v).as_deref() == Some(abs)))
            .unwrap_or(false)
    })
}

/// Append `abs` to the `$LOADED_FEATURES` Array (in place, so the `$"` alias
/// sees it too).
fn record_feature(abs: &str) {
    with_host(|h| {
        let lf = h.get_global("LOADED_FEATURES");
        if let Some(mut xs) = h.as_array(&lf) {
            let s = h.new_string(abs.to_string());
            xs.push(s);
            h.set_array(&lf, xs);
        }
    });
}

/// Implement `require` / `require_relative` / `load`: resolve the path, dedup
/// (`require`/`require_relative` only), read + compile, merge onto the host
/// (rebasing proc/begin ids), and run the file's top level in its own scope
/// while tracking its directory for nested `require_relative`.
fn do_require(args: &[Value], mode: ReqMode) -> Result<Value, String> {
    let raw = match args.first() {
        Some(v) => with_host(|h| h.as_str(v)).unwrap_or_else(|| with_host(|h| h.to_s(v))),
        None => {
            return Err(raise_exc(
                "ArgumentError",
                "wrong number of arguments (given 0, expected 1)",
            ))
        }
    };

    // A pure-Ruby stdlib bundled into the binary (uri, csv, optparse, yaml, …):
    // compile+run its embedded source once, so a bare `require "uri"` works with
    // no external file — the installed `ruby` stays self-contained.
    if mode == ReqMode::Require {
        if let Some(src) = embedded_stdlib(&raw) {
            let feature = format!("<embedded>/{raw}.rb");
            if feature_loaded(&feature) {
                return Ok(Value::Bool(false));
            }
            record_feature(&feature);
            let prog = crate::compile(src).map_err(|e| raise_exc("SyntaxError", &e))?;
            let main = crate::load_merged(prog);
            crate::host::run_required_main(main)?;
            return Ok(Value::Bool(true));
        }
    }

    // A known builtin library name is a no-op that reports success.
    if mode == ReqMode::Require && is_builtin_lib(&raw) {
        return Ok(Value::Bool(true));
    }

    let abs = match mode {
        ReqMode::Require => resolve_require(&raw),
        ReqMode::Relative => resolve_relative(&raw),
        ReqMode::Load => resolve_load(&raw),
    }
    .ok_or_else(|| raise_exc("LoadError", &format!("cannot load such file -- {raw}")))?;
    let abs_str = abs.to_string_lossy().to_string();

    // `require`/`require_relative` dedup on the absolute path; `load` never does.
    if mode != ReqMode::Load && feature_loaded(&abs_str) {
        return Ok(Value::Bool(false));
    }

    let src = std::fs::read_to_string(&abs).map_err(|e| {
        raise_exc(
            "LoadError",
            &format!("cannot load such file -- {abs_str} ({e})"),
        )
    })?;
    let prog =
        crate::compile(&src).map_err(|e| raise_exc("SyntaxError", &format!("{abs_str}: {e}")))?;

    // Record the feature before running the body so a circular `require` sees it
    // already loaded and returns false instead of recursing (MRI behavior).
    if mode != ReqMode::Load {
        record_feature(&abs_str);
    }

    let main = crate::load_merged(prog);
    let dir = abs
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    crate::host::push_file_dir(dir);
    // `__FILE__` inside a required file is that file's own (absolute) path.
    crate::host::push_file_path(abs_str.clone());
    let r = crate::host::run_required_main(main);
    crate::host::pop_file_dir();
    r?;
    Ok(Value::Bool(true))
}

// ---- Array#pack / String#unpack -----------------------------------------
//
// Ruby strings here are UTF-8-backed (`RObj::Str` is a Rust `String`), so a true
// ASCII-8BIT/binary string is modeled with the Latin-1 convention: a "byte" is a
// codepoint in `U+0000..=U+00FF` and its value is the char's low 8 bits. `pack`
// turns bytes into such a string and `unpack` reads it back the same way, so any
// `pack`-produced binary string round-trips (`bytes.pack("C*").unpack("C*")`),
// and `Integer#chr` (which maps `n & 0xff` to `U+00nn`) round-trips through
// `unpack("C*")` too. The documented divergence: `unpack` on a *genuine*
// multibyte-UTF-8 text string reads codepoints (Latin-1), not the raw UTF-8
// bytes MRI would — e.g. `"é".unpack("C*")` is `[233]` here vs `[195, 169]` in
// MRI. `String#bytes`/`#ord` keep their real-UTF-8 semantics, so `255.chr.bytes`
// is `[195, 191]` here vs `[255]` in MRI (a pack-free path). For ASCII and every
// pack-produced binary string the two models coincide.

/// Encode a byte slice as a Latin-1 binary string (`byte b` -> `char U+00xx`).
fn bytes_to_binstr(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Decode a binary string to its byte sequence (each codepoint's low 8 bits).
fn binstr_to_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| (c as u32 & 0xff) as u8).collect()
}

/// A parsed pack/unpack directive: the type char and its count (`Some(n)`, or
/// `None` for the `*` "all/rest" form; a bare directive is `Some(1)`).
struct PackDir {
    kind: char,
    count: Option<usize>,
}

/// Parse a pack/unpack template into directives, ignoring whitespace (MRI does).
fn parse_pack_template(fmt: &str) -> Vec<PackDir> {
    let mut dirs = Vec::new();
    let mut it = fmt.chars().peekable();
    while let Some(k) = it.next() {
        if k.is_whitespace() {
            continue;
        }
        let count = match it.peek() {
            Some('*') => {
                it.next();
                None
            }
            Some(d) if d.is_ascii_digit() => {
                let mut n = 0usize;
                while let Some(d) = it.peek().copied() {
                    if let Some(v) = d.to_digit(10) {
                        n = n * 10 + v as usize;
                        it.next();
                    } else {
                        break;
                    }
                }
                Some(n)
            }
            _ => Some(1),
        };
        dirs.push(PackDir { kind: k, count });
    }
    dirs
}

/// `Array#pack` core: consume `items` per the template, producing raw bytes.
fn pack_bytes(items: &[Value], fmt: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut idx = 0usize; // next array element to consume
    for d in parse_pack_template(fmt) {
        match d.kind {
            // Integer bytes: C (unsigned) / c (signed) are identical at the byte
            // level. `*` consumes every remaining element.
            'C' | 'c' => {
                let n = d.count.unwrap_or(items.len().saturating_sub(idx));
                for _ in 0..n {
                    let v = items.get(idx).map(as_i).unwrap_or(0);
                    out.push((v & 0xff) as u8);
                    idx += 1;
                }
            }
            // A string: `a` NUL-pads (and NUL is the truncation fill), `A` space-
            // pads. `*` uses the full string; a count truncates/pads to width.
            'a' | 'A' => {
                let sbytes = items
                    .get(idx)
                    .map(|v| binstr_to_bytes(&arg_str(v)))
                    .unwrap_or_default();
                idx += 1;
                let pad = if d.kind == 'A' { b' ' } else { 0u8 };
                match d.count {
                    None => out.extend_from_slice(&sbytes),
                    Some(n) => {
                        for i in 0..n {
                            out.push(sbytes.get(i).copied().unwrap_or(pad));
                        }
                    }
                }
            }
            // Fixed-width integers, big-endian (N/n) and little-endian (V/v).
            'N' | 'n' | 'V' | 'v' => {
                let width = if matches!(d.kind, 'N' | 'V') { 4 } else { 2 };
                let little = matches!(d.kind, 'V' | 'v');
                let n = d.count.unwrap_or(items.len().saturating_sub(idx));
                for _ in 0..n {
                    let v = items.get(idx).map(as_i).unwrap_or(0) as u64;
                    idx += 1;
                    let full = v.to_be_bytes();
                    let mut w: Vec<u8> = full[8 - width..].to_vec();
                    if little {
                        w.reverse();
                    }
                    out.extend_from_slice(&w);
                }
            }
            // Hex strings: `H` high-nibble-first, `h` low-nibble-first. Consumes
            // one element; a count limits the nibbles taken (`*` = all).
            'H' | 'h' => {
                let hex = items.get(idx).map(arg_str).unwrap_or_default();
                idx += 1;
                let nibbles: Vec<u8> = hex
                    .chars()
                    .map(|c| c.to_digit(16).unwrap_or(0) as u8)
                    .collect();
                let take = d.count.unwrap_or(nibbles.len()).min(nibbles.len());
                let mut i = 0;
                while i < take {
                    let hi = nibbles[i];
                    let lo = if i + 1 < take { nibbles[i + 1] } else { 0 };
                    let byte = if d.kind == 'H' {
                        (hi << 4) | lo
                    } else {
                        (lo << 4) | hi
                    };
                    out.push(byte);
                    i += 2;
                }
            }
            other => {
                return Err(raise_exc(
                    "ArgumentError",
                    &format!("unsupported pack directive '{other}'"),
                ))
            }
        }
    }
    Ok(out)
}

/// `String#unpack` core: read `bytes` per the template into Ruby values.
fn unpack_bytes(bytes: &[u8], fmt: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for d in parse_pack_template(fmt) {
        match d.kind {
            'C' | 'c' => {
                let n = d.count.unwrap_or(bytes.len().saturating_sub(pos));
                for _ in 0..n {
                    match bytes.get(pos) {
                        Some(&b) => {
                            let v = if d.kind == 'c' {
                                b as i8 as i64
                            } else {
                                b as i64
                            };
                            out.push(Value::Int(v));
                            pos += 1;
                        }
                        None => out.push(Value::Undef),
                    }
                }
            }
            // `a` keeps everything (including trailing NULs); `A` strips trailing
            // NULs and spaces. Yields one string element.
            'a' | 'A' => {
                let n = d.count.unwrap_or(bytes.len().saturating_sub(pos));
                let end = (pos + n).min(bytes.len());
                let slice = &bytes[pos..end];
                pos = end;
                let s = if d.kind == 'A' {
                    let t: &[u8] = {
                        let mut e = slice.len();
                        while e > 0 && (slice[e - 1] == 0 || slice[e - 1] == b' ') {
                            e -= 1;
                        }
                        &slice[..e]
                    };
                    bytes_to_binstr(t)
                } else {
                    bytes_to_binstr(slice)
                };
                out.push(new_str(s));
            }
            'N' | 'n' | 'V' | 'v' => {
                let width = if matches!(d.kind, 'N' | 'V') { 4 } else { 2 };
                let little = matches!(d.kind, 'V' | 'v');
                let n = d.count.unwrap_or(usize::MAX);
                let mut produced = 0;
                while produced < n && pos + width <= bytes.len() {
                    let mut buf = [0u8; 8];
                    let chunk = &bytes[pos..pos + width];
                    if little {
                        for (i, &b) in chunk.iter().enumerate() {
                            buf[i] = b;
                        }
                        out.push(Value::Int(u64::from_le_bytes(buf) as i64));
                    } else {
                        for (i, &b) in chunk.iter().enumerate() {
                            buf[8 - width + i] = b;
                        }
                        out.push(Value::Int(u64::from_be_bytes(buf) as i64));
                    }
                    pos += width;
                    produced += 1;
                }
                // A fixed count past the end yields nils (MRI behavior).
                if d.count.is_some() {
                    for _ in produced..n {
                        out.push(Value::Undef);
                    }
                }
            }
            'H' | 'h' => {
                let rest = &bytes[pos.min(bytes.len())..];
                let avail = rest.len() * 2;
                let take = d.count.unwrap_or(avail).min(avail);
                let mut s = String::with_capacity(take);
                for i in 0..take {
                    let byte = rest[i / 2];
                    let nib = if d.kind == 'H' {
                        if i % 2 == 0 {
                            byte >> 4
                        } else {
                            byte & 0x0f
                        }
                    } else if i % 2 == 0 {
                        byte & 0x0f
                    } else {
                        byte >> 4
                    };
                    s.push(std::char::from_digit(nib as u32, 16).unwrap());
                }
                pos += take.div_ceil(2);
                out.push(new_str(s));
            }
            other => {
                return Err(raise_exc(
                    "ArgumentError",
                    &format!("unsupported unpack directive '{other}'"),
                ))
            }
        }
    }
    Ok(out)
}

// ---- StringIO ------------------------------------------------------------
//
// `require "stringio"` is a no-op (the class is builtin). A StringIO is a plain
// object of class `StringIO` whose `buf` ivar holds the accumulated String and
// whose `pos` ivar is the read cursor (a byte offset). Writes append to `buf`
// (the common output/log-sink and input-buffer patterns only ever append or
// read); `read`/`gets` advance `pos`.

/// The current buffer string and read cursor of a StringIO receiver.
fn stringio_state(recv: &Value) -> (String, usize) {
    with_host(|h| {
        let buf = match h.ivar_of(recv, "buf") {
            Value::Undef => String::new(),
            v => h.as_str(&v).unwrap_or_default(),
        };
        let pos = match h.ivar_of(recv, "pos") {
            Value::Int(n) => n.max(0) as usize,
            _ => 0,
        };
        (buf, pos)
    })
}

fn stringio_set_buf(recv: &Value, buf: String) {
    with_host(|h| {
        let sv = h.new_string(buf);
        h.set_ivar_of(recv, "buf", sv);
    });
}

fn stringio_set_pos(recv: &Value, pos: usize) {
    with_host(|h| h.set_ivar_of(recv, "pos", Value::Int(pos as i64)));
}

/// The lines remaining from the read cursor to EOF, each keeping its trailing
/// `\n` (MRI line semantics). Does not move the cursor — callers that consume
/// (`readlines`) advance it themselves.
fn stringio_rest_lines(recv: &Value) -> Vec<Value> {
    let (buf, pos) = stringio_state(recv);
    let rest = String::from_utf8_lossy(&buf.as_bytes()[pos.min(buf.len())..]).into_owned();
    split_lines(&rest).into_iter().map(new_str).collect()
}

fn stringio_method(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        // The internal buffer String (the accumulated content).
        "string" => Ok(with_host(|h| h.ivar_of(recv, "buf"))),
        // Append: `write` returns the byte count written, `<<` returns self.
        "write" | "<<" | "print" => {
            let (mut buf, _) = stringio_state(recv);
            let mut written = 0usize;
            for a in args {
                let s = with_host(|h| h.to_s(a));
                written += s.len();
                buf.push_str(&s);
            }
            let end = buf.len();
            stringio_set_buf(recv, buf);
            stringio_set_pos(recv, end);
            match name {
                "<<" => Ok(recv.clone()),
                "print" => Ok(Value::Undef),
                _ => Ok(Value::Int(written as i64)),
            }
        }
        // `puts` appends each argument followed by a newline (unless it already
        // ends in one); no arguments appends a bare newline.
        "puts" => {
            let (mut buf, _) = stringio_state(recv);
            if args.is_empty() {
                buf.push('\n');
            } else {
                for a in args {
                    let s = with_host(|h| h.to_s(a));
                    buf.push_str(&s);
                    if !s.ends_with('\n') {
                        buf.push('\n');
                    }
                }
            }
            let end = buf.len();
            stringio_set_buf(recv, buf);
            stringio_set_pos(recv, end);
            Ok(Value::Undef)
        }
        // `read([len])` — from the cursor. No arg reads the rest (as `""` at EOF);
        // a length reads that many bytes and returns nil once past the end.
        "read" => {
            let (buf, pos) = stringio_state(recv);
            let bytes = buf.as_bytes();
            match args.first() {
                None => {
                    let out = String::from_utf8_lossy(&bytes[pos.min(bytes.len())..]).into_owned();
                    stringio_set_pos(recv, bytes.len());
                    Ok(new_str(out))
                }
                Some(a) => {
                    let len = as_i(a).max(0) as usize;
                    if pos >= bytes.len() && len > 0 {
                        return Ok(Value::Undef);
                    }
                    let end = (pos + len).min(bytes.len());
                    let out = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
                    stringio_set_pos(recv, end);
                    Ok(new_str(out))
                }
            }
        }
        // `gets` — the next line (through and including the `\n`), nil at EOF.
        "gets" => {
            let (buf, pos) = stringio_state(recv);
            let bytes = buf.as_bytes();
            if pos >= bytes.len() {
                return Ok(Value::Undef);
            }
            let nl = bytes[pos..].iter().position(|&b| b == b'\n');
            let end = match nl {
                Some(i) => pos + i + 1,
                None => bytes.len(),
            };
            let out = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
            stringio_set_pos(recv, end);
            Ok(new_str(out))
        }
        // `each_line` / `each` — with a block, yield each remaining line (through
        // its `\n`) and return self; without one, an Enumerator over those lines.
        "each_line" | "each" => {
            let Some(b) = &block else {
                return Ok(with_host(|h| {
                    h.new_enumerator(stringio_rest_lines(recv), "each")
                }));
            };
            loop {
                let line = stringio_method(recv, "gets", &[], None)?;
                if matches!(line, Value::Undef) {
                    break;
                }
                call_proc(b, &[line])?;
            }
            Ok(recv.clone())
        }
        // `readlines` / `to_a` — all remaining lines as an Array; consumes to EOF.
        "readlines" | "to_a" => {
            let lines = stringio_rest_lines(recv);
            let (buf, _) = stringio_state(recv);
            stringio_set_pos(recv, buf.len());
            Ok(new_arr(lines))
        }
        // `getc` — the next single character, advancing the cursor; nil at EOF.
        "getc" => {
            let (buf, pos) = stringio_state(recv);
            let bytes = buf.as_bytes();
            let rest = String::from_utf8_lossy(&bytes[pos.min(bytes.len())..]);
            match rest.chars().next() {
                None => Ok(Value::Undef),
                Some(ch) => {
                    stringio_set_pos(recv, pos + ch.len_utf8());
                    Ok(new_str(ch.to_string()))
                }
            }
        }
        "rewind" => {
            stringio_set_pos(recv, 0);
            Ok(Value::Int(0))
        }
        "pos" | "tell" => Ok(Value::Int(stringio_state(recv).1 as i64)),
        "pos=" | "seek" => {
            let p = as_i(&args[0]).max(0) as usize;
            stringio_set_pos(recv, p);
            Ok(Value::Int(p as i64))
        }
        "eof?" | "eof" => {
            let (buf, pos) = stringio_state(recv);
            Ok(Value::Bool(pos >= buf.len()))
        }
        "size" | "length" => Ok(Value::Int(stringio_state(recv).0.len() as i64)),
        "rewind!" | "close" | "flush" | "fsync" => Ok(Value::Undef),
        "to_s" => Ok(with_host(|h| h.ivar_of(recv, "buf"))),
        _ => Err(no_method_error(recv, name)),
    }
}

pub(crate) fn raise_exc(class: &str, msg: &str) -> String {
    let exc = with_host(|h| h.new_exception(class, msg));
    with_host(|h| h.set_pending_exc(exc));
    msg.to_string()
}

/// Raise `FrozenError` when `name` is a mutating method (any `!`-suffixed method,
/// or one of the explicit `mutators`) and `recv` is frozen — MRI's
/// `can't modify frozen <Class>: <inspect>`. A no-op for unfrozen receivers, so
/// non-frozen dispatch is unaffected.
fn frozen_guard(recv: &Value, name: &str, mutators: &[&str]) -> Result<(), String> {
    let is_mutator = (name.len() > 1 && name.ends_with('!')) || mutators.contains(&name);
    if is_mutator && with_host(|h| h.is_frozen(recv)) {
        let (cls, insp) = with_host(|h| (h.class_of(recv).to_string(), h.inspect(recv)));
        return Err(raise_exc(
            "FrozenError",
            &format!("can't modify frozen {cls}: {insp}"),
        ));
    }
    Ok(())
}

/// Explicit (non-`!`) String mutators guarded against a frozen receiver.
const STRING_MUTATORS: &[&str] = &[
    "<<",
    "concat",
    "replace",
    "clear",
    "insert",
    "prepend",
    "[]=",
    "force_encoding",
];

/// Explicit (non-`!`) Array mutators guarded against a frozen receiver.
const ARRAY_MUTATORS: &[&str] = &[
    "<<",
    "push",
    "append",
    "pop",
    "shift",
    "unshift",
    "prepend",
    "concat",
    "insert",
    "delete",
    "delete_at",
    "clear",
    "replace",
    "fill",
    "[]=",
    "store",
];

/// Explicit (non-`!`) Hash mutators guarded against a frozen receiver.
const HASH_MUTATORS: &[&str] = &[
    "[]=", "store", "delete", "clear", "replace", "update", "merge!",
];

/// Raise a `NoMethodError` with the ruby-4.0 message form for `recv`:
/// `for nil` / `for true` / `for false`, `for class C` when the receiver is a
/// class/module reference, or `for an instance of C` for every other value.
fn no_method_error(recv: &Value, name: &str) -> String {
    let target = match recv {
        Value::Undef => "nil".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        _ => match with_host(|h| h.classref_name(recv)) {
            Some(c) => format!("class {c}"),
            None => format!("an instance of {}", with_host(|h| h.class_of(recv))),
        },
    };
    raise_exc(
        "NoMethodError",
        &format!("undefined method '{name}' for {target}"),
    )
}

/// Range/Set/Enumerator delegate unknown methods to `Array` (a synthesized
/// buffer). If the method is missing there too, rewrite the resulting
/// `NoMethodError` so it names the original receiver's class rather than
/// `Array`.
fn remap_array_delegate(
    r: Result<Value, String>,
    recv: &Value,
    name: &str,
) -> Result<Value, String> {
    r.map_err(|e| {
        if e.starts_with("undefined method") && e.contains("an instance of Array") {
            no_method_error(recv, name)
        } else {
            e
        }
    })
}

/// The display string of a value — a user object's own `to_s` if it defines one,
/// otherwise the host's default. Used by `puts`/`print`/interpolation.
fn display(v: &Value) -> String {
    if let Some(cls) = with_host(|h| h.object_class(v)) {
        if with_host(|h| h.find_method(&cls, "to_s")).is_some() {
            if let Ok(s) = dispatch(v, "to_s", &[], None) {
                return with_host(|h| h.as_str(&s).unwrap_or_default());
            }
        }
    }
    with_host(|h| h.to_s(v))
}
fn new_arr(items: Vec<Value>) -> Value {
    with_host(|h| h.new_array(items))
}

/// All `n`-element combinations of `arr` in MRI order (increasing indices).
/// `n < 0` yields none; `n == 0` yields a single empty combination; `n > len`
/// yields none.
fn combinations(arr: &[Value], n: i64) -> Vec<Vec<Value>> {
    if n < 0 || n as usize > arr.len() {
        return Vec::new();
    }
    let n = n as usize;
    let mut out = Vec::new();
    let mut idx: Vec<usize> = (0..n).collect();
    if n == 0 {
        return vec![Vec::new()];
    }
    loop {
        out.push(idx.iter().map(|&i| arr[i].clone()).collect());
        // Advance the odometer: find rightmost index that can still move.
        let mut i = n;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if idx[i] != i + arr.len() - n {
                break;
            }
        }
        idx[i] += 1;
        for j in i + 1..n {
            idx[j] = idx[j - 1] + 1;
        }
    }
}

/// All `n`-length permutations of `arr` in MRI order (element-index order,
/// no repeats). `n < 0` or `n > len` yields none; `n == 0` yields one empty
/// permutation.
fn permutations(arr: &[Value], n: i64) -> Vec<Vec<Value>> {
    if n < 0 || n as usize > arr.len() {
        return Vec::new();
    }
    let n = n as usize;
    let mut out = Vec::new();
    let mut used = vec![false; arr.len()];
    let mut cur: Vec<Value> = Vec::with_capacity(n);
    permute_rec(arr, n, &mut used, &mut cur, &mut out);
    out
}

fn permute_rec(
    arr: &[Value],
    n: usize,
    used: &mut [bool],
    cur: &mut Vec<Value>,
    out: &mut Vec<Vec<Value>>,
) {
    if cur.len() == n {
        out.push(cur.clone());
        return;
    }
    for i in 0..arr.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        cur.push(arr[i].clone());
        permute_rec(arr, n, used, cur, out);
        cur.pop();
        used[i] = false;
    }
}
fn arg_str(v: &Value) -> String {
    with_host(|h| {
        h.as_str(v)
            .or_else(|| h.as_symbol(v))
            .unwrap_or_else(|| h.to_s(v))
    })
}

/// Ruby `String#to_i`: the leading integer prefix, or 0.
/// Scan a leading Ruby-style integer from `s` in the given `base`.
///
/// `base == 0` auto-detects the radix from a `0x`/`0b`/`0o`/`0d` prefix or a
/// bare leading `0` (octal), defaulting to decimal. Otherwise `base` is
/// 2..=36 and a matching prefix (`0x` for 16, `0b` for 2, `0o` for 8, `0d`
/// for 10) is consumed if present. A leading sign and single underscores
/// between digits are honoured. Returns `(value, byte_offset_past_number)`
/// or `None` when no digit was consumed. Shared by `String#to_i`, `#hex`,
/// `#oct`, and `Kernel#Integer`.
fn scan_int(s: &str, base: i64) -> Option<(i64, usize)> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < n && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let prefix_base = |c: u8| -> Option<i64> {
        match c {
            b'x' | b'X' => Some(16),
            b'b' | b'B' => Some(2),
            b'o' | b'O' => Some(8),
            b'd' | b'D' => Some(10),
            _ => None,
        }
    };
    let mut radix = base;
    // Explicit `0x`-style prefix (auto-detect, or when it matches `base`).
    if i + 1 < n && bytes[i] == b'0' {
        if let Some(pb) = prefix_base(bytes[i + 1]) {
            if base == 0 || base == pb {
                radix = pb;
                i += 2;
            }
        }
    }
    let mut val: i64 = 0;
    let mut count = 0usize;
    let mut prev_digit = false;
    // Base 0 with no `0x`-style prefix: a bare leading `0` selects octal and
    // is itself the first (zero-valued) digit; anything else is decimal.
    if radix == 0 {
        if i < n && bytes[i] == b'0' {
            radix = 8;
            i += 1;
            count = 1;
            prev_digit = true;
        } else {
            radix = 10;
        }
    }
    let r = radix as u32;
    while i < n {
        let c = bytes[i] as char;
        if c == '_' {
            if prev_digit && i + 1 < n && (bytes[i + 1] as char).is_digit(r) {
                i += 1;
                prev_digit = false;
                continue;
            }
            break;
        }
        match c.to_digit(r) {
            Some(d) => {
                val = val.saturating_mul(radix).saturating_add(d as i64);
                count += 1;
                i += 1;
                prev_digit = true;
            }
            None => break,
        }
    }
    if count == 0 {
        return None;
    }
    Some((if neg { -val } else { val }, i))
}

/// Like `scan_int`, but returns a full-precision Integer `Value` — promoting to a
/// BigInt when the parsed number exceeds the `i64` range (`"0x8000000000000000"`,
/// large decimals). `new_bigint` demotes back to an `i64` immediate when it fits.
fn scan_int_value(s: &str, base: i64) -> Option<Value> {
    // Reuse `scan_int` for the prefix/sign/radix logic and the end position, then
    // re-parse the exact consumed slice with arbitrary precision. Detect the radix
    // by re-running the same prefix detection on the trimmed input.
    let (_, end) = scan_int(s, base)?;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < end && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let neg = i < end && bytes[i] == b'-';
    if i < end && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut radix = if (2..=36).contains(&base) { base as u32 } else { 10 };
    if i + 1 < end && bytes[i] == b'0' {
        let pb = match bytes[i + 1] {
            b'x' | b'X' => Some(16),
            b'b' | b'B' => Some(2),
            b'o' | b'O' => Some(8),
            b'd' | b'D' => Some(10),
            _ => None,
        };
        if let Some(pb) = pb {
            if base == 0 || base == pb {
                radix = pb as u32;
                i += 2;
            }
        } else if base == 0 {
            radix = 8; // bare leading 0 → octal
        }
    } else if base == 0 {
        radix = 10;
    }
    let digits: String = s[i..end].chars().filter(|c| *c != '_').collect();
    let big = num_bigint::BigInt::parse_bytes(digits.as_bytes(), radix)?;
    let big = if neg { -big } else { big };
    Some(with_host(|h| h.new_bigint(big)))
}

/// True when `s` (after leading whitespace and an optional sign) opens with a
/// `0x`/`0b`/`0o`/`0d` radix prefix. Used by `String#oct` to switch from its
/// default base 8 into prefix auto-detect.
fn has_radix_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    i + 1 < b.len()
        && b[i] == b'0'
        && matches!(
            b[i + 1],
            b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
        )
}

/// Ruby `String#to_f`: the leading float prefix, or 0.0.
fn parse_leading_float(s: &str) -> f64 {
    let t = s.trim();
    let mut buf = String::new();
    let mut seen_dot = false;
    for (i, c) in t.chars().enumerate() {
        if c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')) {
            buf.push(c);
        } else if c == '.' && !seen_dot {
            seen_dot = true;
            buf.push(c);
        } else {
            break;
        }
    }
    buf.parse().unwrap_or(0.0)
}

/// Resolve a range `lo..hi` to a `[start, end)` half-open slice range over a
/// collection of `len` elements, honoring negative indices, exclusivity, and the
/// beginless/endless sentinels. Returns `None` when the start is out of range.
fn range_bounds(lo: i64, hi: i64, excl: bool, len: usize) -> Option<(usize, usize)> {
    use crate::host::{RANGE_BEGINLESS, RANGE_ENDLESS};
    let start = if lo == RANGE_BEGINLESS {
        0
    } else {
        norm_idx(lo, len)?
    };
    if start > len {
        return None;
    }
    let end = if hi == RANGE_ENDLESS {
        len
    } else {
        // A negative endpoint that underflows past index 0 (`|hi| > len`) points
        // before the string/array start, so the slice is empty — end collapses to
        // `start` below. (`"foo"[1...-4]` is `""`, not `"oo"`.)
        match norm_idx(hi, len) {
            Some(mut e) => {
                if !excl {
                    e += 1;
                }
                e.min(len)
            }
            None => 0,
        }
    };
    Some((start, end.max(start)))
}

fn norm_idx(i: i64, len: usize) -> Option<usize> {
    if i >= 0 {
        Some(i as usize)
    } else {
        let n = len as i64 + i;
        if n >= 0 {
            Some(n as usize)
        } else {
            None
        }
    }
}

fn add_values(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Value::Int(x + y),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            Value::Float(as_f(a) + as_f(b))
        }
        // Object operands (Rational, BigInt, String, Array, …) add through host
        // dispatch so `sum`/`reduce(:+)` stay exact and type-correct.
        _ => with_host(|h| h.num_op(fusevm::NumOp::Add, a, b))
            .unwrap_or_else(|_| Value::Float(as_f(a) + as_f(b))),
    }
}

fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // A user object compares through its `<=>` method (Comparable).
    if with_host(|h| h.object_class(a)).is_some() {
        if let Ok(v) = dispatch(a, "<=>", std::slice::from_ref(b), None) {
            return as_i(&v).cmp(&0);
        }
    }
    // Two Times order by their epoch seconds.
    if let (Some(x), Some(y)) = with_host(|h| (h.time_secs(a), h.time_secs(b))) {
        return x.total_cmp(&y);
    }
    // Two Dates order by their day count.
    if let (Some(x), Some(y)) = with_host(|h| (h.date_days(a), h.date_days(b))) {
        return x.cmp(&y);
    }
    // Two DateTimes order by their epoch seconds.
    if let (Some(x), Some(y)) = with_host(|h| (h.datetime_secs(a), h.datetime_secs(b))) {
        return x.total_cmp(&y);
    }
    // Two Symbols order lexicographically by name (`[:b, :a].sort == [:a, :b]`).
    if let (Some(x), Some(y)) = with_host(|h| (h.as_symbol(a), h.as_symbol(b))) {
        return x.cmp(&y);
    }
    // Two Arrays compare element-wise, shorter-is-less on a common prefix
    // (`Array#<=>`), so `[[:b,2],[:a,1]].sort` orders by the first element.
    if let (Some(xs), Some(ys)) = with_host(|h| (h.as_array(a), h.as_array(b))) {
        for (x, y) in xs.iter().zip(ys.iter()) {
            let o = cmp_values(x, y);
            if o != Ordering::Equal {
                return o;
            }
        }
        return xs.len().cmp(&ys.len());
    }
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Str(_), _) | (_, Value::Str(_)) | (Value::Obj(_), _) | (_, Value::Obj(_)) => {
            let (xs, ys) = with_host(|h| (h.as_str(a), h.as_str(b)));
            match (xs, ys) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ => as_f(a).partial_cmp(&as_f(b)).unwrap_or(Ordering::Equal),
            }
        }
        _ => as_f(a).partial_cmp(&as_f(b)).unwrap_or(Ordering::Equal),
    }
}

/// Apply an operator/method symbol between the accumulator and an element for
/// `inject(:sym)` / `reduce(init, :sym)`. Integer/Float arithmetic operators are
/// evaluated directly (the VM handles these opcodes, not per-type dispatch);
/// everything else is a normal method send (so string `:+`, `:concat`, … work).
fn reduce_sym(acc: &Value, op: &str, x: &Value) -> Result<Value, String> {
    let numeric = |v: &Value| {
        matches!(v, Value::Int(_) | Value::Float(_))
            || with_host(|h| h.as_bigint(v).is_some() || h.as_rational(v).is_some())
    };
    // Float arithmetic (either operand a Float; a Rational operand demotes to
    // Float, matching Ruby). Native float ops — num_op only covers heap numbers.
    if numeric(acc)
        && numeric(x)
        && (matches!(acc, Value::Float(_)) || matches!(x, Value::Float(_)))
    {
        let (a, b) = (as_f(acc), as_f(x));
        return Ok(match op {
            "+" => Value::Float(a + b),
            "-" => Value::Float(a - b),
            "*" => Value::Float(a * b),
            "/" => Value::Float(a / b),
            "%" => Value::Float(a - (a / b).floor() * b),
            "**" => Value::Float(a.powf(b)),
            _ => return dispatch(acc, op, std::slice::from_ref(x), None),
        });
    }
    // Native small-Int floor division/modulo — stay Integer-typed, floor toward
    // negative infinity (Ruby semantics).
    if let (Value::Int(a), Value::Int(b)) = (acc, x) {
        match op {
            "/" | "%" if *b == 0 => return Err(raise_exc("ZeroDivisionError", "divided by 0")),
            "/" => return Ok(Value::Int(floor_div(*a, *b))),
            "%" => return Ok(Value::Int(floor_mod(*a, *b))),
            _ => {}
        }
    }
    // Integer / BigInt / Rational +,-,*,/,% via the host numeric op, so results
    // promote to BigInt on overflow (reduce(:*) factorials), keep BigInt/Rational
    // accumulators exact, and never panic. `**` routes to Integer#** (bignum base,
    // negative-exponent Rational). The accumulator may already be a heap number
    // from a prior overflow, so this can't be gated to native Int only.
    if numeric(acc) && numeric(x) {
        let nop = match op {
            "+" => Some(fusevm::NumOp::Add),
            "-" => Some(fusevm::NumOp::Sub),
            "*" => Some(fusevm::NumOp::Mul),
            "/" => Some(fusevm::NumOp::Div),
            "%" => Some(fusevm::NumOp::Mod),
            _ => None,
        };
        if let Some(nop) = nop {
            return with_host(|h| h.num_op(nop, acc, x));
        }
        if op == "**" {
            return dispatch(acc, "**", std::slice::from_ref(x), None);
        }
    }
    dispatch(acc, op, std::slice::from_ref(x), None)
}

/// Insertion sort using a block comparator (returns -1/0/1). Kept simple; block
/// comparators run on modest arrays and correctness matters more than speed.
fn sort_with_block(mut arr: Vec<Value>, bl: &Value) -> Result<Vec<Value>, String> {
    for i in 1..arr.len() {
        let mut j = i;
        while j > 0 {
            let c = as_i(&call_proc(bl, &[arr[j - 1].clone(), arr[j].clone()])?);
            if c > 0 {
                arr.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
    Ok(arr)
}
