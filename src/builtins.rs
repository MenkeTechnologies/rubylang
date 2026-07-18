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

/// Register every rubylang builtin on `vm`.
pub fn install(vm: &mut VM) {
    vm.register_builtin(ops::GETLOCAL, b_getlocal);
    vm.register_builtin(ops::SETLOCAL, b_setlocal);
    vm.register_builtin(ops::GETIVAR, b_getivar);
    vm.register_builtin(ops::SETIVAR, b_setivar);
    vm.register_builtin(ops::GETGVAR, b_getgvar);
    vm.register_builtin(ops::SETGVAR, b_setgvar);
    vm.register_builtin(ops::GETCONST, b_getconst);
    vm.register_builtin(ops::SETCONST, b_setconst);
    vm.register_builtin(ops::CALL, b_call);
    vm.register_builtin(ops::CALL_BLK, b_call_blk);
    vm.register_builtin(ops::CALL_METHOD, b_call_method);
    vm.register_builtin(ops::CALL_METHOD_BLK, b_call_method_blk);
    vm.register_builtin(ops::MKSTR, b_mkstr);
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
    vm.register_builtin(ops::GETSELF, b_getself);
    vm.register_builtin(ops::BEGIN, b_begin);
    vm.register_builtin(ops::SUPER, b_super);
    vm.register_builtin(ops::SUPER_FWD, b_super_fwd);
    vm.register_builtin(ops::MKARGS, b_mkargs);
    vm.register_builtin(ops::CALL_ARR, b_call_arr);
    vm.register_builtin(ops::CALL_METHOD_ARR, b_call_method_arr);
}

