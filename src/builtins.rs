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
    call_method, call_proc, current_block, has_pending_signal, raise_signal_break,
    raise_signal_next, raise_signal_return, take_break, with_host,
};
use fusevm::{Value, VM};
use indexmap::IndexMap;

/// Register every rubyrs builtin on `vm`.
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
    vm.register_builtin(ops::YIELD, b_yield);
    vm.register_builtin(ops::TRUTHY, b_truthy);
    vm.register_builtin(ops::INDEX_GET, b_index_get);
    vm.register_builtin(ops::INDEX_SET, b_index_set);
    vm.register_builtin(ops::TOSTR, b_tostr);
    vm.register_builtin(ops::DEFINED, b_defined);
    vm.register_builtin(ops::SIG_BREAK, b_sig_break);
    vm.register_builtin(ops::SIG_NEXT, b_sig_next);
    vm.register_builtin(ops::SIG_RETURN, b_sig_return);
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
    with_host(|h| h.as_str(v).or_else(|| h.as_symbol(v)).unwrap_or_default())
}

// ---- variable builtins ----------------------------------------------------

fn b_getlocal(vm: &mut VM, _: u8) -> Value {
    let name = name_of(&vm.pop());
    let (defined, v) = with_host(|h| (h.local_defined(&name), h.get_local(&name)));
    if defined {
        return v;
    }
    // A bare name that is not a local is a zero-arg call to a user method.
    if with_host(|h| h.has_method(&name)) {
        return match call_method(&name, &[], None) {
            Ok(v) => propagate(vm, v),
            Err(e) => abort(vm, e),
        };
    }
    Value::Undef
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
    with_host(|h| h.get_const(&name))
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
    with_host(|h| {
        let mut s = String::new();
        for p in &parts {
            s.push_str(&h.to_s(p));
        }
        h.new_string(s)
    })
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
    if with_host(|h| h.has_method(name)) {
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
    // Universal methods available on every object.
    match name {
        "to_s" => {
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
            // Case-equality: a Range covers, a Class matches, else `==`.
            if let Some((lo, hi, excl)) = with_host(|h| h.as_range(recv)) {
                let n = as_i(&args[0]);
                let end = if excl { hi } else { hi + 1 };
                return Ok(Value::Bool(n >= lo && n < end));
            }
            return Ok(Value::Bool(with_host(|h| h.eq_values(recv, &args[0]))));
        }
        "is_a?" | "kind_of?" | "instance_of?" => {
            let cls = name_of(&args[0]);
            let actual = with_host(|h| h.class_of(recv).to_string());
            let numeric = actual == "Integer" || actual == "Float";
            return Ok(Value::Bool(actual == cls || (cls == "Numeric" && numeric)));
        }
        "freeze" | "itself" | "dup" | "clone" | "tap" if name != "tap" => return Ok(recv.clone()),
        "frozen?" => return Ok(Value::Bool(false)),
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

    let class = with_host(|h| h.class_of(recv).to_string());
    match class.as_str() {
        "Integer" | "Float" => dispatch_number(recv, name, args, block),
        "String" => dispatch_string(recv, name, args, block),
        "Array" => dispatch_array(recv, name, args, block),
        "Hash" => dispatch_hash(recv, name, args, block),
        "Range" => dispatch_range(recv, name, args, block),
        "Symbol" => dispatch_symbol(recv, name, args),
        "Proc" => dispatch_proc(recv, name, args),
        _ => Err(format!("undefined method '{name}' for {class}")),
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
            // Integer ** non-negative Integer stays an Integer, like Ruby.
            match (recv, &args[0]) {
                (Value::Int(base), Value::Int(exp)) if *exp >= 0 => {
                    Ok(Value::Int(base.pow(*exp as u32)))
                }
                _ => Ok(Value::Float(as_f(recv).powf(as_f(&args[0])))),
            }
        }
        "/" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err("divided by 0".into()),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(floor_div(*a, *b))),
            _ => Ok(Value::Float(as_f(recv) / as_f(&args[0]))),
        },
        "%" | "modulo" => match (recv, &args[0]) {
            (Value::Int(_), Value::Int(0)) => Err("divided by 0".into()),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(floor_mod(*a, *b))),
            _ => {
                let (x, y) = (as_f(recv), as_f(&args[0]));
                Ok(Value::Float(x - (x / y).floor() * y))
            }
        },
        "upto" => iter_int_range(recv, as_i(&args[0]), 1, &block, recv.clone()),
        "downto" => iter_int_range(recv, as_i(&args[0]), -1, &block, recv.clone()),
        "to_s" => Ok(with_host(|h| {
            let s = h.to_s(recv);
            h.new_string(s)
        })),
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
        "floor" => Ok(Value::Int(as_f(recv).floor() as i64)),
        "ceil" => Ok(Value::Int(as_f(recv).ceil() as i64)),
        "round" => Ok(match recv {
            Value::Float(f) => Value::Int(f.round() as i64),
            _ => recv.clone(),
        }),
        "chr" => Ok(with_host(|h| {
            let c = (as_i(recv) as u8) as char;
            h.new_string(c.to_string())
        })),
        "gcd" => Ok(Value::Int(gcd(as_i(recv), as_i(&args[0])))),
        _ => Err(format!(
            "undefined method '{name}' for {}",
            with_host(|h| h.class_of(recv))
        )),
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

