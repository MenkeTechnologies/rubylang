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
    raise_signal_break, raise_signal_next, raise_signal_return, take_break, with_host, RKey,
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
    match (lo, hi) {
        (Value::Int(a), Value::Int(b)) => with_host(|h| h.new_range(a, b, excl)),
        _ => abort(vm, "range bounds must be integers".into()),
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
fn dispatch(
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
        "freeze" | "itself" => return Ok(recv.clone()),
        // `dup`/`clone` make a fresh shallow copy so mutating the copy does not
        // leak back to the original (Ruby's shallow-copy semantics).
        "dup" | "clone" => return Ok(with_host(|h| h.dup_value(recv))),
        // Immediates (Integer/Float/true/false/nil) and Symbols are always frozen
        // in Ruby; freeze itself is a best-effort no-op so mutable reference types
        // report unfrozen.
        "frozen?" => {
            let frozen = match recv {
                Value::Obj(_) => with_host(|h| h.as_symbol(recv).is_some()),
                _ => true,
            };
            return Ok(Value::Bool(frozen));
        }
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
            let obj = with_host(|h| h.new_object(cls));
            if with_host(|h| h.find_method(cls, "initialize")).is_some() {
                call_instance_method(obj.clone(), cls, "initialize", args, block)?;
            }
            Ok(obj)
        }
        "name" | "to_s" | "inspect" => Ok(new_str(cls.to_string())),
        // `Class === obj` and `obj.is_a?(Class)` — case/when matching.
        "===" => Ok(Value::Bool(with_host(|h| h.is_a(&args[0], cls)))),
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
            }
            Ok(recv.clone())
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
                }
            }
            Ok(recv.clone())
        }
        "upto" => iter_int_range(recv, as_i(&args[0]), 1, &block, recv.clone()),
        "downto" => iter_int_range(recv, as_i(&args[0]), -1, &block, recv.clone()),
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
        "abs" => Ok(match recv {
            Value::Int(n) => Value::Int(n.abs()),
            Value::Float(f) => Value::Float(f.abs()),
            _ => recv.clone(),
        }),
        "even?" => Ok(Value::Bool(as_i(recv) % 2 == 0)),
        "odd?" => Ok(Value::Bool(as_i(recv) % 2 != 0)),
        "zero?" => Ok(Value::Bool(as_f(recv) == 0.0)),
        "positive?" => Ok(Value::Bool(as_f(recv) > 0.0)),
        "negative?" => Ok(Value::Bool(as_f(recv) < 0.0)),
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
            let x = as_f(recv);
            if x < as_f(&args[0]) {
                Ok(args[0].clone())
            } else if x > as_f(&args[1]) {
                Ok(args[1].clone())
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
    ret: Value,
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
    }
    Ok(ret)
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
        "lines" => Ok(new_arr(split_lines(&s).into_iter().map(new_str).collect())),
        "each_line" => {
            if let Some(bl) = &block {
                for line in split_lines(&s) {
                    call_proc(bl, &[new_str(line)])?;
                    if has_pending_signal() {
                        take_break();
                        break;
                    }
                }
            }
            Ok(recv.clone())
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
            let from: Vec<char> = arg_str(&args[0]).chars().collect();
            let to: Vec<char> = arg_str(&args[1]).chars().collect();
            let out: String = s
                .chars()
                .map(|c| match from.iter().position(|&f| f == c) {
                    Some(i) => *to.get(i).or_else(|| to.last()).unwrap_or(&c),
                    None => c,
                })
                .collect();
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
        "to_i" => match args.first().map(as_i) {
            Some(base) if (2..=36).contains(&base) => Ok(Value::Int(
                i64::from_str_radix(s.trim(), base as u32).unwrap_or(0),
            )),
            _ => Ok(Value::Int(parse_leading_int(&s))),
        },
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
            Some(re) => Ok(re
                .find(&s)
                .map(|m| Value::Int(s[..m.start()].chars().count() as i64))
                .unwrap_or(Value::Undef)),
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
        "concat" | "<<" | "+" => {
            let other = with_host(|h| h.to_s(&args[0]));
            if name == "+" {
                Ok(new_str(format!("{s}{other}")))
            } else {
                with_host(|h| h.set_str(recv, format!("{s}{other}")));
                Ok(recv.clone())
            }
        }
        "<=>" => match with_host(|h| h.as_str(&args[0])) {
            Some(other) => Ok(Value::Int(match s.cmp(&other) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })),
            None => Ok(Value::Undef),
        },
        "*" => Ok(new_str(s.repeat(as_i(&args[0]).max(0) as usize))),
        "%" => {
            // `"%d-%d" % [1, 2]` or `"%d" % 5`.
            let fargs =
                with_host(|h| h.as_array(&args[0])).unwrap_or_else(|| vec![args[0].clone()]);
            Ok(new_str(sprintf(&s, &fargs)))
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
        "chr" => Ok(new_str(s.chars().next().map(|c| c.to_string()).unwrap_or_default())),
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
                return Err(raise_exc(
                    "IndexError",
                    &format!("index {i} out of string"),
                ));
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
        "index" => Ok(str_find(&s, &arg_str(&args[0]), args.get(1).map(as_i), false)),
        "rindex" => Ok(str_find(&s, &arg_str(&args[0]), args.get(1).map(as_i), true)),
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

/// Build a `MatchData` value for the first match of `re` in `s`, or `nil`.
fn match_data(re: &regex::Regex, s: &str) -> Value {
    match re.captures(s) {
        Some(caps) => {
            let whole = caps.get(0).unwrap();
            let groups: Vec<Option<String>> = (0..caps.len())
                .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
                .collect();
            let pre = s[..whole.start()].to_string();
            let post = s[whole.end()..].to_string();
            with_host(|h| h.new_matchdata(groups, pre, post))
        }
        None => Value::Undef,
    }
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

/// Build a predicate matching a char against ALL selector specs (Ruby intersects
/// multiple args). With no args every char matches.
fn char_matcher(args: &[Value]) -> impl Fn(char) -> bool {
    let parsed: Vec<(bool, std::collections::HashSet<char>)> =
        args.iter().map(|v| parse_char_selector(&arg_str(v))).collect();
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
        "length" | "size" | "count" if !(name == "count" && !args.is_empty()) => {
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
        "join" => {
            let sep = args.first().map(arg_str).unwrap_or_default();
            let parts: Vec<String> = arr.iter().map(|x| with_host(|h| h.to_s(x))).collect();
            Ok(new_str(parts.join(&sep)))
        }
        "sort" => {
            let a = match &block {
                Some(bl) => sort_with_block(arr, bl)?,
                None => {
                    let mut a = arr;
                    a.sort_by(cmp_values);
                    a
                }
            };
            Ok(new_arr(a))
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
            }
            Ok(recv.clone())
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
            } else {
                Ok(Value::Int(arr.len() as i64))
            }
        }
        "reduce" | "inject" => {
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
        "min_by" | "max_by" | "sort_by" => sort_by_family(recv, name, &arr, &block),
        "[]" => Ok(arr_index(&arr, args)),
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
                let start = if start < 0 { (start + len).max(0) } else { start };
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
                let start = if start < 0 { (start + len).max(0) } else { start };
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
            Ok(map.get(&k).cloned().unwrap_or(Value::Undef))
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
    let (lo, hi, excl) = with_host(|h| h.as_range(recv).unwrap());
    let end = if excl { hi } else { hi + 1 };
    match name {
        "to_a" | "to_ary" | "entries" => Ok(new_arr((lo..end).map(Value::Int).collect())),
        "min" | "first" | "begin" => Ok(Value::Int(lo)),
        "max" | "last" | "end" if name != "end" => Ok(Value::Int(if excl { hi - 1 } else { hi })),
        "end" => Ok(Value::Int(hi)),
        "size" | "count" | "length" => Ok(Value::Int((end - lo).max(0))),
        "sum" => Ok(Value::Int((lo..end).sum())),
        "include?" | "cover?" | "member?" | "===" => {
            let n = as_i(&args[0]);
            Ok(Value::Bool(n >= lo && n < end))
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
        "start_with?" => Ok(Value::Bool(
            args.iter().any(|a| s.starts_with(&arg_str(a))),
        )),
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
            for a in args {
                print!("{}", display(a));
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
            None if matches!(args[0], Value::Undef) => with_host(|h| h.new_array(vec![])),
            None => with_host(|h| h.new_array(vec![args[0].clone()])),
        }),
        "format" | "sprintf" => Ok(new_str(sprintf(&arg_str(&args[0]), &args[1..]))),
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
        "block_given?" => Ok(Value::Bool(current_block().is_some())),
        "exit" | "exit!" | "abort" => {
            let code = args.first().map(as_i).unwrap_or(0);
            std::process::exit(code as i32);
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

/// Minimal `sprintf`/`format`: handles `%s %d %f %x %%` positionally.
/// `format`/`sprintf`/`String#%` — handles `%[flags][width][.precision]conv`
/// with flags `-`, `0`, `+`, ` `, and conversions d/i/f/e/g/s/x/X/o/b/c/%.
fn sprintf(fmt: &str, args: &[Value]) -> String {
    let bytes: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut ai = 0;
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
        // flags
        let (mut left, mut zero, mut plus, mut space) = (false, false, false, false);
        while i < bytes.len() {
            match bytes[i] {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                ' ' => space = true,
                _ => break,
            }
            i += 1;
        }
        // width
        let mut width = 0usize;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            width = width * 10 + (bytes[i] as usize - '0' as usize);
            i += 1;
        }
        // precision
        let mut prec: Option<usize> = None;
        if i < bytes.len() && bytes[i] == '.' {
            i += 1;
            let mut p = 0usize;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                p = p * 10 + (bytes[i] as usize - '0' as usize);
                i += 1;
            }
            prec = Some(p);
        }
        let conv = if i < bytes.len() { bytes[i] } else { '%' };
        i += 1;
        let arg = args.get(ai).cloned().unwrap_or(Value::Undef);
        ai += 1;

        // Render the value (body), then apply sign, zero-fill, and width.
        let (mut body, numeric, negative) = match conv {
            'd' | 'i' | 'u' => {
                let n = as_i(&arg);
                (n.unsigned_abs().to_string(), true, n < 0)
            }
            'f' => {
                let f = as_f(&arg);
                (format!("{:.*}", prec.unwrap_or(6), f.abs()), true, f < 0.0)
            }
            'e' => {
                let f = as_f(&arg);
                (format!("{:.*e}", prec.unwrap_or(6), f.abs()), true, f < 0.0)
            }
            'g' => {
                let f = as_f(&arg);
                (format!("{}", f.abs()), true, f < 0.0)
            }
            'x' => (format!("{:x}", as_i(&arg)), false, false),
            'X' => (format!("{:X}", as_i(&arg)), false, false),
            'o' => (format!("{:o}", as_i(&arg)), false, false),
            'b' => (format!("{:b}", as_i(&arg)), false, false),
            'c' => (
                char::from_u32(as_i(&arg) as u32)
                    .map(String::from)
                    .unwrap_or_default(),
                false,
                false,
            ),
            's' => {
                let mut s = with_host(|h| h.to_s(&arg));
                if let Some(p) = prec {
                    s.truncate(p);
                }
                (s, false, false)
            }
            other => {
                out.push('%');
                out.push(other);
                ai -= 1;
                continue;
            }
        };

        let sign = if negative {
            "-"
        } else if numeric && plus {
            "+"
        } else if numeric && space {
            " "
        } else {
            ""
        };
        let total = sign.len() + body.len();
        if width > total {
            let pad = width - total;
            if left {
                out.push_str(sign);
                out.push_str(&body);
                out.extend(std::iter::repeat(' ').take(pad));
            } else if zero && numeric {
                out.push_str(sign);
                out.extend(std::iter::repeat('0').take(pad));
                out.push_str(&body);
            } else {
                out.extend(std::iter::repeat(' ').take(pad));
                out.push_str(sign);
                out.push_str(&body);
            }
        } else {
            out.push_str(sign);
            out.push_str(&body);
        }
        let _ = &mut body;
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
fn parse_leading_int(s: &str) -> i64 {
    let t = s.trim();
    let mut buf = String::new();
    for (i, c) in t.chars().enumerate() {
        if c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')) {
            buf.push(c);
        } else {
            break;
        }
    }
    buf.parse().unwrap_or(0)
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