/// Concatenate `argc` arrays into one (splat argument/element building).
fn b_mkargs(vm: &mut VM, argc: u8) -> Value {
    let pieces = pop_n(vm, argc as usize);
    let mut out = Vec::new();
    for p in &pieces {
        match with_host(|h| h.as_array(p)) {
            Some(xs) => out.extend(xs),
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

/// Abort the running chunk with an error, halting the VM cleanly.
fn abort(vm: &mut VM, e: String) -> Value {
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
        _ => {}
    }
    // A bare name that is not a local is a zero-arg call: a method on `self` /
    // top-level method (errors propagate), else a Kernel function (`puts`, …).
    // An unknown bare name reads as nil, matching rubylang's lenient behaviour.
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
    let name = name_of(&vm.pop());
    let v = with_host(|h| h.get_const(&name));
    if !matches!(v, Value::Undef) {
        return v;
    }
    // An unassigned constant that names a class (user-defined, or a builtin
    // exception like `RuntimeError`) resolves to a class reference.
    if with_host(|h| h.class_exists(&name) || h.is_builtin_class(&name))
        || is_builtin_exception(&name)
    {
        return with_host(|h| h.class_ref(&name));
    }
    v
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
    with_host(|h| h.set_const(&name, val.clone()));
    val
}
fn b_defined(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    Value::Bool(with_host(|h| h.local_defined(&name)))
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
    let block = vals.pop();
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
    let block = vals.pop();
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
    match (&lo, &hi) {
        (Value::Int(a), Value::Int(b)) => with_host(|h| h.new_range(*a, *b, excl)),
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
    // Inside a class method `self` is a class ref — `new` and class methods
    // dispatch on the class; anything else (raise, puts, …) is a Kernel call.
    let this = with_host(|h| h.current_self());
    if let Some(cls) = with_host(|h| h.classref_name(&this)) {
        if name == "new" || with_host(|h| h.find_class_method(&cls, name)).is_some() {
            return dispatch_classref(&cls, name, args, block);
        }
        return kernel(name, args, block);
    }
    if with_host(|h| h.responds_to(name)) {
        return call_method(name, args, block);
    }
    kernel(name, args, block)
}

/// A receiver method call. Universal methods first, then per-class.
pub(crate) fn dispatch(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    // A user-defined method wins over the universal fallbacks (so a class can
    // override `to_s`, `==`, `inspect`, etc.).
    if let Some(cls) = with_host(|h| h.object_class(recv)) {
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
            return Ok(with_host(|h| {
                let s = h.class_of(recv).to_string();
                h.new_string(s)
            }))
        }
        "nil?" => return Ok(Value::Bool(matches!(recv, Value::Undef))),
        "==" => return Ok(Value::Bool(with_host(|h| h.eq_values(recv, &args[0])))),
        "!=" => return Ok(Value::Bool(!with_host(|h| h.eq_values(recv, &args[0])))),
        "===" => {
            // Case-equality: a Class matches instances, a Regexp matches a
            // string, a Range covers, else `==`.
            if let Some(cls) = with_host(|h| h.classref_name(recv)) {
                return Ok(Value::Bool(with_host(|h| h.is_a(&args[0], &cls))));
            }
            if let Some((re, _)) = with_host(|h| h.as_regex(recv)) {
                return Ok(Value::Bool(re.is_match(&arg_str(&args[0]))));
            }
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(recv)) {
                let n = as_i(&args[0]);
                let end = if excl { hi } else { hi + 1 };
                return Ok(Value::Bool(n >= lo && n < end));
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
        "frozen?" => return Ok(Value::Bool(with_host(|h| h.is_frozen(recv)))),
        "instance_variable_get" => {
            let raw = name_of(&args[0]);
            let key = raw.strip_prefix('@').unwrap_or(&raw);
            return Ok(with_host(|h| h.ivar_of(recv, key)));
        }
        "instance_variable_set" => {
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
        "send" | "__send__" | "public_send" => {
            let m = name_of(&args[0]);
            return dispatch(recv, &m, &args[1..], block);
        }
        // `obj.method(:name)` — capture a bound Method object.
        "method" => {
            let m = name_of(&args[0]);
            return Ok(with_host(|h| h.new_method(recv.clone(), &m)));
        }
        "respond_to?" => return Ok(Value::Bool(true)),
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
    match class.as_str() {
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
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for {class}"),
        )),
    }
}

/// Methods on a class reference: `new` (allocate + `initialize`), `name`.
fn dispatch_classref(
    cls: &str,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    match name {
        "new" => {
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
        _ => {
            // A class method: `def self.m` runs with self bound to the class ref.
            if let Some(def) = with_host(|h| h.find_class_method(cls, name)) {
                let recv = with_host(|h| h.class_ref(cls));
                return crate::host::call_class_method(recv, &def, name, cls, args, block);
            }
            Err(format!("undefined method '{name}' for {cls}:Class"))
        }
    }
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
    match name {
        "message" | "to_s" => {
            let m = with_host(|h| h.ivar_of(recv, "message"));
            if matches!(m, Value::Undef) {
                Ok(new_str(cls.to_string()))
            } else {
                Ok(m)
            }
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
fn as_f(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

/// Resolve the `(lo, hi)` bounds for a numeric `clamp`, accepting either the
/// two-argument form `clamp(lo, hi)` or the single-`Range` form `clamp(1..5)`.
/// An exclusive range is rejected, matching Ruby's `Comparable#clamp`.
fn clamp_bounds(args: &[Value]) -> Result<(Value, Value), String> {
    if args.len() == 1 {
        if let Some((lo, hi, exclusive)) = with_host(|h| h.as_range(&args[0])) {
            if exclusive {
                return Err(raise_exc(
                    "ArgumentError",
                    "cannot clamp with an exclusive range",
                ));
            }
            return Ok((Value::Int(lo), Value::Int(hi)));
        }
        return Err(raise_exc("TypeError", "wrong argument type"));
    }
    Ok((args[0].clone(), args[1].clone()))
}

// ---- Integer / Float ------------------------------------------------------

fn dispatch_number(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
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
                // No Enumerator type yet: return the yielded values as an array so
                // `n.times.to_a` / `.map` still work.
                Ok(new_arr((0..n.max(0)).map(Value::Int).collect()))
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
                    // TODO: negative exponent needs a modular inverse (gcd == 1);
                    // not yet implemented, fall through to the plain-pow path.
                }
            }
            // Integer ** non-negative Integer stays an Integer, like Ruby.
            match (recv, &args[0]) {
                (Value::Int(base), Value::Int(exp)) if *exp >= 0 => {
                    Ok(Value::Int(base.pow(*exp as u32)))
                }
                _ => Ok(Value::Float(as_f(recv).powf(as_f(&args[0])))),
            }
        }
        "/" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(floor_div(*a, *b))),
            _ => Ok(Value::Float(as_f(recv) / as_f(&args[0]))),
        },
        "%" | "modulo" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(floor_mod(*a, *b))),
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
                && args.get(1).map(|v| matches!(v, Value::Int(_))).unwrap_or(true);
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
                    // No Enumerator type yet: return the stepped values as an array so
                    // `n.step(lim, by).to_a` / `.map` still work.
                    let mut vals = Vec::new();
                    let mut i = as_i(recv);
                    while (by > 0 && i <= limit) || (by < 0 && i >= limit) {
                        vals.push(Value::Int(i));
                        i += by;
                    }
                    return Ok(new_arr(vals));
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
                    // Blockless: materialize the stepped floats as an array.
                    let mut vals = Vec::new();
                    let mut i = 0.0f64;
                    while i < n {
                        vals.push(Value::Float(clamp(i)));
                        i += 1.0;
                    }
                    return Ok(new_arr(vals));
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
        "to_i" | "to_int" | "floor" if name != "floor" => Ok(Value::Int(as_i(recv))),
        "to_f" => Ok(Value::Float(as_f(recv))),
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
        "floor" => Ok(round_like(recv, args.first().map(as_i), f64::floor)),
        "ceil" => Ok(round_like(recv, args.first().map(as_i), f64::ceil)),
        "round" => Ok(round_like(recv, args.first().map(as_i), f64::round)),
        "truncate" => Ok(round_like(recv, args.first().map(as_i), f64::trunc)),
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
            _ => Ok(Value::Int(as_f(recv).div_euclid(as_f(&args[0])).ceil() as i64)),
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
        "fdiv" => Ok(Value::Float(as_f(recv) / as_f(&args[0]))),
        "clamp" => {
            // Ruby returns the receiver when in range, otherwise the bound
            // itself (preserving its type, so `(-2.7).clamp(-1.0, 1.0)` is a Float).
            // Accepts `clamp(lo, hi)` or `clamp(range)`; an exclusive range is
            // rejected exactly like `Comparable#clamp`.
            // TODO: beginless/endless ranges (`clamp(3..)`) aren't representable
            // as a `Range` in rubylang yet, so single-sided clamps are unhandled.
            let (lo, hi) = clamp_bounds(args)?;
            let x = as_f(recv);
            if x < as_f(&lo) {
                Ok(lo)
            } else if x > as_f(&hi) {
                Ok(hi)
            } else {
                Ok(recv.clone())
            }
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
        "<<" => Ok(Value::Int(as_i(recv) << as_i(&args[0]))),
        ">>" => Ok(Value::Int(as_i(recv) >> as_i(&args[0]))),
        "&" => Ok(Value::Int(as_i(recv) & as_i(&args[0]))),
        "|" => Ok(Value::Int(as_i(recv) | as_i(&args[0]))),
        "^" => Ok(Value::Int(as_i(recv) ^ as_i(&args[0]))),
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
        _ => Err(format!(
            "undefined method '{name}' for {}",
            with_host(|h| h.class_of(recv))
        )),
    }
}

/// Shared impl for `Float#round/ceil/floor/truncate(ndigits)`.
///
/// Ruby returns a `Float` only when `ndigits > 0`; with no argument or a
/// non-positive count the result is an `Integer`. Integers are returned
/// unchanged unless `ndigits < 0` (round to a power of ten).
fn round_like(recv: &Value, ndigits: Option<i64>, op: fn(f64) -> f64) -> Value {
    match recv {
        Value::Float(f) => match ndigits {
            Some(d) if d > 0 => {
                let m = 10f64.powi(d as i32);
                Value::Float(op(f * m) / m)
            }
            Some(d) if d < 0 => {
                let m = 10f64.powi((-d) as i32);
                Value::Int((op(f / m) * m) as i64)
            }
            _ => Value::Int(op(*f) as i64),
        },
        Value::Int(n) => match ndigits {
            Some(d) if d < 0 => {
                let m = 10f64.powi((-d) as i32);
                Value::Int((op(*n as f64 / m) * m) as i64)
            }
            _ => Value::Int(*n),
        },
        _ => recv.clone(),
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
    // No Enumerator type yet: return the yielded values as an array so
    // `n.upto(m).to_a` / `n.downto(m).map` still work.
    let mut vals = Vec::new();
    let mut i = start;
    while (step > 0 && i <= bound) || (step < 0 && i >= bound) {
        vals.push(Value::Int(i));
        i += step;
    }
    Ok(new_arr(vals))
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

// ---- String ---------------------------------------------------------------

fn dispatch_string(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let s = with_host(|h| h.as_str(recv).unwrap_or_default());
    match name {
        "length" | "size" => Ok(Value::Int(s.chars().count() as i64)),
        "upcase" => Ok(new_str(s.to_uppercase())),
        "downcase" => Ok(new_str(s.to_lowercase())),
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
        "bytes" => Ok(new_arr(s.bytes().map(|b| Value::Int(b as i64)).collect())),
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
            None => Ok(new_arr(s.bytes().map(|b| Value::Int(b as i64)).collect())),
        },
        // Byte at index `i` (supports negatives); nil when out of range.
        "getbyte" => {
            let bytes = s.as_bytes();
            let raw = as_i(&args[0]);
            let idx = if raw < 0 { raw + bytes.len() as i64 } else { raw };
            if idx < 0 || idx >= bytes.len() as i64 {
                Ok(Value::Undef)
            } else {
                Ok(Value::Int(bytes[idx as usize] as i64))
            }
        }
        // `b` returns a copy as ASCII-8BIT in real Ruby; we have no separate
        // encoding, so it returns an equivalent String (byte content unchanged).
        "b" => Ok(new_str(s.clone())),
        // True when every byte is 7-bit ASCII.
        "ascii_only?" => Ok(Value::Bool(s.is_ascii())),
        // We only carry valid UTF-8 Strings, so this is always true.
        "valid_encoding?" => Ok(Value::Bool(true)),
        // No real multi-encoding: these are self-returning / string-identity
        // shims so `force_encoding`/`encode` chains don't break.
        "force_encoding" => Ok(recv.clone()),
        "encode" => Ok(new_str(s.clone())),
        "lines" => Ok(new_arr(split_lines(&s).into_iter().map(new_str).collect())),
        "each_line" => {
            // With a block, iterate the lines; without one, yield the lines
            // array so `.each_line.to_a` works without a real Enumerator.
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
                None => Ok(new_arr(split_lines(&s).into_iter().map(new_str).collect())),
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
            let (neg, from) = expand_tr_spec(&arg_str(&args[0]), true);
            let (_, to) = expand_tr_spec(&arg_str(&args[1]), false);
            let out: String = s.chars().filter_map(|c| tr_map(c, neg, &from, &to)).collect();
            Ok(new_str(out))
        }
        "delete" => {
            let m = char_matcher(args);
            Ok(new_str(s.chars().filter(|&c| !m(c)).collect()))
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
            Ok(Value::Int(scan_int(&s, base).map(|(v, _)| v).unwrap_or(0)))
        }
        "hex" => Ok(Value::Int(scan_int(&s, 16).map(|(v, _)| v).unwrap_or(0))),
        "oct" => {
            // Ruby `String#oct` defaults to base 8 but honours a
            // `0x`/`0b`/`0o`/`0d` prefix, which flips it to auto-detect.
            let base = if has_radix_prefix(&s) { 0 } else { 8 };
            Ok(Value::Int(scan_int(&s, base).map(|(v, _)| v).unwrap_or(0)))
        }
        "to_f" => Ok(Value::Float(parse_leading_float(&s))),
        "to_s" | "to_str" => Ok(recv.clone()),
        "to_sym" => Ok(with_host(|h| h.new_symbol(&s))),
        "include?" => Ok(Value::Bool(s.contains(&arg_str(&args[0])))),
        "start_with?" => Ok(Value::Bool(args.iter().any(|a| match str_regex(a) {
            // A Regexp prefix matches when it matches at the very start.
            Some(re) => re.find(&s).map(|m| m.start() == 0).unwrap_or(false),
            None => s.starts_with(&arg_str(a)),
        }))),
        "end_with?" => Ok(Value::Bool(args.iter().any(|a| s.ends_with(&arg_str(a))))),
        "match?" => {
            let m = str_regex(&args[0])
                .map(|re| re.is_match(&s))
                .unwrap_or(false);
            Ok(Value::Bool(m))
        }
        "=~" => match str_regex(&args[0]) {
            // `=~` sets `$~`/`$1`.. as a side effect, then yields the char offset.
            Some(re) => {
                match_data(&re, &s);
                Ok(re
                    .find(&s)
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
            Some(re) => Ok(scan_regex(&re, &s)),
            None => Ok(new_arr(vec![])),
        },
        "split" => {
            let parts: Vec<Value> = if args.is_empty() {
                s.split_whitespace()
                    .map(|p| new_str(p.to_string()))
                    .collect()
            } else if let Some(re) = str_regex(&args[0]) {
                re.split(&s).map(|p| new_str(p.to_string())).collect()
            } else {
                let sep = arg_str(&args[0]);
                s.split(&sep).map(|p| new_str(p.to_string())).collect()
            };
            Ok(new_arr(parts))
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
            as_i(&args[0]) as usize,
            pad_str(args),
            true,
        ))),
        "rjust" => Ok(new_str(pad(
            &s,
            as_i(&args[0]) as usize,
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
            }
            Ok(recv.clone())
        }
        "[]" => Ok(str_index(&s, args)),
        "slice" => Ok(str_index(&s, args)),
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
            let (neg, from) = expand_tr_spec(&arg_str(&args[0]), true);
            let (_, to) = expand_tr_spec(&arg_str(&args[1]), false);
            let mut out = String::new();
            let mut last_translated: Option<char> = None;
            for c in s.chars() {
                let matched = if neg { !from.contains(&c) } else { from.contains(&c) };
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
        _ => Err(format!("undefined method '{name}' for String")),
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
fn str_regex(v: &Value) -> Option<regex::Regex> {
    with_host(|h| h.as_regex(v)).map(|(re, _)| re)
}

/// Build a `MatchData` value for the first match of `re` in `s`, or `nil`, and
/// update the match globals (`$~`, `$&`, `` $` ``, `$'`, `$+`, `$1`..`$9`).
fn match_data(re: &regex::Regex, s: &str) -> Value {
    let caps = re.captures(s);
    set_match_globals(caps.as_ref().map(|c| (c, s)))
}

/// Set the Ruby match globals from a set of captures (or clear them to `nil` on a
/// failed match), and return the corresponding `MatchData` value (or `nil`).
/// Ruby names these `$~` (the MatchData), `$&` (whole match), `` $` ``/`$'`
/// (pre/post text), `$+` (last matched group), and `$1`..`$9` (numbered groups).
fn set_match_globals(m: Option<(&regex::Captures, &str)>) -> Value {
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
        let md = h.new_matchdata(groups.clone(), pre.clone(), post.clone());
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
    let (groups, pre, post) = match with_host(|h| h.as_matchdata(recv)) {
        Some(t) => t,
        None => return Err("not a MatchData".to_string()),
    };
    let strv = |o: &Option<String>| o.clone().map(new_str).unwrap_or(Value::Undef);
    match name {
        "[]" => {
            let i = as_i(&args[0]);
            Ok(groups.get(i as usize).map(strv).unwrap_or(Value::Undef))
        }
        "pre_match" => Ok(new_str(pre)),
        "post_match" => Ok(new_str(post)),
        "to_a" => Ok(new_arr(groups.iter().map(strv).collect())),
        "captures" => Ok(new_arr(groups.iter().skip(1).map(strv).collect())),
        "to_s" => Ok(strv(groups.first().unwrap_or(&None))),
        "size" | "length" => Ok(Value::Int(groups.len() as i64)),
        _ => Err(format!("undefined method '{name}' for MatchData")),
    }
}

/// `String#scan(re)`: every match. With capture groups, each element is an array
/// of the captured groups; otherwise each element is the whole matched string.
fn scan_regex(re: &regex::Regex, s: &str) -> Value {
    let ngroups = re.captures_len(); // includes the whole-match group 0
    if ngroups <= 1 {
        let out: Vec<Value> = re
            .find_iter(s)
            .map(|m| new_str(m.as_str().to_string()))
            .collect();
        new_arr(out)
    } else {
        let out: Vec<Value> = re
            .captures_iter(s)
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

/// `String#sub`/`gsub` with a Regexp: a replacement string (with `\1` group
/// refs) or a block that receives each match.
fn regex_replace(
    re: &regex::Regex,
    s: &str,
    rest: &[Value],
    block: &Option<Value>,
    all: bool,
) -> Result<Value, String> {
    let mut out = String::new();
    let mut last = 0;
    for (count, caps) in re.captures_iter(s).enumerate() {
        if !all && count >= 1 {
            break;
        }
        let m = caps.get(0).unwrap();
        out.push_str(&s[last..m.start()]);
        if let Some(bl) = block {
            // Expose `$~`/`$1`.. to the block for the current match.
            set_match_globals(Some((&caps, s)));
            let r = call_proc(bl, &[new_str(m.as_str().to_string())])?;
            out.push_str(&with_host(|h| h.to_s(&r)));
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
fn expand_backrefs(repl: &str, caps: &regex::Captures) -> String {
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
fn expand_tr_spec(spec: &str, allow_neg: bool) -> (bool, Vec<char>) {
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
            if start <= end {
                for ch in start..=end {
                    out.push(ch);
                }
            }
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    (negated, out)
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
            let start = norm_idx(*i, chars.len()).unwrap_or(chars.len());
            let end = (start + (*len).max(0) as usize).min(chars.len());
            if start > chars.len() {
                Value::Undef
            } else {
                new_str(chars[start..end].iter().collect())
            }
        }
        [rng] => {
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(rng)) {
                let start = norm_idx(lo, chars.len()).unwrap_or(chars.len());
                let mut e = norm_idx(hi, chars.len()).unwrap_or(chars.len());
                if !excl {
                    e += 1;
                }
                let e = e.min(chars.len());
                if start > chars.len() {
                    Value::Undef
                } else {
                    new_str(chars[start..e.max(start)].iter().collect())
                }
            } else {
                Value::Undef
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

fn dispatch_array(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let arr = with_host(|h| h.as_array(recv).unwrap_or_default());
    match name {
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
        "to_a" | "to_ary" | "dup" | "clone" => Ok(new_arr(arr)),
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
            let mut out: Vec<Value> = Vec::new();
            for x in arr {
                if !out.iter().any(|y| with_host(|h| h.eq_values(&x, y))) {
                    out.push(x);
                }
            }
            Ok(new_arr(out))
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
        "each" => {
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
            }
            Ok(recv.clone())
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
                // No Enumerator type yet: return `[elem, index]` pairs so the chain
                // `each_with_index.map { |x, i| … }` / `.to_a` still works.
                Ok(new_arr(
                    arr.iter()
                        .enumerate()
                        .map(|(i, x)| new_arr(vec![x.clone(), Value::Int(i as i64)]))
                        .collect(),
                ))
            }
        }
        "map" | "collect" | "flat_map" => {
            let mut out = Vec::with_capacity(arr.len());
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    if name == "flat_map" {
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
        "select" | "filter" | "reject" => {
            let keep_when = name != "reject";
            let mut out = Vec::new();
            if let Some(b) = &block {
                for x in &arr {
                    let r = call_proc(b, std::slice::from_ref(x))?;
                    let t = with_host(|h| h.truthy(&r));
                    if t == keep_when {
                        out.push(x.clone());
                    }
                }
            }
            Ok(new_arr(out))
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
                if let Some(pair) = with_host(|h| h.as_array(x)) {
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
                // Without a block MRI returns an Enumerator; no Enumerator type yet.
                return Ok(Value::Undef);
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
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for Array"),
        )),
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
        "max_by" => Ok(keyed
            .into_iter()
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
                let s = norm_idx(lo, arr.len()).unwrap_or(arr.len());
                let mut e = norm_idx(hi, arr.len()).unwrap_or(arr.len());
                if !excl {
                    e += 1;
                }
                let e = e.min(arr.len());
                if s > arr.len() {
                    Value::Undef
                } else {
                    new_arr(arr[s..e.max(s)].to_vec())
                }
            } else {
                Value::Undef
            }
        }
        _ => Value::Undef,
    }
}

// ---- Hash -----------------------------------------------------------------

fn dispatch_hash(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
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
            let mut m = map;
            if let Some(other) = with_host(|h| h.as_hash(&args[0])) {
                m.extend(other);
            }
            Ok(with_host(|h| h.new_hash(m)))
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
                    out.push(call_proc(b, &[kv, v.clone()])?);
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
            let mut out = IndexMap::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let nk = call_proc(b, std::slice::from_ref(&kv))?;
                    let nkey = with_host(|h| h.value_to_key(&nk));
                    out.insert(nkey, v.clone());
                }
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
        "min_by" | "max_by" | "sort_by" => {
            let rows: Vec<Value> = with_host(|h| {
                map.iter()
                    .map(|(k, v)| {
                        let kv = h.key_value(k);
                        h.new_array(vec![kv, v.clone()])
                    })
                    .collect()
            });
            let tmp = with_host(|h| h.new_array(rows));
            dispatch_array(&tmp, name, args, block)
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
        "to_h" => Ok(recv.clone()),
        "except" => {
            // Return a copy without the given keys (original order preserved).
            let drop: Vec<_> = args.iter().map(|a| with_host(|h| h.value_to_key(a))).collect();
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
        _ => Err(raise_exc(
            "NoMethodError",
            &format!("undefined method '{name}' for Hash"),
        )),
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
    let (lo, hi, excl) = with_host(|h| h.as_range(recv).unwrap());
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
        "include?" | "cover?" | "member?" | "===" => {
            let n = as_i(&args[0]);
            Ok(Value::Bool(n >= lo && n < end))
        }
        "step" => {
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
            dispatch_array(&tmp, name, args, block)
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
            dispatch_array(&tmp, name, args, block)
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
                .map(|re| re.is_match(&s))
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
        _ => Err(format!("undefined method '{name}' for Symbol")),
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
            _ => Err(format!("undefined method '{name}' for Proc")),
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
        _ => Err(format!("undefined method '{name}' for Proc")),
    }
}

/// Methods on a bound `Method` object (`obj.method(:m)`): `call`/`[]`/`()` route
/// back through dispatch on the captured receiver; `arity`, `name`, `to_proc`,
/// `receiver` expose its parts.
fn dispatch_method(
    recv: &Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let (mrecv, mname) = match with_host(|h| h.as_method(recv)) {
        Some(m) => m,
        None => return Err(format!("undefined method '{name}' for Method")),
    };
    match name {
        "call" | "()" | "[]" | "yield" | "===" => dispatch(&mrecv, &mname, args, block),
        "arity" => Ok(Value::Int(with_host(|h| h.method_arity(&mrecv, &mname)))),
        "name" => Ok(with_host(|h| h.new_symbol(&mname))),
        "receiver" => Ok(mrecv),
        // A `Method` is itself callable via `call_proc`, so `to_proc` is identity.
        "to_proc" => Ok(recv.clone()),
        _ => Err(format!("undefined method '{name}' for Method")),
    }
}

fn dispatch_regexp(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let (re, source) = with_host(|h| h.as_regex(recv)).unwrap();
    match name {
        "source" => Ok(new_str(source)),
        "match?" => {
            let s = arg_str(&args[0]);
            Ok(Value::Bool(re.is_match(&s)))
        }
        "=~" => {
            let s = arg_str(&args[0]);
            match_data(&re, &s); // sets `$~`/`$1`..
            Ok(re
                .find(&s)
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
        _ => Err(format!("undefined method '{name}' for Regexp")),
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
        "require" | "require_relative" | "load" => Ok(Value::Bool(true)),
        "raise" | "fail" => {
            // Forms: `raise` / `raise "msg"` / `raise SomeError` /
            // `raise SomeError, "msg"` / `raise instance`.
            let (class, message) = match args {
                [] => ("RuntimeError".to_string(), "RuntimeError".to_string()),
                [a] => {
                    if let Some(cls) = with_host(|h| h.classref_name(a)) {
                        (cls.clone(), cls)
                    } else if let Some(cls) = with_host(|h| h.object_class(a)) {
                        // Re-raising an existing exception instance.
                        let m = with_host(|h| h.to_s(a));
                        with_host(|h| h.set_pending_exc(a.clone()));
                        return Err(if m.is_empty() { cls } else { m });
                    } else {
                        let m = with_host(|h| h.to_s(a));
                        ("RuntimeError".to_string(), m)
                    }
                }
                [cls, msg, ..] => {
                    let clsname = with_host(|h| h.classref_name(cls))
                        .unwrap_or_else(|| with_host(|h| h.to_s(cls)));
                    (clsname, with_host(|h| h.to_s(msg)))
                }
            };
            let exc = with_host(|h| h.new_exception(&class, &message));
            with_host(|h| h.set_pending_exc(exc));
            Err(message)
        }
        "rand" => Ok(kernel_rand(args)),
        "srand" | "sleep" => Ok(Value::Int(0)),
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
                None => with_host(|h| h.new_array(vec![args[0].clone()])),
            },
        }),
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
        "gets" => Ok(read_line()),
        "proc" => block.ok_or_else(|| "tried to create Proc without a block".into()),
        "lambda" => {
            let b = block.ok_or_else(|| String::from("tried to create Proc without a block"))?;
            with_host(|h| h.set_proc_lambda(&b));
            Ok(b)
        }
        "loop" => {
            if let Some(b) = &block {
                loop {
                    call_proc(b, &[])?;
                    if has_pending_signal() {
                        take_break();
                        break;
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
        println!("{}", display(v));
    }
}

fn kernel_rand(args: &[Value]) -> Value {
    // Deterministic-free RNG is out of scope; use a cheap counter-free splitmix
    // seeded from the OS time so scripts that call rand get varied values.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    match args.first() {
        Some(Value::Int(n)) if *n > 0 => Value::Int((z % (*n as u64)) as i64),
        _ => Value::Float((z as f64 / u64::MAX as f64).abs()),
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
        let arg = match named_arg {
            Some(v) => v,
            None => next_arg(&mut ai),
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
fn raise_exc(class: &str, msg: &str) -> String {
    let exc = with_host(|h| h.new_exception(class, msg));
    with_host(|h| h.set_pending_exc(exc));
    msg.to_string()
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
        _ => Value::Float(as_f(a) + as_f(b)),
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
    let both_num = matches!(acc, Value::Int(_) | Value::Float(_))
        && matches!(x, Value::Int(_) | Value::Float(_));
    if both_num {
        let int_pair = match (acc, x) {
            (Value::Int(a), Value::Int(b)) => Some((*a, *b)),
            _ => None,
        };
        match op {
            "+" => return Ok(add_values(acc, x)),
            "-" => {
                return Ok(match int_pair {
                    Some((a, b)) => Value::Int(a - b),
                    None => Value::Float(as_f(acc) - as_f(x)),
                })
            }
            "*" => {
                return Ok(match int_pair {
                    Some((a, b)) => Value::Int(a * b),
                    None => Value::Float(as_f(acc) * as_f(x)),
                })
            }
            "/" => {
                return match int_pair {
                    Some((_, 0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
                    Some((a, b)) => Ok(Value::Int(floor_div(a, b))),
                    None => Ok(Value::Float(as_f(acc) / as_f(x))),
                }
            }
            "%" => {
                return match int_pair {
                    Some((_, 0)) => Err(raise_exc("ZeroDivisionError", "divided by 0")),
                    Some((a, b)) => Ok(Value::Int(floor_mod(a, b))),
                    None => {
                        let (a, b) = (as_f(acc), as_f(x));
                        Ok(Value::Float(a - (a / b).floor() * b))
                    }
                }
            }
            "**" => {
                return Ok(match int_pair {
                    Some((a, b)) if b >= 0 => Value::Int(a.pow(b as u32)),
                    _ => Value::Float(as_f(acc).powf(as_f(x))),
                })
            }
            _ => {}
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