fn gcd(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
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
        "chomp" => Ok(new_str(s.trim_end_matches(['\n', '\r']).to_string())),
        "chars" => Ok(new_arr(s.chars().map(|c| new_str(c.to_string())).collect())),
        "empty?" => Ok(Value::Bool(s.is_empty())),
        "to_i" => Ok(Value::Int(parse_leading_int(&s))),
        "to_f" => Ok(Value::Float(parse_leading_float(&s))),
        "to_s" | "to_str" => Ok(recv.clone()),
        "to_sym" => Ok(with_host(|h| h.new_symbol(&s))),
        "include?" => Ok(Value::Bool(s.contains(&arg_str(&args[0])))),
        "start_with?" => Ok(Value::Bool(s.starts_with(&arg_str(&args[0])))),
        "end_with?" => Ok(Value::Bool(s.ends_with(&arg_str(&args[0])))),
        "split" => {
            let parts: Vec<Value> = if args.is_empty() {
                s.split_whitespace()
                    .map(|p| new_str(p.to_string()))
                    .collect()
            } else {
                let sep = arg_str(&args[0]);
                s.split(&sep).map(|p| new_str(p.to_string())).collect()
            };
            Ok(new_arr(parts))
        }
        "sub" => {
            let (from, to) = (arg_str(&args[0]), arg_str(&args[1]));
            Ok(new_str(s.replacen(&from, &to, 1)))
        }
        "gsub" => {
            let (from, to) = (arg_str(&args[0]), arg_str(&args[1]));
            Ok(new_str(s.replace(&from, &to)))
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
        "*" => Ok(new_str(s.repeat(as_i(&args[0]).max(0) as usize))),
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
        _ => Err(format!("undefined method '{name}' for String")),
    }
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
        _ => Value::Undef,
    }
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
        "first" => Ok(arr.first().cloned().unwrap_or(Value::Undef)),
        "last" => Ok(arr.last().cloned().unwrap_or(Value::Undef)),
        "empty?" => Ok(Value::Bool(arr.is_empty())),
        "reverse" => Ok(new_arr(arr.into_iter().rev().collect())),
        "to_a" | "to_ary" | "dup" | "clone" => Ok(new_arr(arr)),
        "include?" => Ok(Value::Bool(
            arr.iter().any(|x| with_host(|h| h.eq_values(x, &args[0]))),
        )),
        "index" | "find_index" => {
            let pos = arr
                .iter()
                .position(|x| with_host(|h| h.eq_values(x, &args[0])));
            Ok(pos.map(|p| Value::Int(p as i64)).unwrap_or(Value::Undef))
        }
        "join" => {
            let sep = args.first().map(arg_str).unwrap_or_default();
            let parts: Vec<String> = arr.iter().map(|x| with_host(|h| h.to_s(x))).collect();
            Ok(new_str(parts.join(&sep)))
        }
        "sort" => {
            let mut a = arr;
            a.sort_by(cmp_values);
            Ok(new_arr(a))
        }
        "min" => Ok(arr
            .iter()
            .cloned()
            .min_by(cmp_values)
            .unwrap_or(Value::Undef)),
        "max" => Ok(arr
            .iter()
            .cloned()
            .max_by(cmp_values)
            .unwrap_or(Value::Undef)),
        "sum" => {
            let mut acc = args.first().cloned().unwrap_or(Value::Int(0));
            for x in &arr {
                acc = add_values(&acc, x);
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
            let mut out = Vec::new();
            flatten_into(&arr, &mut out);
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
        _ => Err(format!("undefined method '{name}' for Array")),
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

fn flatten_into(arr: &[Value], out: &mut Vec<Value>) {
    for x in arr {
        if let Some(inner) = with_host(|h| h.as_array(x)) {
            flatten_into(&inner, out);
        } else {
            out.push(x.clone());
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
        "size" | "length" | "count" => Ok(Value::Int(map.len() as i64)),
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
        "[]" | "fetch" => {
            let k = with_host(|h| h.value_to_key(&args[0]));
            Ok(map
                .get(&k)
                .cloned()
                .or_else(|| args.get(1).cloned())
                .unwrap_or(Value::Undef))
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
        "select" | "filter" => {
            let mut out = IndexMap::new();
            if let Some(b) = &block {
                for (k, v) in &map {
                    let kv = with_host(|h| h.key_value(k));
                    let r = call_proc(b, &[kv, v.clone()])?;
                    if with_host(|h| h.truthy(&r)) {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            Ok(with_host(|h| h.new_hash(out)))
        }
        _ => Err(format!("undefined method '{name}' for Hash")),
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
        "map" | "collect" | "select" | "filter" | "reject" | "reduce" | "inject" | "sum_by" => {
            let arr: Vec<Value> = (lo..end).map(Value::Int).collect();
            let tmp = with_host(|h| h.new_array(arr));
            dispatch_array(&tmp, name, args, block)
        }
        _ => Err(format!("undefined method '{name}' for Range")),
    }
}

// ---- Symbol / Proc --------------------------------------------------------

fn dispatch_symbol(recv: &Value, name: &str, _args: &[Value]) -> Result<Value, String> {
    let s = with_host(|h| h.as_symbol(recv).unwrap_or_default());
    match name {
        "to_s" | "id2name" | "name" => Ok(new_str(s)),
        "to_sym" => Ok(recv.clone()),
        "length" | "size" => Ok(Value::Int(s.chars().count() as i64)),
        "upcase" => Ok(with_host(|h| h.new_symbol(&s.to_uppercase()))),
        "downcase" => Ok(with_host(|h| h.new_symbol(&s.to_lowercase()))),
        _ => Err(format!("undefined method '{name}' for Symbol")),
    }
}

fn dispatch_proc(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "call" | "()" | "[]" | "yield" => call_proc(recv, args),
        _ => Err(format!("undefined method '{name}' for Proc")),
    }
}

// ===========================================================================
// Kernel functions (self / top-level calls with no user method).
// ===========================================================================

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
                let s = with_host(|h| h.to_s(a));
                print!("{s}");
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
            let msg = args
                .first()
                .map(|a| with_host(|h| h.to_s(a)))
                .unwrap_or_else(|| "RuntimeError".into());
            Err(msg)
        }
        "rand" => Ok(kernel_rand(args)),
        "srand" | "sleep" => Ok(Value::Int(0)),
        "Integer" => Ok(Value::Int(as_i(&args[0]))),
        "Float" => Ok(Value::Float(as_f(&args[0]))),
        "String" => Ok(with_host(|h| {
            let s = h.to_s(&args[0]);
            h.new_string(s)
        })),
        "Array" => Ok(match with_host(|h| h.as_array(&args[0])) {
            Some(a) => with_host(|h| h.new_array(a)),
            None if matches!(args[0], Value::Undef) => with_host(|h| h.new_array(vec![])),
            None => with_host(|h| h.new_array(vec![args[0].clone()])),
        }),
        "format" | "sprintf" => Ok(new_str(sprintf(args))),
        "gets" => Ok(read_line()),
        "lambda" | "proc" => block.ok_or_else(|| "tried to create Proc without a block".into()),
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
        let s = with_host(|h| h.to_s(v));
        println!("{s}");
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
fn sprintf(args: &[Value]) -> String {
    let fmt = arg_str(&args[0]);
    let mut out = String::new();
    let mut ai = 1;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('s') => {
                out.push_str(
                    &args
                        .get(ai)
                        .map(|a| with_host(|h| h.to_s(a)))
                        .unwrap_or_default(),
                );
                ai += 1;
            }
            Some('d') | Some('i') => {
                out.push_str(&args.get(ai).map(as_i).unwrap_or(0).to_string());
                ai += 1;
            }
            Some('f') => {
                out.push_str(&format!("{:.6}", args.get(ai).map(as_f).unwrap_or(0.0)));
                ai += 1;
            }
            Some('x') => {
                out.push_str(&format!("{:x}", args.get(ai).map(as_i).unwrap_or(0)));
                ai += 1;
            }
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

// ---- small value helpers --------------------------------------------------

fn new_str(s: String) -> Value {
    with_host(|h| h.new_string(s))
}
fn new_arr(items: Vec<Value>) -> Value {
    with_host(|h| h.new_array(items))
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
